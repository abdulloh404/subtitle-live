//! เวิร์กเกอร์ whisper.cpp ภายในเครื่องและเหตุการณ์ STT สำหรับชั้นแอปพลิเคชัน

mod backend;

pub use backend::{ComputeBackend, compiled_compute_backend};

use std::{
    collections::VecDeque,
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::audio::{LatestQueue, MixedAudioChunk, SAMPLE_RATE_HZ};

// คิว debug แยกจากเหตุการณ์หลัก เพื่อไม่ให้การเปิดหน้าดูข้อมูลรบกวน subtitle
const DEBUG_QUEUE_CAPACITY: usize = 256;
// เวิร์กเกอร์พักสั้น ๆ ระหว่างรอบเพื่อรับคำสั่งได้ไวโดยไม่วนใช้ CPU เปล่า
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
// ปิด utterance เมื่อไม่พบเสียงพูดต่อเนื่อง เพื่อให้ Final ตรงกับปลายช่วงพูดแทนรอบ inference
const ENDPOINT_SILENCE_DURATION: Duration = Duration::from_millis(500);
// เก็บเสียงก่อนผ่าน threshold เล็กน้อยเพื่อไม่ตัดพยัญชนะนำหน้าที่มีพลังงานต่ำ
const SPEECH_PREROLL_DURATION: Duration = Duration::from_millis(160);
// ไม่ส่งเสียงกระตุกสั้นมากเข้า Whisper และรอให้มีเนื้อเสียงพอสำหรับ partial แรก
const MINIMUM_SPEECH_DURATION: Duration = Duration::from_millis(240);
// บังคับปิด utterance ยาวเพื่อจำกัดเวลาประมวลผลรอบ Final และขนาดหน่วยความจำ
const MAX_UTTERANCE_DURATION: Duration = Duration::from_secs(12);
// ระดับ RMS ต่ำกว่านี้ถือเป็นความเงียบและไม่ส่งเข้าโมเดล Whisper
const SILENCE_RMS_THRESHOLD: f32 = 0.001;
// เมื่อเปิดตัวกรองกิจกรรมเสียง จะใช้เกณฑ์ RMS สูงขึ้นเพื่อกันเสียงพลังงานต่ำก่อนถึง Whisper
const VAD_RMS_THRESHOLD: f32 = 0.003;
// หลังเริ่มพูดแล้วใช้เกณฑ์ต่ำลงเพื่อไม่ตัดพยัญชนะเบาหรือท้ายคำเร็วเกินไป
const VAD_CONTINUE_RMS_THRESHOLD: f32 = 0.0015;

/// ค่าที่เปลี่ยนระหว่างทำงานได้โดยไม่ต้องโหลดโมเดล Whisper ใหม่
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SttStreamingConfig {
    /// ช่วงเสียงใหม่ขั้นต่ำระหว่างผลชั่วคราวแต่ละรอบ
    pub step_ms: u32,
    /// ความยาวเสียงย้อนหลังสูงสุดที่ส่งเข้า Whisper
    pub window_ms: u32,
    /// ใช้เกณฑ์พลังงานเสียง RMS ที่เข้มกว่าตัวกรองความเงียบพื้นฐาน โดยไม่ได้แยกว่าเป็นเสียงพูดหรือไม่
    pub vad_enabled: bool,
}

/// ค่าคงที่ที่ใช้สร้างเซสชันอนุมาน Whisper ภายในเครื่องหนึ่งเซสชัน
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SttStartConfig {
    /// ตำแหน่งไฟล์โมเดล whisper.cpp บนเครื่อง
    pub model_path: PathBuf,
    /// รหัสภาษาที่ส่งให้ Whisper โดยระยะแรกรองรับเฉพาะภาษาอังกฤษ
    pub language: String,
    /// ช่วงเสียงใหม่ขั้นต่ำระหว่างผลชั่วคราวแต่ละรอบ
    pub step_ms: u32,
    /// ความยาวเสียงย้อนหลังสูงสุดที่ส่งเข้า Whisper
    pub window_ms: u32,
    /// ใช้เกณฑ์พลังงานเสียง RMS ที่เข้มกว่าตัวกรองความเงียบพื้นฐาน โดยไม่ได้แยกว่าเป็นเสียงพูดหรือไม่
    pub vad_enabled: bool,
}

/// สมมติฐานของช่วงปัจจุบัน หรือช่วงที่นิ่งแล้วและจะไม่ถูกแก้อีก
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptUpdate {
    /// สมมติฐานที่ยังแก้ไขได้เมื่อเสียงใหม่เข้ามา
    Partial { segment_id: u64, text: String },
    /// ข้อความที่นิ่งแล้วและใช้ปิดช่วงปัจจุบัน
    Final { segment_id: u64, text: String },
}

/// ชนิดผลลัพธ์ของสมมติฐาน Whisper หนึ่งรอบ
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SttDebugRecordKind {
    /// สมมติฐานยังเปลี่ยนได้เมื่อมีเสียงใหม่
    Partial,
    /// สมมติฐานนิ่งและปิด segment แล้ว
    Final,
    /// Whisper คืนข้อความที่ถูกกรองและไม่ส่งไปยัง subtitle
    Ignored,
}

impl SttDebugRecordKind {
    /// คืนชื่อคงที่สำหรับแสดงผลและเขียน JSONL
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Partial => "partial",
            Self::Final => "final",
            Self::Ignored => "ignored",
        }
    }
}

