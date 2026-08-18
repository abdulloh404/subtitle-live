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
    time::{Duration, Instant},
};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::audio::{LatestQueue, MixedAudioChunk, SAMPLE_RATE_HZ};

// คิวเหตุการณ์มีขอบเขตเพื่อรักษาความสดของคำบรรยายเมื่อ UI รับข้อมูลไม่ทัน
const EVENT_QUEUE_CAPACITY: usize = 64;
// สมมติฐานที่ไม่เปลี่ยนติดต่อกันสองรอบถือว่านิ่งพอที่จะยืนยันเป็นผลสุดท้าย
const STABLE_PASSES_TO_FINAL: u8 = 2;
// เวิร์กเกอร์พักสั้น ๆ ระหว่างรอบเพื่อรับคำสั่งได้ไวโดยไม่วนใช้ CPU เปล่า
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
// ระดับ RMS ต่ำกว่านี้ถือเป็นความเงียบและไม่ส่งเข้าโมเดล Whisper
const SILENCE_RMS_THRESHOLD: f32 = 0.001;
// เมื่อเปิดตัวกรองกิจกรรมเสียง จะใช้เกณฑ์ RMS สูงขึ้นเพื่อกันเสียงพลังงานต่ำก่อนถึง Whisper
const VAD_RMS_THRESHOLD: f32 = 0.003;