/// ข้อมูลจาก Whisper หนึ่ง inference ซึ่งเป็นสมมติฐานทั้งท่อน ไม่ใช่ผลทีละคำ
#[derive(Clone, Debug, PartialEq)]
pub struct SttDebugRecord {
    /// เวลา wall-clock หลัง inference จบในหน่วย Unix milliseconds
    pub timestamp_unix_ms: u64,
    /// รุ่นเสียงที่ใช้กันผลลัพธ์จาก source เก่า
    pub audio_generation: u64,
    /// รหัสช่วงคำพูดที่ Whisper กำลังแก้ไขหรือยืนยัน
    pub segment_id: u64,
    /// สถานะ partial, final หรือ ignored
    pub kind: SttDebugRecordKind,
    /// ข้อความรวมทุก segment ที่ Whisper คืนจาก inference รอบนี้
    pub raw_text: String,
    /// ข้อความหลังลบ marker และจัดช่องว่าง
    pub processed_text: String,
    /// ข้อความที่ส่งออกเป็น transcript จริง หรือว่างเมื่อถูกละทิ้ง
    pub emitted_text: String,
    /// เวลารวมการเรียกโมเดลและจัดผลลัพธ์รอบนี้
    pub whisper_model_duration: Duration,
    /// เวลา wall-clock ที่ครอบ `WhisperState::full` ทั้งหมด ไม่ใช่เวลา GPU kernel โดยตรง
    pub backend_full_duration: Duration,
    /// backend ของ whisper.cpp ถูกตั้งค่าให้ร้องขอ GPU หรือไม่ โดยไม่ได้ยืนยันว่า GPU ทำงานจริง
    pub gpu_backend_requested: bool,
    /// หมายเลขอุปกรณ์ GPU ที่ส่งให้ whisper.cpp
    pub gpu_device: i32,
    /// ระยะเสียงใหม่ขั้นต่ำของรอบนี้
    pub step_ms: u32,
    /// ความยาวบริบทเสียงของรอบนี้
    pub window_ms: u32,
}

impl TranscriptUpdate {
    /// คืนรหัสช่วงที่เพิ่มขึ้นต่อเนื่องโดยไม่เปิดเผยข้อความถอดเสียง
    pub const fn segment_id(&self) -> u64 {
        match self {
            Self::Partial { segment_id, .. } | Self::Final { segment_id, .. } => *segment_id,
        }
    }

    /// คืนข้อความภายในเพื่อส่งต่อให้ตัวรวมคำบรรยายและ debug ตามที่ผู้ใช้ร้องขอ
    pub fn text(&self) -> &str {
        match self {
            Self::Partial { text, .. } | Self::Final { text, .. } => text,
        }
    }
}

/// ข้อความจากเวิร์กเกอร์ STT ที่ตัวทำงานหลักของแอปเป็นผู้รับไปประมวลผล
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SttEvent {
    /// เริ่มตรวจสอบและโหลดโมเดลแล้ว
    Loading,
    /// โมเดลและสถานะอนุมานที่ใช้ซ้ำพร้อมทำงานแล้ว
    Ready,
    /// ข้อความชั่วคราวหรือยืนยันแล้วที่สร้างจากรุ่นเสียงปัจจุบัน
    Transcript {
        /// รุ่นเสียงที่ใช้สร้างข้อความนี้
        audio_generation: u64,
        /// เวลาเริ่มของเสียงใหม่ชุดที่ทำให้เกิดสมมติฐานนี้
        audio_origin_at: Instant,
        /// ผลการถอดเสียงพร้อมรหัสช่วง
        update: TranscriptUpdate,
    },
    /// เวลาในแต่ละรอบอนุมานสำหรับแผงประสิทธิภาพ
    Metrics {
        /// รุ่นเสียงที่ตรงกับรอบอนุมานนี้
        audio_generation: u64,
        /// เวลาจากต้น audio step จนเริ่มเรียก Whisper
        audio_buffer_duration: Duration,
        /// เวลาที่ใช้เฉพาะใน Whisper
        inference_duration: Duration,
    },
    /// เวิร์กเกอร์ไม่มีเซสชัน Whisper ที่กำลังทำงาน
    Stopped,
    /// ข้อผิดพลาดของโมเดลหรือการอนุมานที่ต้องแจ้งผู้ใช้
    Error(String),
}

/// ตัวควบคุมแบบไม่บล็อกสำหรับเธรด Whisper โดยเฉพาะ
pub struct SttService {
    /// ช่องส่งคำสั่งควบคุมไปยังเวิร์กเกอร์
    commands: Sender<WorkerCommand>,
    /// ช่องรับเหตุการณ์ตามลำดับโดยไม่ทิ้ง lifecycle, transcript หรือ metrics
    events: Receiver<SttEvent>,
    /// คิวข้อมูล debug ที่เปิดใช้ตามคำขอและไม่ปะปนกับเหตุการณ์ subtitle
    debug_records: LatestQueue<SttDebugRecord>,
    /// รุ่นเสียงที่เวิร์กเกอร์ยอมรับและเผยแพร่แล้ว
    audio_generation: Arc<AtomicU64>,
    /// สิทธิ์ join เธรด Whisper ซึ่งอยู่กับ service หลักเพียงตัวเดียว
    worker: Option<thread::JoinHandle<()>>,
}

impl SttService {
    /// ขอโหลดโมเดลโดยไม่บล็อกผู้เรียกระหว่างอ่านไฟล์หรือเริ่ม Whisper
    pub fn start(&self, config: SttStartConfig) {
        let _ = self.commands.send(WorkerCommand::Start(config));
    }

    /// ขอหยุดเซสชัน STT และล้างเสียงที่ยังค้างในคิว
    pub fn stop(&self) {
        let _ = self.commands.send(WorkerCommand::Stop);
    }

    /// เปลี่ยนจังหวะ streaming และตัวกรองกิจกรรมเสียงโดยใช้โมเดลเดิมที่โหลดอยู่
    pub fn update_streaming(&self, config: SttStreamingConfig) {
        let _ = self.commands.send(WorkerCommand::UpdateStreaming(config));
    }

    /// เปิดหรือปิดการสร้างข้อมูล debug ภายใน worker โดยการปิดจะล้างข้อมูลค้างทันที
    pub fn set_debug_enabled(&self, enabled: bool) {
        if !enabled {
            self.debug_records.clear_reliable();
        }
        let _ = self.commands.send(WorkerCommand::SetDebugEnabled(enabled));
    }

    /// หยุดรับเสียงชั่วคราวโดยเก็บโมเดลที่โหลดแล้วไว้
    pub fn pause_audio(&self) {
        let _ = self.commands.send(WorkerCommand::PauseAudio);
    }

    /// เริ่มรุ่นเสียงใหม่เพื่อให้ชั้นแอปปฏิเสธเหตุการณ์เก่าจากแหล่งก่อนหน้าได้
    pub fn resume_audio(&self, generation: u64) -> u64 {
        let _ = self.commands.send(WorkerCommand::ResumeAudio(generation));
        generation
    }

    /// คืนรุ่นเสียงที่เวิร์กเกอร์เริ่มใช้งานจริงล่าสุด
    pub fn audio_generation(&self) -> u64 {
        self.audio_generation.load(Ordering::Acquire)
    }

    /// ขอให้เธรดเวิร์กเกอร์จบการทำงาน
    pub fn shutdown(&self) {
        let _ = self.commands.send(WorkerCommand::Shutdown);
    }

    /// ขอปิดเวิร์กเกอร์และรอให้การอนุมานรอบปัจจุบันจบก่อนคืนทรัพยากร
    pub fn finish(&mut self) -> Result<(), String> {
        self.shutdown();
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| "Whisper worker panicked while shutting down".to_owned())
    }

    /// ดึงเหตุการณ์ STT ที่รออยู่ทั้งหมดเพื่อให้ตัวทำงานหลักประมวลผลเป็นชุด
    pub fn drain_events(&self) -> Vec<SttEvent> {
        self.events.try_iter().collect()
    }

    /// ดึงข้อมูล debug ที่รออยู่ทั้งหมดโดยไม่แตะคิวเหตุการณ์ STT หลัก
    pub fn drain_debug_records(&self) -> Vec<SttDebugRecord> {
        self.debug_records.drain()
    }

    /// คืนจำนวน debug record สะสมที่คิว STT ทิ้งในโปรเซสปัจจุบัน
    pub fn debug_records_dropped(&self) -> u64 {
        self.debug_records.dropped()
    }
}

impl Drop for SttService {
    /// แจ้งปิดเวิร์กเกอร์เมื่อตัวควบคุมถูกทำลายเป็นแนวป้องกันชั้นสุดท้าย
    fn drop(&mut self) {
        let _ = self.commands.send(WorkerCommand::Shutdown);
    }
}

/// สร้างเวิร์กเกอร์ STT หนึ่งตัวเพื่อแยกการโหลดโมเดลและอนุมานออกจากเธรด GTK
pub fn spawn_service(input: LatestQueue<MixedAudioChunk>) -> SttService {
    let (commands, command_receiver) = mpsc::channel();
    let (worker_events, events) = mpsc::channel();
    let debug_records = LatestQueue::new(DEBUG_QUEUE_CAPACITY);
    let worker_debug_records = debug_records.clone();
    let audio_generation = Arc::new(AtomicU64::new(0));
    let worker_generation = Arc::clone(&audio_generation);
    let worker = thread::Builder::new()
        .name("whisper-stt".to_owned())
        .spawn(move || {
            run_worker(
                input,
                command_receiver,
                worker_events,
                worker_debug_records,
                worker_generation,
            )
        })
        .expect("failed to spawn STT worker");

    SttService {
        commands,
        events,
        debug_records,
        audio_generation,
        worker: Some(worker),
    }
}

/// คำสั่งควบคุมมีจำนวนน้อยและไม่จำกัดคิว ส่วนเสียงใช้คิวล่าสุดที่จำกัดขนาดแยกกัน
enum WorkerCommand {
    /// โหลดโมเดลใหม่และเริ่มเซสชัน
    Start(SttStartConfig),
    /// ปลดเซสชันโมเดลและหยุด STT
    Stop,
    /// ใช้ค่าการส่งเสียงชุดใหม่โดยไม่โหลดโมเดลซ้ำ
    UpdateStreaming(SttStreamingConfig),
    /// เปิดหรือปิดการเก็บ raw transcript สำหรับหน้าและไฟล์ debug
    SetDebugEnabled(bool),
    /// คงโมเดลไว้แต่หยุดรับเสียงชั่วคราว
    PauseAudio,
    /// รับเสียงต่อด้วยหมายเลขรุ่นใหม่
    ResumeAudio(u64),
    /// จบเธรดเวิร์กเกอร์อย่างถาวร
    Shutdown,
}