/// ค่าที่เปลี่ยนระหว่างทำงานได้โดยไม่ต้องโหลดโมเดล Whisper ใหม่
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SttStreamingConfig {
    /// ช่วงเสียงใหม่ขั้นต่ำระหว่างการอนุมานแต่ละรอบ
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
    /// ช่วงเสียงใหม่ขั้นต่ำระหว่างการอนุมานแต่ละรอบ
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
    /// คิวเหตุการณ์แบบเก็บข้อมูลล่าสุดเพื่อไม่ให้ STT ดันงานค้างไปถึง UI
    events: LatestQueue<SttEvent>,
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
        self.events.drain()
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
    let events = LatestQueue::new(EVENT_QUEUE_CAPACITY);
    let worker_events = events.clone();
    let audio_generation = Arc::new(AtomicU64::new(0));
    let worker_generation = Arc::clone(&audio_generation);
    let worker = thread::Builder::new()
        .name("whisper-stt".to_owned())
        .spawn(move || run_worker(input, command_receiver, worker_events, worker_generation))
        .expect("failed to spawn STT worker");

    SttService {
        commands,
        events,
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
    events: LatestQueue<SttEvent>,
    audio_generation: Arc<AtomicU64>,
) {
    let mut active: Option<ActiveStt> = None;
    let mut audio_paused = true;
    let mut current_audio_generation = 0;

    loop {
        match next_command(&commands, active.is_some()) {
            CommandState::Command(WorkerCommand::Start(config)) => {
                active = None;
                audio_paused = true;
                input.clear_reliable();
                events.push_latest_reliable(SttEvent::Loading);
                match ActiveStt::load(config, input.dropped()) {
                    Ok(loaded) => {
                        active = Some(loaded);
                        events.push_latest_reliable(SttEvent::Ready);
                    }
                    Err(error) => {
                        events.push_latest_reliable(SttEvent::Error(error));
                        events.push_latest_reliable(SttEvent::Stopped);
                    }
                }
            }
            CommandState::Command(WorkerCommand::Stop) => {
                active = None;
                audio_paused = true;
                input.clear_reliable();
                events.push_latest_reliable(SttEvent::Stopped);
            }
            CommandState::Command(WorkerCommand::UpdateStreaming(config)) => {
                input.clear_reliable();
                if let Some(stt) = active.as_mut()
                    && let Err(error) = stt.update_streaming(config)
                {
                    events.push_latest_reliable(SttEvent::Error(error));
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

        match stt.consume_latest(&input, current_audio_generation) {
            Ok(Some(result)) => {
                if let Some(update) = result.update {
                    events.push_latest(SttEvent::Transcript {
                        audio_generation: current_audio_generation,
                        audio_origin_at: result.audio_origin_at,
                        update,
                    });
                }
                events.push_latest(SttEvent::Metrics {
                    audio_generation: current_audio_generation,
                    audio_buffer_duration: result.audio_buffer_duration,
                    inference_duration: result.inference_duration,
                });
            }
            Ok(None) => {}
            Err(error) => {
                active = None;
                input.clear_reliable();
                events.push_latest_reliable(SttEvent::Error(error));
                events.push_latest_reliable(SttEvent::Stopped);
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
    /// จำนวนตัวอย่างใหม่ขั้นต่ำก่อนเริ่มการอนุมานครั้งถัดไป
    step_samples: usize,
    /// จำนวนตัวอย่างใหม่ที่สะสมตั้งแต่การอนุมานครั้งก่อน
    samples_since_inference: usize,
    /// เวลาเริ่มของเสียงใหม่ที่กำลังสะสมจนครบหนึ่ง audio step
    pending_audio_started_at: Option<Instant>,
    /// ตัวนับการทิ้งข้อมูลล่าสุดที่เห็น ใช้ตรวจจับช่วงเสียงขาดตอน
    observed_dropped: u64,
    /// รหัสช่วงคำพูดปัจจุบัน เพิ่มเมื่อยืนยันผลหรือเสียงขาดตอน
    segment_id: u64,
    /// สมมติฐานรอบก่อน ใช้วัดความนิ่งของผลลัพธ์
    previous_hypothesis: String,
    /// จำนวนรอบต่อเนื่องที่สมมติฐานไม่เปลี่ยน
    stable_passes: u8,
    /// ป้องกันการสร้างขอบเขต segment ซ้ำทุกครั้งที่รับ sample เงียบต่อเนื่อง
    silence_active: bool,
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
            samples_since_inference: 0,
            pending_audio_started_at: None,
            observed_dropped,
            segment_id: 1,
            previous_hypothesis: String::new(),
            stable_passes: 0,
            silence_active: true,
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

    /// รวมชิ้นเสียงล่าสุดเข้าหน้าต่างแบบเลื่อน และอนุมานเมื่อมีเสียงใหม่ครบหนึ่งระยะก้าว
    fn consume_latest(
        &mut self,
        input: &LatestQueue<MixedAudioChunk>,
        current_audio_generation: u64,
    ) -> Result<Option<InferenceResult>, String> {
        let dropped = input.dropped();
        if dropped != self.observed_dropped {
            self.reset_discontinuity();
            self.observed_dropped = dropped;
        }

        let chunks: Vec<_> = input
            .drain()
            .into_iter()
            .filter(|chunk| chunk.generation == current_audio_generation)
            .collect();
        if chunks.is_empty() {
            return Ok(None);
        }

        for chunk in chunks {
            self.pending_audio_started_at
                .get_or_insert(chunk.captured_at);
            self.samples_since_inference = self
                .samples_since_inference
                .saturating_add(chunk.samples.len());
            self.rolling_audio.extend(chunk.samples);
        }
        while self.rolling_audio.len() > self.window_samples {
            self.rolling_audio.pop_front();
        }

        if self.samples_since_inference < self.step_samples {
            return Ok(None);
        }
        let pending_samples = self.samples_since_inference.min(self.rolling_audio.len());
        self.samples_since_inference = 0;

        let audio = self.rolling_audio.make_contiguous();
        let pending_start = audio.len().saturating_sub(pending_samples);
        let minimum_rms = if self.config.vad_enabled {
            VAD_RMS_THRESHOLD
        } else {
            SILENCE_RMS_THRESHOLD
        };
        let input_is_silent = !has_audible_signal(&audio[pending_start..], minimum_rms);
        if input_is_silent {
            if self.silence_active {
                self.reset_audio();
            } else {
                self.reset_discontinuity();
            }
            return Ok(None);
        }
        self.silence_active = false;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.config.language));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        let inference_segment_id = self.segment_id;
        let whisper_model_started = Instant::now();
        let audio_origin_at = self
            .pending_audio_started_at
            .take()
            .unwrap_or(whisper_model_started);
        let audio_buffer_duration =
            whisper_model_started.saturating_duration_since(audio_origin_at);
        let gpu_backend_started = Instant::now();
        self.state
            .full(params, audio)
            .map_err(|error| format!("Whisper inference failed: {error}"))?;
        let gpu_backend_duration = gpu_backend_started.elapsed();
        let raw_text = collect_hypothesis(&self.state)?;
        let processed_text = normalize_hypothesis_text(&raw_text);
        let update = self.reconcile_hypothesis(processed_text.clone());
        let whisper_model_duration = whisper_model_started.elapsed();
        tracing::debug!(
            event = "whisper_model_latency",
            segment_id = inference_segment_id,
            whisper_model_latency_ms = whisper_model_duration.as_secs_f64() * 1_000.0,
            "วัด latency การถอดเสียงจากโมเดล whisper.cpp"
        );
        tracing::debug!(
            event = "gpu_transcription_latency",
            segment_id = inference_segment_id,
            gpu_backend_requested = self.gpu_backend_requested,
            gpu_device = self.gpu_device,
            gpu_backend_wall_ms = gpu_backend_duration.as_secs_f64() * 1_000.0,
            "วัดเวลาครอบ WhisperState::full ของ backend ที่ร้องขอ GPU"
        );
        let (result_kind, emitted_text) = match update.as_ref() {
            Some(TranscriptUpdate::Partial { text, .. }) => ("partial", text.as_str()),
            Some(TranscriptUpdate::Final { text, .. }) => ("final", text.as_str()),
            None => ("ignored", ""),
        };
        tracing::debug!(
            event = "whisper_transcript_text",
            segment_id = inference_segment_id,
            result_kind,
            raw_text = %raw_text,
            processed_text = %processed_text,
            emitted_text = %emitted_text,
            "แสดงข้อความดิบและข้อความที่ตัดออกจาก Whisper"
        );

        Ok(Some(InferenceResult {
            update,
            audio_origin_at,
            audio_buffer_duration,
            inference_duration: gpu_backend_duration,
        }))
    }

    /// ส่งสมมติฐานเป็นผลชั่วคราวจนกว่าจะคงเดิมครบจำนวนรอบที่กำหนดจึงยืนยันผล
    fn reconcile_hypothesis(&mut self, hypothesis: String) -> Option<TranscriptUpdate> {
        let hypothesis = hypothesis.trim().to_owned();
        if hypothesis.is_empty() {
            return None;
        }

        if hypothesis == self.previous_hypothesis {
            self.stable_passes = self.stable_passes.saturating_add(1);
        } else {
            self.previous_hypothesis.clone_from(&hypothesis);
            self.stable_passes = 0;
        }

        if self.stable_passes >= STABLE_PASSES_TO_FINAL {
            let update = TranscriptUpdate::Final {
                segment_id: self.segment_id,
                text: hypothesis,
            };
            self.segment_id = self.segment_id.saturating_add(1);
            self.previous_hypothesis.clear();
            self.stable_passes = 0;
            self.reset_audio();
            Some(update)
        } else {
            Some(TranscriptUpdate::Partial {
                segment_id: self.segment_id,
                text: hypothesis,
            })
        }
    }

    /// ล้างเฉพาะบัฟเฟอร์เสียง แต่คงรหัสช่วงและโมเดลที่โหลดไว้
    fn reset_audio(&mut self) {
        self.rolling_audio.clear();
        self.samples_since_inference = 0;
        self.pending_audio_started_at = None;
    }

    /// ล้างทั้งเสียงและสมมติฐานเมื่อแหล่งเสียงหยุด เปลี่ยน หรือมีข้อมูลตกหล่น
    fn reset_discontinuity(&mut self) {
        self.reset_audio();
        self.previous_hypothesis.clear();
        self.stable_passes = 0;
        self.segment_id = self.segment_id.saturating_add(1);
        self.silence_active = true;
    }
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

#[cfg(test)]
mod tests {
    use super::{normalize_hypothesis_text, spawn_service};
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
}