/// วงจรหลักของเวิร์กเกอร์ที่สลับระหว่างคำสั่งควบคุมและเสียงล่าสุด
fn run_worker(
    input: LatestQueue<MixedAudioChunk>,
    commands: Receiver<WorkerCommand>,
    events: Sender<SttEvent>,
    debug_records: LatestQueue<SttDebugRecord>,
    audio_generation: Arc<AtomicU64>,
) {
    let mut active: Option<ActiveStt> = None;
    let mut audio_paused = true;
    let mut current_audio_generation = 0;
    let mut debug_enabled = false;

    loop {
        match next_command(&commands, active.is_some()) {
            CommandState::Command(WorkerCommand::Start(config)) => {
                active = None;
                audio_paused = true;
                input.clear_reliable();
                let _ = events.send(SttEvent::Loading);
                match ActiveStt::load(config, input.dropped()) {
                    Ok(loaded) => {
                        active = Some(loaded);
                        let _ = events.send(SttEvent::Ready);
                    }
                    Err(error) => {
                        let _ = events.send(SttEvent::Error(error));
                        let _ = events.send(SttEvent::Stopped);
                    }
                }
            }
            CommandState::Command(WorkerCommand::Stop) => {
                active = None;
                audio_paused = true;
                input.clear_reliable();
                let _ = events.send(SttEvent::Stopped);
            }
            CommandState::Command(WorkerCommand::UpdateStreaming(config)) => {
                input.clear_reliable();
                if let Some(stt) = active.as_mut()
                    && let Err(error) = stt.update_streaming(config)
                {
                    let _ = events.send(SttEvent::Error(error));
                }
            }
            CommandState::Command(WorkerCommand::SetDebugEnabled(enabled)) => {
                debug_enabled = enabled;
                if !enabled {
                    debug_records.clear_reliable();
                }
            }
            CommandState::Command(WorkerCommand::PauseAudio) => {
                audio_paused = true;
                input.clear_reliable();
                if let Some(stt) = active.as_mut() {
                    stt.reset_discontinuity();
                }
            }
            CommandState::Command(WorkerCommand::ResumeAudio(generation)) => {
                if generation < current_audio_generation {
                    continue;
                }
                input.clear_reliable();
                if let Some(stt) = active.as_mut() {
                    stt.reset_discontinuity();
                    audio_paused = false;
                    current_audio_generation = generation;
                    audio_generation.store(generation, Ordering::Release);
                }
            }
            CommandState::Command(WorkerCommand::Shutdown) | CommandState::Disconnected => {
                input.clear_reliable();
                break;
            }
            CommandState::Idle => {}
        }

        let Some(stt) = active.as_mut() else {
            continue;
        };
        if audio_paused {
            continue;
        }

        match stt.consume_latest(&input, current_audio_generation, debug_enabled) {
            Ok(Some(result)) => {
                if let Some(record) = result.debug_record {
                    debug_records.push_latest(record);
                }
                if let Some(update) = result.update {
                    let _ = events.send(SttEvent::Transcript {
                        audio_generation: current_audio_generation,
                        audio_origin_at: result.audio_origin_at,
                        update,
                    });
                }
                let _ = events.send(SttEvent::Metrics {
                    audio_generation: current_audio_generation,
                    audio_buffer_duration: result.audio_buffer_duration,
                    inference_duration: result.inference_duration,
                });
            }
            Ok(None) => {}
            Err(error) => {
                active = None;
                input.clear_reliable();
                let _ = events.send(SttEvent::Error(error));
                let _ = events.send(SttEvent::Stopped);
            }
        }
    }
}

/// ผลการอ่านช่องคำสั่งในหนึ่งรอบของเวิร์กเกอร์
enum CommandState {
    /// ได้รับคำสั่งหนึ่งรายการ
    Command(WorkerCommand),
    /// ยังไม่มีคำสั่งและสามารถทำงานเสียงต่อได้
    Idle,
    /// ผู้ส่งคำสั่งทั้งหมดถูกปิดแล้ว
    Disconnected,
}

/// รับคำสั่งแบบบล็อกเมื่อยังไม่มีโมเดล และแบบไม่บล็อกระหว่างประมวลผลเสียง
fn next_command(commands: &Receiver<WorkerCommand>, active: bool) -> CommandState {
    let result = if active {
        commands.try_recv()
    } else {
        commands.recv().map_err(|_| TryRecvError::Disconnected)
    };

    match result {
        Ok(command) => CommandState::Command(command),
        Err(TryRecvError::Empty) => {
            thread::sleep(WORKER_POLL_INTERVAL);
            CommandState::Idle
        }
        Err(TryRecvError::Disconnected) => CommandState::Disconnected,
    }
}

/// สถานะ Whisper ที่ใช้ซ้ำระหว่างการอนุมานแต่ละรอบของเซสชันที่กำลังทำงาน
struct ActiveStt {
    /// สถานะภายในของ Whisper ที่เก็บโมเดลและบัฟเฟอร์คำนวณไว้ใช้ซ้ำ
    state: WhisperState,
    /// ค่าภาษาและขนาดหน้าต่างที่ใช้สร้างพารามิเตอร์การอนุมาน
    config: SttStartConfig,
    /// ระบุว่าพารามิเตอร์ context ร้องขอให้ใช้ GPU หรือไม่
    gpu_backend_requested: bool,
    /// หมายเลข GPU ที่ส่งให้ whisper.cpp
    gpu_device: i32,
    /// ตัวอย่างเสียงล่าสุดไม่เกิน `window_samples` สำหรับหน้าต่างแบบเลื่อน
    rolling_audio: VecDeque<f32>,
    /// จำนวนตัวอย่างสูงสุดที่ส่งเข้า Whisper ต่อหนึ่งรอบ
    window_samples: usize,
    /// จำนวนตัวอย่างใหม่ขั้นต่ำก่อนสร้างผลชั่วคราวครั้งถัดไป
    step_samples: usize,
    /// เสียงทั้ง utterance ปัจจุบันสำหรับอนุมาน Final หลังพบ endpoint
    utterance_audio: Vec<f32>,
    /// เสียงก่อนเริ่ม utterance เล็กน้อยเพื่อรักษาต้นคำที่อยู่ก่อน threshold
    pre_roll_audio: VecDeque<f32>,
    /// frame หลัง endpoint ที่รอเริ่ม segment ถัดไปโดยไม่ถูกรวมกับ Final ปัจจุบัน
    deferred_audio: VecDeque<MixedAudioChunk>,
    /// จำนวนตัวอย่างใหม่ที่สะสมตั้งแต่ผลชั่วคราวครั้งก่อน
    samples_since_partial: usize,
    /// จำนวนตัวอย่างที่ผ่านเกณฑ์กิจกรรมเสียงใน utterance ปัจจุบัน
    speech_samples: usize,
    /// จำนวนตัวอย่างเงียบต่อเนื่องท้าย utterance
    trailing_silence_samples: usize,
    /// เวลาเริ่มของเสียงใหม่ที่กำลังสะสมจนครบหนึ่ง partial interval
    pending_audio_started_at: Option<Instant>,
    /// เวลา frame ล่าสุดที่ผ่านเกณฑ์กิจกรรมเสียง ใช้ปิดช่วงแม้ mixer หยุดส่ง frame เงียบ
    last_speech_at: Option<Instant>,
    /// ตัวนับการทิ้งข้อมูลล่าสุดที่เห็น ใช้ตรวจจับช่วงเสียงขาดตอน
    observed_dropped: u64,
    /// รหัสช่วงคำพูดปัจจุบัน เพิ่มเมื่อยืนยันผลหรือเสียงขาดตอน
    segment_id: u64,
    /// ผลชั่วคราวล่าสุด ใช้ไม่ส่งข้อความเดิมซ้ำโดยไม่จำเป็น
    previous_partial: String,
    /// ระบุว่ากำลังสะสม utterance ซึ่งยังแก้ไขได้
    speech_active: bool,
}

/// ผลจากการอนุมานหนึ่งรอบพร้อมค่าหน่วงเวลาสำหรับหน้า Performance
struct InferenceResult {
    /// เหตุการณ์ข้อความถ้ามีคำพูดที่ใช้งานได้ในรอบนี้
    update: Option<TranscriptUpdate>,
    /// เวลาเริ่มของเสียงใหม่ชุดที่ทำให้เกิด inference รอบนี้
    audio_origin_at: Instant,
    /// เวลาตั้งแต่เริ่มรับเสียงใหม่จนเริ่ม inference รวมการสะสมหนึ่ง audio step
    audio_buffer_duration: Duration,
    /// เวลาที่ใช้เรียก Whisper โดยไม่รวมการรอเสียง
    inference_duration: Duration,
    /// ข้อมูลเต็มของสมมติฐานรอบนี้ ซึ่งสร้างเฉพาะเมื่อผู้ใช้เปิด debug
    debug_record: Option<SttDebugRecord>,
}

impl ActiveStt {
    /// ตรวจค่าและไฟล์โมเดล จากนั้นสร้างสถานะ Whisper และขนาดบัฟเฟอร์ตัวอย่าง
    fn load(config: SttStartConfig, observed_dropped: u64) -> Result<Self, String> {
        validate_config(&config)?;
        File::open(&config.model_path).map_err(|error| {
            format!(
                "Whisper model is not readable at {}: {error}",
                config.model_path.display()
            )
        })?;

        let context_parameters = WhisperContextParameters::default();
        let gpu_backend_requested = context_parameters.use_gpu;
        let gpu_device = context_parameters.gpu_device;
        let context = WhisperContext::new_with_params(&config.model_path, context_parameters)
            .map_err(|error| {
                format!(
                    "Failed to load Whisper model at {}: {error}",
                    config.model_path.display()
                )
            })?;
        let state = context
            .create_state()
            .map_err(|error| format!("Failed to initialize Whisper inference state: {error}"))?;
        let window_samples = milliseconds_to_samples(config.window_ms);
        let step_samples = milliseconds_to_samples(config.step_ms);

        Ok(Self {
            state,
            config,
            gpu_backend_requested,
            gpu_device,
            rolling_audio: VecDeque::with_capacity(window_samples),
            window_samples,
            step_samples,
            utterance_audio: Vec::with_capacity(duration_to_samples(MAX_UTTERANCE_DURATION)),
            pre_roll_audio: VecDeque::with_capacity(duration_to_samples(SPEECH_PREROLL_DURATION)),
            deferred_audio: VecDeque::new(),
            samples_since_partial: 0,
            speech_samples: 0,
            trailing_silence_samples: 0,
            pending_audio_started_at: None,
            last_speech_at: None,
            observed_dropped,
            segment_id: 1,
            previous_partial: String::new(),
            speech_active: false,
        })
    }

    /// ใช้ค่า streaming ใหม่และเริ่ม segment ใหม่เพื่อไม่ให้บริบทคนละค่าปะปนกัน
    fn update_streaming(&mut self, config: SttStreamingConfig) -> Result<(), String> {
        validate_streaming_config(config)?;
        self.config.step_ms = config.step_ms;
        self.config.window_ms = config.window_ms;
        self.config.vad_enabled = config.vad_enabled;
        self.step_samples = milliseconds_to_samples(config.step_ms);
        self.window_samples = milliseconds_to_samples(config.window_ms);
        self.reset_discontinuity();
        Ok(())
    }

    /// สะสม utterance, ส่ง Partial ระหว่างพูด และส่ง Final เมื่อพบช่วงเงียบที่เป็น endpoint
    fn consume_latest(
        &mut self,
        input: &LatestQueue<MixedAudioChunk>,
        current_audio_generation: u64,
        debug_enabled: bool,
    ) -> Result<Option<InferenceResult>, String> {
        let dropped = input.dropped();
        if dropped != self.observed_dropped {
            self.reset_discontinuity();
            self.observed_dropped = dropped;
        }

        let mut chunks = std::mem::take(&mut self.deferred_audio);
        let mut input_drain_succeeded = false;
        if chunks.is_empty() {
            let Some(input_chunks) = input.try_drain() else {
                return Ok(None);
            };
            input_drain_succeeded = true;
            chunks.extend(
                input_chunks
                    .into_iter()
                    .filter(|chunk| chunk.generation == current_audio_generation),
            );
        }
        let endpoint_silence_samples = duration_to_samples(ENDPOINT_SILENCE_DURATION);
        let max_utterance_samples = duration_to_samples(MAX_UTTERANCE_DURATION);
        let mut heard_speech = false;
        let mut processed_audio = false;
        let mut endpoint_reached = false;
        while let Some(chunk) = chunks.pop_front() {
            if self.speech_active
                && self.last_speech_at.is_some_and(|last_speech_at| {
                    chunk
                        .captured_at
                        .saturating_duration_since(last_speech_at)
                        >= ENDPOINT_SILENCE_DURATION
                })
            {
                endpoint_reached = true;
                self.deferred_audio.push_back(chunk);
                self.deferred_audio.append(&mut chunks);
                break;
            }
            processed_audio = true;

            let minimum_rms = if self.config.vad_enabled {
                if self.speech_active {
                    VAD_CONTINUE_RMS_THRESHOLD
                } else {
                    VAD_RMS_THRESHOLD
                }
            } else {
                SILENCE_RMS_THRESHOLD
            };
            let chunk_is_speech = has_audible_signal(&chunk.samples, minimum_rms);
            if !self.speech_active {
                self.pre_roll_audio.extend(chunk.samples.iter().copied());
                let pre_roll_samples = duration_to_samples(SPEECH_PREROLL_DURATION);
                while self.pre_roll_audio.len() > pre_roll_samples {
                    self.pre_roll_audio.pop_front();
                }
                if !chunk_is_speech {
                    continue;
                }

                self.speech_active = true;
                self.pending_audio_started_at
                    .get_or_insert(chunk.captured_at);
                let starting_audio: Vec<_> = self.pre_roll_audio.drain(..).collect();
                self.samples_since_partial = self
                    .samples_since_partial
                    .saturating_add(starting_audio.len());
                self.rolling_audio.extend(starting_audio.iter().copied());
                self.utterance_audio.extend_from_slice(&starting_audio);
                heard_speech = true;
                self.speech_samples = self.speech_samples.saturating_add(chunk.samples.len());
                self.last_speech_at = Some(chunk.captured_at);
                continue;
            }

            self.pending_audio_started_at
                .get_or_insert(chunk.captured_at);
            self.samples_since_partial = self
                .samples_since_partial
                .saturating_add(chunk.samples.len());
            self.rolling_audio.extend(chunk.samples.iter().copied());
            self.utterance_audio.extend_from_slice(&chunk.samples);
            if chunk_is_speech {
                heard_speech = true;
                self.speech_samples = self.speech_samples.saturating_add(chunk.samples.len());
                self.trailing_silence_samples = 0;
                self.last_speech_at = Some(chunk.captured_at);
            } else {
                self.trailing_silence_samples = self
                    .trailing_silence_samples
                    .saturating_add(chunk.samples.len());
            }
            if self.trailing_silence_samples >= endpoint_silence_samples
                || self.utterance_audio.len() >= max_utterance_samples
            {
                endpoint_reached = true;
                self.deferred_audio.append(&mut chunks);
                break;
            }
        }
        while self.rolling_audio.len() > self.window_samples {
            self.rolling_audio.pop_front();
        }

        if !self.speech_active {
            return Ok(None);
        }

        let silence_deadline_reached = endpoint_reached
            || self.trailing_silence_samples >= endpoint_silence_samples
            || (!processed_audio
                && input_drain_succeeded
                && self.deferred_audio.is_empty()
                && self.last_speech_at.is_some_and(|last_speech_at| {
                    Instant::now().saturating_duration_since(last_speech_at)
                        >= ENDPOINT_SILENCE_DURATION
                }));
        let forced_endpoint = self.utterance_audio.len() >= max_utterance_samples;
        let final_inference = silence_deadline_reached || forced_endpoint;
        let enough_speech =
            self.speech_samples >= duration_to_samples(MINIMUM_SPEECH_DURATION);

        if final_inference && !enough_speech {
            self.reset_audio();
            return Ok(None);
        }
        if !final_inference
            && (!heard_speech || !enough_speech || self.samples_since_partial < self.step_samples)
        {
            return Ok(None);
        }

        let audio: Vec<f32> = if final_inference {
            self.utterance_audio.clone()
        } else {
            self.rolling_audio.iter().copied().collect()
        };
        if audio.is_empty() {
            return Ok(None);
        }

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.config.language));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_single_segment(!final_inference);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        let inference_segment_id = self.segment_id;
        let whisper_model_started = Instant::now();
        let audio_origin_at = self
            .pending_audio_started_at
            .take()
            .or(self.last_speech_at)
            .unwrap_or(whisper_model_started);
        let audio_buffer_duration =
            whisper_model_started.saturating_duration_since(audio_origin_at);
        let backend_full_started = Instant::now();
        self.state
            .full(params, &audio)
            .map_err(|error| format!("Whisper inference failed: {error}"))?;
        let backend_full_duration = backend_full_started.elapsed();
        let raw_text = collect_hypothesis(&self.state)?;
        let normalized_text = normalize_hypothesis_text(&raw_text);
        let processed_text = if final_inference {
            normalized_text
        } else {
            normalize_partial_hypothesis_text(&normalized_text)
        };
        let update = self.make_update(processed_text.clone(), final_inference);
        let whisper_model_duration = whisper_model_started.elapsed();
        let debug_record = debug_enabled.then(|| {
            let (kind, emitted_text) = match update.as_ref() {
                Some(TranscriptUpdate::Partial { text, .. }) => {
                    (SttDebugRecordKind::Partial, text.clone())
                }
                Some(TranscriptUpdate::Final { text, .. }) => {
                    (SttDebugRecordKind::Final, text.clone())
                }
                None => (SttDebugRecordKind::Ignored, String::new()),
            };
            SttDebugRecord {
                timestamp_unix_ms: unix_timestamp_ms(),
                audio_generation: current_audio_generation,
                segment_id: inference_segment_id,
                kind,
                raw_text,
                processed_text,
                emitted_text,
                whisper_model_duration,
                backend_full_duration,
                gpu_backend_requested: self.gpu_backend_requested,
                gpu_device: self.gpu_device,
                step_ms: self.config.step_ms,
                window_ms: self.config.window_ms,
            }
        });

        if final_inference {
            self.segment_id = self.segment_id.saturating_add(1);
            self.reset_audio();
        } else {
            self.samples_since_partial = 0;
        }

        Ok(Some(InferenceResult {
            update,
            audio_origin_at,
            audio_buffer_duration,
            inference_duration: backend_full_duration,
            debug_record,
        }))
    }

    /// สร้างผลตาม endpoint และไม่ใช้ข้อความซ้ำสองรอบเป็นหลักฐานว่าประโยคจบแล้ว
    fn make_update(
        &mut self,
        hypothesis: String,
        final_inference: bool,
    ) -> Option<TranscriptUpdate> {
        let hypothesis = hypothesis.trim().to_owned();
        if final_inference {
            let text = final_hypothesis_or_previous(hypothesis, &self.previous_partial)?;
            return Some(TranscriptUpdate::Final {
                segment_id: self.segment_id,
                text,
            });
        }
        if hypothesis.is_empty() || hypothesis == self.previous_partial {
            None
        } else {
            self.previous_partial.clone_from(&hypothesis);
            Some(TranscriptUpdate::Partial {
                segment_id: self.segment_id,
                text: hypothesis,
            })
        }
    }

    /// ล้างเฉพาะบัฟเฟอร์เสียง แต่คงรหัสช่วงและโมเดลที่โหลดไว้
    fn reset_audio(&mut self) {
        self.rolling_audio.clear();
        self.utterance_audio.clear();
        self.pre_roll_audio.clear();
        self.samples_since_partial = 0;
        self.speech_samples = 0;
        self.trailing_silence_samples = 0;
        self.pending_audio_started_at = None;
        self.last_speech_at = None;
        self.previous_partial.clear();
        self.speech_active = false;
    }

    /// ล้างทั้งเสียงและสมมติฐานเมื่อแหล่งเสียงหยุด เปลี่ยน หรือมีข้อมูลตกหล่น
    fn reset_discontinuity(&mut self) {
        self.reset_audio();
        self.deferred_audio.clear();
        self.segment_id = self.segment_id.saturating_add(1);
    }
}

/// ใช้ partial ล่าสุดเมื่อ Whisper คืน Final ว่าง เพื่อไม่ลบข้อความที่เคยแสดงแล้ว
fn final_hypothesis_or_previous(hypothesis: String, previous_partial: &str) -> Option<String> {
    if hypothesis.is_empty() {
        let previous_partial = previous_partial.trim();
        (!previous_partial.is_empty()).then(|| previous_partial.to_owned())
    } else {
        Some(hypothesis)
    }
}

/// คืนเวลา Unix ปัจจุบันโดย fallback เป็นศูนย์หากนาฬิการะบบอยู่ก่อน epoch
fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

/// ตรวจค่าที่มีผลต่อความถูกต้องและขนาดงานของ Whisper ก่อนโหลดโมเดล
fn validate_config(config: &SttStartConfig) -> Result<(), String> {
    if config.language != "en" {
        return Err(format!(
            "Unsupported STT language '{}'; Phase 1 supports English ('en') only",
            config.language
        ));
    }
    validate_streaming_config(SttStreamingConfig {
        step_ms: config.step_ms,
        window_ms: config.window_ms,
        vad_enabled: config.vad_enabled,
    })
}

/// ตรวจค่าที่แก้สดได้ก่อนเปลี่ยนขนาดหน้าต่างเสียงของเซสชันปัจจุบัน
fn validate_streaming_config(config: SttStreamingConfig) -> Result<(), String> {
    if config.step_ms == 0 {
        return Err("STT step_ms must be greater than zero".to_owned());
    }
    if config.window_ms < config.step_ms {
        return Err("STT window_ms must be greater than or equal to step_ms".to_owned());
    }
    if config.window_ms > 30_000 {
        return Err("STT window_ms must not exceed 30000".to_owned());
    }
    Ok(())
}

/// แปลงมิลลิวินาทีเป็นจำนวนตัวอย่างตามอัตราตัวอย่างกลางของระบบ
fn milliseconds_to_samples(milliseconds: u32) -> usize {
    (u64::from(milliseconds) * u64::from(SAMPLE_RATE_HZ) / 1_000) as usize
}

/// แปลง Duration เป็นจำนวน sample โดยปัดลงตามอัตราตัวอย่างกลางของระบบ
fn duration_to_samples(duration: Duration) -> usize {
    (duration.as_secs_f64() * f64::from(SAMPLE_RATE_HZ)) as usize
}

/// ตรวจว่าช่วง sample ใหม่มีพลังงานพอที่จะคุ้มกับการเรียก Whisper หรือไม่
fn has_audible_signal(samples: &[f32], minimum_rms: f32) -> bool {
    if samples.is_empty() {
        return false;
    }

    let mean_square = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt() >= f64::from(minimum_rms)
}

/// รวมทุกช่วงข้อความดิบของ Whisper เพื่อให้ผู้เรียกกรองและบันทึกเทียบกันได้
fn collect_hypothesis(state: &WhisperState) -> Result<String, String> {
    let mut text = String::new();
    for segment in state.as_iter() {
        let segment = segment
            .to_str_lossy()
            .map_err(|error| format!("Failed to read Whisper transcript: {error}"))?;
        text.push_str(&segment);
    }
    Ok(text)
}

/// ลบเครื่องหมายที่ Whisper ใช้แทนเสียงว่าง โดยไม่เปลี่ยนคำพูดจริง
pub(crate) fn normalize_hypothesis_text(text: &str) -> String {
    const BLANK_AUDIO_MARKER: &[u8] = b"[BLANK_AUDIO]";

    let mut without_marker = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let remaining = &text.as_bytes()[index..];
        if remaining.len() >= BLANK_AUDIO_MARKER.len()
            && remaining[..BLANK_AUDIO_MARKER.len()].eq_ignore_ascii_case(BLANK_AUDIO_MARKER)
        {
            index += BLANK_AUDIO_MARKER.len();
            while index < text.len() {
                let character = text[index..]
                    .chars()
                    .next()
                    .expect("valid character boundary");
                if matches!(character, ',' | '.' | '!' | '?' | ';' | ':') {
                    index += character.len_utf8();
                } else {
                    break;
                }
            }
            without_marker.push(' ');
            continue;
        }

        let character = text[index..]
            .chars()
            .next()
            .expect("valid character boundary");
        without_marker.push(character);
        index += character.len_utf8();
    }

    without_marker
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// ซ่อนเครื่องหมายจบประโยคจาก draft เพราะ Whisper ยังแก้คำท้ายและวรรคตอนได้
fn normalize_partial_hypothesis_text(text: &str) -> String {
    text.trim_end_matches(['.', '!', '?']).trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        final_hypothesis_or_previous, normalize_hypothesis_text,
        normalize_partial_hypothesis_text, spawn_service,
    };
    use crate::audio::LatestQueue;

    #[test]
    fn blank_audio_marker_is_removed_before_reconciliation() {
        assert_eq!(normalize_hypothesis_text(" [BLANK_AUDIO] "), "");
        assert_eq!(
            normalize_hypothesis_text("keep [blank_audio] repeated repeated words"),
            "keep repeated repeated words"
        );
        assert_eq!(normalize_hypothesis_text("[BLANK_AUDIO],"), "");
        assert_eq!(
            normalize_hypothesis_text("hello[BlAnK_aUdIo]there"),
            "hello there"
        );
    }

    #[test]
    fn idle_stt_worker_can_be_joined_idempotently() {
        let mut service = spawn_service(LatestQueue::new(1));

        assert!(service.finish().is_ok());
        assert!(service.finish().is_ok());
    }

    #[test]
    fn terminal_punctuation_is_deferred_until_final() {
        assert_eq!(normalize_partial_hypothesis_text("Welcome to my."), "Welcome to my");
        assert_eq!(normalize_partial_hypothesis_text("So?"), "So");
        assert_eq!(normalize_partial_hypothesis_text("Wait,"), "Wait,");
    }

    #[test]
    fn empty_final_uses_previous_partial() {
        assert_eq!(
            final_hypothesis_or_previous(String::new(), " previous partial "),
            Some("previous partial".to_owned())
        );
        assert_eq!(
            final_hypothesis_or_previous("final transcript".to_owned(), "previous partial"),
            Some("final transcript".to_owned())
        );
        assert_eq!(final_hypothesis_or_previous(String::new(), "  "), None);
    }
}
