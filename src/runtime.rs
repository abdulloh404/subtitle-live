//! ประสานสถานะแอปบน GTK กับบริการเบื้องหลัง โดยไม่ให้ UI เรียก audio backend หรือ Whisper โดยตรง

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use crate::{
    app::{AppCommand, ApplicationController, ApplicationState, DebugSessionState},
    audio::{LatestQueue, MixedAudioChunk, MixerHandle, SourceAudioChunk, spawn_mixer},
    audio_source::{
        AudioSourceEvent, AudioSourceService, CaptureTarget, spawn_service as spawn_audio_source,
    },
    config::{AppConfig, ConfigWriter, default_debug_log_path},
    debug_log::{DebugLogStatus, DebugLogWriter},
    metrics::{LatencyTracker, MetricsSnapshot},
    stt::{
        SttDebugRecord, SttEvent, SttService, SttStartConfig, SttStreamingConfig, TranscriptUpdate,
        spawn_service as spawn_stt,
    },
    subtitle::TranscriptReconciler,
    tray::{TrayCommand, TrayIndicator, TrayState},
};

const SOURCE_QUEUE_CAPACITY: usize = 128;
// ตัว mixer สร้างเสียงครั้งละ 20 มิลลิวินาที จึงใช้ค่านี้แปลง window เป็นความจุคิว
const MIX_FRAME_DURATION_MS: usize = 20;
// เผื่อพื้นที่เพิ่มเพื่อให้ STT มีช่วงรับความผันผวนโดยไม่ทำให้คิวโตแบบไม่จำกัด
const INFERENCE_HEADROOM_MS: usize = 2_000;
// เมื่อไม่มีผลถอดเสียงใหม่เกินช่วงนี้ ให้ล้างบริบทเก่าและซ่อน overlay
const SUBTITLE_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
// จำกัด correlation ที่รอ overlay ตอบกลับ เพราะ frame เก่าอาจถูก mailbox แทนที่และไม่มี acknowledgment
const MAX_PENDING_OVERLAY_FRAMES: usize = 64;

/// ชุดการเปลี่ยนแปลงที่ GTK main thread ต้องนำไปใช้หลังจบรอบ poll หนึ่งครั้ง
#[derive(Debug, Default)]
pub struct RuntimeUpdate {
    /// รายการ stream เปลี่ยนและหน้า Audio Sources ต้องวาดใหม่
    pub streams_changed: bool,
    /// ข้อความล่าสุดที่พร้อมส่งไปยัง overlay
    pub subtitle: Option<SubtitleFrame>,
    /// ต้องซ่อน overlay เพราะหยุดงาน เปลี่ยนแหล่งเสียง หรือพบข้อผิดพลาด
    pub hide_overlay: bool,
    /// ต้องเปิดหน้าต่าง Settings จากคำสั่ง tray
    pub show_settings: bool,
    /// ต้องออกจาก GTK application อย่างเป็นระเบียบ
    pub quit_requested: bool,
    /// ข้อมูล inference ใหม่สำหรับหน้า Debug เฉพาะเมื่อผู้ใช้เปิด live debug
    pub debug_records: Vec<SttDebugRecord>,
    /// สถานะ writer ล่าสุดเมื่อมีการเปลี่ยนแปลง
    pub debug_log_status: Option<DebugLogStatus>,
    /// จำนวน debug record สะสมที่ถูกทิ้ง เมื่อค่าเปลี่ยนจากรอบก่อน
    pub debug_drop_counts: Option<DebugDropCounts>,
}

/// จำนวน debug record สะสมที่แต่ละคิวทิ้งตลอดอายุโปรเซส
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DebugDropCounts {
    /// จำนวน record ที่คิวระหว่าง STT กับ runtime ทิ้ง
    pub stt_queue: u64,
    /// จำนวน record ที่คิวระหว่าง runtime กับ file writer ทิ้ง
    pub file_queue: u64,
}

/// ข้อความหนึ่งเฟรมที่ runtime ส่งไปยัง GTK
#[derive(Debug)]
pub struct SubtitleFrame {
    /// ตัวข้อความสำหรับแสดงผล
    pub text: String,
    /// ระบุว่า Whisper ยืนยัน segment นี้แล้วหรือยังเป็น partial
    pub is_final: bool,
    /// เวลาเริ่มของ audio step ที่สร้างข้อความ สำหรับวัดจน overlay วาดเสร็จ
    pub audio_origin_at: Instant,
}

/// เจ้าของ lifecycle ของบริการเสียง, STT, tray, config และสถานะ transcript
pub struct ApplicationRuntime {
    // Controller อยู่บน GTK thread และเป็นแหล่งสถานะ/config กลางของ UI
    controller: Rc<RefCell<ApplicationController>>,
    // คิวทั้งสองเป็นคิวแบบเก็บข้อมูลล่าสุด เพื่อรักษา latency เมื่อ producer เร็วกว่า consumer
    source_audio: LatestQueue<SourceAudioChunk>,
    mixed_audio: LatestQueue<MixedAudioChunk>,
    // Handle ของ worker/service ใช้ส่งคำสั่งเท่านั้น งานหนักไม่เกิดบน GTK thread
    mixer: MixerHandle,
    audio_source: AudioSourceService,
    // Service รุ่นเก่าจาก Retry ถูกสั่งปิดแล้วและรอ join หลัง GTK event loop จบ
    retired_audio_sources: Vec<AudioSourceService>,
    stt: SttService,
    // Tray เป็น optional เพราะบาง desktop ไม่มี StatusNotifier host
    tray: Option<TrayIndicator>,
    tray_commands: Option<Receiver<TrayCommand>>,
    // Writer บันทึก config บน thread แยกเพื่อไม่บล็อก UI
    config_writer: Option<ConfigWriter>,
    // Writer debug แยก thread และไม่สร้างไฟล์จนกว่าผู้ใช้เปิดในเซสชันนี้
    debug_log_writer: DebugLogWriter,
    default_model_path: PathBuf,
    model_ready: bool,
    last_persisted_config: AppConfig,
    // เปรียบเทียบเป้าหมายที่ต้องการกับ capture ที่ audio backend ยืนยันว่าเริ่มแล้ว
    applied_targets: Vec<CaptureTarget>,
    desired_capture_ids: HashSet<u32>,
    active_captures: HashSet<u32>,
    capture_errors: HashMap<u32, String>,
    // ระหว่างเปลี่ยน capture จะหยุดรับ transcript จน STT ยืนยัน audio generation ใหม่
    capture_transition_pending: bool,
    // รุ่นเดียวที่ส่งผ่าน audio source, mixer และ STT เพื่อกันเสียงจาก transition เก่า
    pipeline_audio_generation: u64,
    stt_resume_generation: Option<u64>,
    accepted_audio_generation: Option<u64>,
    // Reconciler ป้องกัน partial/final ซ้ำและรักษาคำที่ commit แล้ว
    transcript: TranscriptReconciler,
    last_subtitle_update: Option<Instant>,
    metrics: LatencyTracker,
    // จับคู่ frame id ของ IPC กับต้นทางเสียง โดยเก็บจำนวนคงที่เพื่อรองรับ frame ที่ถูก coalesce
    pending_overlay_frames: VecDeque<(u64, i64)>,
    running_requested: bool,
    stt_ready: bool,
    // เลขคำขอ retry ที่ runtime ประมวลผลแล้ว ป้องกันการเริ่มซ้ำในแต่ละรอบ poll
    observed_retry_generation: u64,
    // ใช้ข้าม Stopped ที่เกิดจากคำสั่งหยุดโดยตั้งใจก่อนเริ่ม retry เท่านั้น
    retry_stop_pending: bool,
    pipeline_error: Option<String>,
    // เป็น true หลัง registry ส่ง snapshot ครั้งแรก จึงไม่รายงาน Connected ก่อนเชื่อมจริง
    audio_source_connected: bool,
    audio_source_error: Option<String>,
    startup_warning: Option<String>,
    last_tray_state: Option<TrayState>,
    // ค่า streaming ล่าสุดที่ส่งให้ STT ใช้ตรวจว่าต้องอัปเดต worker หรือไม่
    applied_stt_streaming: Option<SttStreamingConfig>,
    // สถานะ debug ล่าสุดที่ runtime ส่งให้ STT และ file writer แล้ว
    applied_debug_session: DebugSessionState,
    // snapshot ตัวนับ debug ที่ส่งให้ UI ล่าสุด เริ่มที่ศูนย์ทุกโปรเซส
    last_debug_drop_counts: DebugDropCounts,
    shutdown_requested: bool,
}

impl ApplicationRuntime {
    /// สร้าง bounded queues และ worker ทุกตัว แต่ยังไม่เริ่มถอดเสียงจน Live Subtitles เปิด
    pub fn new(
        controller: Rc<RefCell<ApplicationController>>,
        config_path: PathBuf,
        default_model_path: PathBuf,
        model_ready: bool,
        persistence_enabled: bool,
        mut startup_warning: Option<String>,
    ) -> Self {
        let last_persisted_config = controller.borrow().config_snapshot();
        let observed_retry_generation = controller.borrow().retry_generation();
        let source_audio = LatestQueue::new(SOURCE_QUEUE_CAPACITY);
        let mixed_audio =
            LatestQueue::new(mixed_queue_capacity(last_persisted_config.stt.window_ms));
        let mixer = spawn_mixer(source_audio.clone(), mixed_audio.clone());
        let audio_source =
            spawn_audio_source(last_persisted_config.audio.backend, source_audio.clone());
        let stt = spawn_stt(mixed_audio.clone());
        let (tray, tray_commands) = match TrayIndicator::spawn(TrayState::Paused) {
            Ok((tray, commands)) => (Some(tray), Some(commands)),
            Err(error) => {
                let warning = format!(
                    "Tray indicator unavailable ({error}); run the app again to reopen Settings"
                );
                tracing::warn!(error = %error, "Tray indicator is unavailable");
                startup_warning = Some(match startup_warning {
                    Some(existing) => format!("{existing}; {warning}"),
                    None => warning,
                });
                (None, None)
            }
        };
        let config_writer = persistence_enabled.then(|| ConfigWriter::spawn(config_path));
        let debug_log_path = default_debug_log_path().unwrap_or_else(|_| {
            default_model_path
                .parent()
                .and_then(|models| models.parent())
                .map(|data| data.join("logs/transcript-debug.jsonl"))
                .unwrap_or_else(|| PathBuf::from("transcript-debug.jsonl"))
        });
        let debug_log_writer = DebugLogWriter::spawn(debug_log_path);

        Self {
            controller,
            source_audio,
            mixed_audio,
            mixer,
            audio_source,
            retired_audio_sources: Vec::new(),
            stt,
            tray,
            tray_commands,
            config_writer,
            debug_log_writer,
            default_model_path,
            model_ready,
            last_persisted_config,
            applied_targets: Vec::new(),
            desired_capture_ids: HashSet::new(),
            active_captures: HashSet::new(),
            capture_errors: HashMap::new(),
            capture_transition_pending: false,
            pipeline_audio_generation: 0,
            stt_resume_generation: None,
            accepted_audio_generation: None,
            transcript: TranscriptReconciler::default(),
            last_subtitle_update: None,
            metrics: LatencyTracker::default(),
            pending_overlay_frames: VecDeque::new(),
            running_requested: false,
            stt_ready: false,
            observed_retry_generation,
            retry_stop_pending: false,
            pipeline_error: None,
            audio_source_connected: false,
            audio_source_error: None,
            startup_warning,
            last_tray_state: None,
            applied_stt_streaming: None,
            applied_debug_session: DebugSessionState::default(),
            last_debug_drop_counts: DebugDropCounts::default(),
            shutdown_requested: false,
        }
    }

    /// ระบาย command/event แบบไม่บล็อก แล้วคืนเฉพาะงาน UI ที่ต้องทำในรอบนี้
    pub fn poll(&mut self) -> RuntimeUpdate {
        let mut update = RuntimeUpdate::default();
        if self.shutdown_requested {
            return update;
        }

        self.reap_retired_audio_sources();
        self.handle_tray_commands(&mut update);
        self.sync_audio_source_backend(&mut update);
        self.sync_retry_intent(&mut update);
        self.sync_live_intent(&mut update);
        self.sync_debug_session(&mut update);
        self.sync_stt_streaming_settings(&mut update);
        self.sync_capture_targets(&mut update);
        self.handle_audio_source_events(&mut update);
        self.sync_capture_targets(&mut update);
        self.handle_stt_events(&mut update);
        self.handle_stt_debug_records(&mut update);
        self.sync_debug_drop_counts(&mut update);
        self.sync_stt_resume_generation();
        self.sync_capture_targets(&mut update);
        self.clear_stale_subtitle(&mut update);
        self.persist_if_changed();
        self.sync_tray_state();
        update
    }

    /// join เฉพาะ audio worker รุ่นเก่าที่จบแล้ว จึงไม่รอ backend loop บน GTK thread
    fn reap_retired_audio_sources(&mut self) {
        let mut index = 0;
        while index < self.retired_audio_sources.len() {
            if !self.retired_audio_sources[index].is_finished() {
                index += 1;
                continue;
            }
            let mut service = self.retired_audio_sources.swap_remove(index);
            if let Err(error) = service.finish() {
                tracing::error!(error = %error, "Retired audio source worker did not shut down cleanly");
            }
        }
    }

    /// คืนสถานะ lifecycle ปัจจุบันสำหรับหน้า Settings และ tray
    pub fn state(&self) -> ApplicationState {
        self.controller.borrow().state()
    }

    /// เลือกข้อผิดพลาดที่สำคัญที่สุดตามลำดับ pipeline → capture → audio source → startup
    pub fn last_error(&self) -> Option<&str> {
        self.pipeline_error
            .as_deref()
            .or_else(|| self.capture_errors.values().next().map(String::as_str))
            .or(self.audio_source_error.as_deref())
            .or(self.startup_warning.as_deref())
    }

    /// คืนสถานะการเชื่อมต่อ audio backend สำหรับหน้า About
    pub fn audio_source_status(&self) -> String {
        let status = if self.audio_source_error.is_some() {
            "Error"
        } else if self.audio_source_connected {
            "Connected"
        } else {
            "Starting"
        };
        format!("{} · {status}", self.audio_source.backend().display_name())
    }

    /// สร้าง snapshot latency พร้อมจำนวนข้อมูลที่ bounded queues จำเป็นต้องทิ้ง
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics
            .snapshot(self.source_audio.dropped(), self.mixed_audio.dropped())
    }

    /// ผูก frame ที่ส่งเข้า overlay IPC กับเวลาเริ่ม audio step ต้นทาง
    pub fn track_overlay_frame(&mut self, frame_id: u64, audio_origin_micros: i64) {
        if self.pending_overlay_frames.len() == MAX_PENDING_OVERLAY_FRAMES {
            self.pending_overlay_frames.pop_front();
        }
        self.pending_overlay_frames
            .push_back((frame_id, audio_origin_micros));
    }

    /// บันทึก End-to-End เมื่อ helper ยืนยันว่า GTK ผ่านรอบวาดของ frame แล้ว
    pub fn record_overlay_rendered(&mut self, frame_id: u64, rendered_at_micros: i64) {
        let Some(position) = self
            .pending_overlay_frames
            .iter()
            .position(|(pending_id, _)| *pending_id == frame_id)
        else {
            return;
        };
        let audio_origin_micros = self.pending_overlay_frames[position].1;
        self.pending_overlay_frames.drain(..=position);
        let Some(duration) = monotonic_duration(audio_origin_micros, rendered_at_micros) else {
            return;
        };
        self.metrics.record_end_to_end(duration);
    }

    /// ล้าง frame ที่ไม่มีทางถูกวาดแล้วเมื่อ overlay ถูกซ่อนหรือ transport ล้มเหลว
    pub fn clear_pending_overlay_frames(&mut self) {
        self.pending_overlay_frames.clear();
    }

    /// คืน model path ที่ผู้ใช้กำหนด หรือ path เริ่มต้นเมื่อไม่ได้ override
    pub fn model_path(&self) -> PathBuf {
        self.controller
            .borrow()
            .config()
            .stt
            .model_path
            .clone()
            .unwrap_or_else(|| self.default_model_path.clone())
    }

    /// ระบุว่า model ที่ resolve ตอน startup มีอยู่จริงหรือไม่
    pub const fn model_exists(&self) -> bool {
        self.model_ready
    }

    /// ส่งคำสั่งหยุดไปยังทุก worker เพียงครั้งเดียว โดยไม่รอ inference บน GTK thread
    pub fn request_shutdown(&mut self) {
        if self.shutdown_requested {
            return;
        }
        self.shutdown_requested = true;
        self.persist_if_changed();
        let _ = self.audio_source.stop_capture();
        self.source_audio.clear_reliable();
        self.mixed_audio.clear_reliable();
        self.pending_overlay_frames.clear();
        let _ = self.audio_source.shutdown();
        self.stt.stop();
        self.stt.shutdown();
        self.mixer.stop();
        if let Some(tray) = &self.tray {
            tray.shutdown();
        }
    }

    /// รอ worker ทุกตัวหลัง `Application::run` คืนค่าแล้ว จึงไม่บล็อก GTK main thread
    pub fn finish(&mut self) {
        self.request_shutdown();
        // อ่านค่าที่ผู้ใช้เลือกจริงตอนปิด แทน snapshot จาก poll รอบก่อนที่อาจยังไม่ทัน sync
        let desired_debug_session = self.controller.borrow().debug_session_state();
        self.debug_log_writer
            .set_enabled(desired_debug_session.file_logging_enabled);
        if let Err(error) = self.audio_source.finish() {
            tracing::error!(error = %error, "Audio source worker did not shut down cleanly");
        }
        for service in &mut self.retired_audio_sources {
            if let Err(error) = service.finish() {
                tracing::error!(error = %error, "Retired audio source worker did not shut down cleanly");
            }
        }
        self.retired_audio_sources.clear();
        if let Err(error) = self.mixer.finish() {
            tracing::error!(error = %error, "Audio mixer worker did not shut down cleanly");
        }
        if let Err(error) = self.stt.finish() {
            tracing::error!(error = %error, "Whisper worker did not shut down cleanly");
        }
        let debug_tail = self.stt.drain_debug_records();
        if desired_debug_session.file_logging_enabled {
            self.debug_log_writer.write_records(debug_tail);
        }
        if let Some(tray) = &mut self.tray
            && let Err(error) = tray.finish()
        {
            tracing::error!(error = %error, "Tray worker did not shut down cleanly");
        }
        if let Some(writer) = &mut self.config_writer {
            writer.finish();
        }
        if let Err(error) = self.debug_log_writer.finish() {
            tracing::error!(error = %error, "Transcript debug writer did not shut down cleanly");
        }
    }

    /// แปลงคำสั่งจาก tray เป็น AppCommand หรือ RuntimeUpdate โดยไม่แตะ worker โดยตรง
    fn handle_tray_commands(&mut self, update: &mut RuntimeUpdate) {
        let commands: Vec<_> = self
            .tray_commands
            .as_ref()
            .map(|commands| commands.try_iter().collect())
            .unwrap_or_default();
        for command in commands {
            match command {
                TrayCommand::StartSubtitles => {
                    let _ = self
                        .controller
                        .borrow_mut()
                        .handle_command(AppCommand::StartSubtitles);
                }
                TrayCommand::StopSubtitles => {
                    let _ = self
                        .controller
                        .borrow_mut()
                        .handle_command(AppCommand::StopSubtitles);
                }
                TrayCommand::RetryPipeline => {
                    let _ = self
                        .controller
                        .borrow_mut()
                        .handle_command(AppCommand::RetryPipeline);
                }
                TrayCommand::ShowSettings => update.show_settings = true,
                TrayCommand::Quit => update.quit_requested = true,
            }
        }
    }

    /// เปลี่ยน service เมื่อผู้ใช้เลือก backend ใหม่ โดยไม่ให้ UI แตะ implementation โดยตรง
    fn sync_audio_source_backend(&mut self, update: &mut RuntimeUpdate) {
        let desired_backend = self.controller.borrow().config().audio.backend;
        if self.audio_source.backend() == desired_backend {
            return;
        }

        let replacement = spawn_audio_source(desired_backend, self.source_audio.clone());
        let previous = std::mem::replace(&mut self.audio_source, replacement);
        let _ = previous.shutdown();
        self.retired_audio_sources.push(previous);
        self.audio_source_connected = false;
        self.audio_source_error = None;
        self.applied_targets.clear();
        self.desired_capture_ids.clear();
        self.active_captures.clear();
        self.capture_errors.clear();
        self.controller.borrow_mut().set_streams(Vec::new());
        update.streams_changed = true;
        update.hide_overlay = true;
    }

    /// ประมวลผลคำขอ retry จาก controller เพียงครั้งเดียวไม่ว่าจะมาจาก UI หรือ tray
    fn sync_retry_intent(&mut self, update: &mut RuntimeUpdate) {
        let requested_generation = self.controller.borrow().retry_generation();
        if requested_generation == self.observed_retry_generation {
            return;
        }
        self.observed_retry_generation = requested_generation;
        self.retry_pipeline(update);
    }

    /// ทำให้ lifecycle จริงตรงกับค่าปุ่ม Live Subtitles ใน config
    fn sync_live_intent(&mut self, update: &mut RuntimeUpdate) {
        let requested = self.controller.borrow().config().general.live_subtitles;
        if requested == self.running_requested {
            return;
        }

        self.running_requested = requested;
        if requested {
            self.start_pipeline();
        } else {
            self.stop_pipeline();
            update.hide_overlay = true;
        }
    }

    /// ส่งสถานะ debug ชั่วคราวไปยัง STT และ writer โดยไม่บันทึกลง config
    fn sync_debug_session(&mut self, update: &mut RuntimeUpdate) {
        let desired = self.controller.borrow().debug_session_state();
        if desired != self.applied_debug_session {
            let capture_enabled = desired.live_enabled || desired.file_logging_enabled;
            let previous_capture_enabled = self.applied_debug_session.live_enabled
                || self.applied_debug_session.file_logging_enabled;
            if capture_enabled != previous_capture_enabled {
                self.stt.set_debug_enabled(capture_enabled);
            }
            if desired.file_logging_enabled != self.applied_debug_session.file_logging_enabled {
                self.debug_log_writer
                    .set_enabled(desired.file_logging_enabled);
            }
            self.applied_debug_session = desired;
        }

        if let Some(status) = self.debug_log_writer.drain_statuses().into_iter().last() {
            update.debug_log_status = Some(status);
        }
    }

    /// ล้าง state จากรอบก่อน แล้วสั่ง Whisper โหลด model ด้วยค่าปัจจุบัน
    fn start_pipeline(&mut self) {
        let config = self.controller.borrow().config_snapshot();
        let streaming = SttStreamingConfig {
            step_ms: config.stt.step_ms,
            window_ms: config.stt.window_ms,
            vad_enabled: config.stt.vad_enabled,
        };
        self.pipeline_error = None;
        self.stt_ready = false;
        self.desired_capture_ids.clear();
        self.active_captures.clear();
        self.capture_errors.clear();
        self.capture_transition_pending = false;
        self.stt_resume_generation = None;
        self.accepted_audio_generation = None;
        self.applied_targets.clear();
        self.source_audio.clear_reliable();
        self.mixed_audio.clear_reliable();
        self.mixer.reset(self.pipeline_audio_generation);
        self.transcript.clear();
        self.last_subtitle_update = None;
        self.applied_stt_streaming = Some(streaming);
        let _ = self.audio_source.stop_capture();
        self.stt.start(SttStartConfig {
            model_path: config
                .stt
                .model_path
                .unwrap_or_else(|| self.default_model_path.clone()),
            language: config.stt.language,
            compute_backend: config.stt.backend,
            step_ms: streaming.step_ms,
            window_ms: streaming.window_ms,
            vad_enabled: streaming.vad_enabled,
        });
        self.set_state(if self.audio_source_error.is_some() {
            ApplicationState::Error
        } else {
            ApplicationState::Starting
        });
    }

    /// ทิ้ง session ที่เสีย เริ่ม generation ใหม่ แล้วเข้าทางเริ่ม pipeline ปกติอีกครั้ง
    fn retry_pipeline(&mut self, update: &mut RuntimeUpdate) {
        // ถ้า service หลักเสีย ให้เปลี่ยน handle ก่อนเริ่มใหม่แทนการส่งกลับไปยัง channel ที่ตายแล้ว
        if self.audio_source_error.is_some() {
            let backend = self.controller.borrow().config().audio.backend;
            let replacement = spawn_audio_source(backend, self.source_audio.clone());
            let previous = std::mem::replace(&mut self.audio_source, replacement);
            let _ = previous.shutdown();
            self.retired_audio_sources.push(previous);
            self.audio_source_connected = false;
        }
        // ทิ้งผล STT เก่าก่อนส่ง Stop เพื่อไม่ลบ Stopped ที่ใช้ยืนยันคำสั่ง retry รอบนี้
        let _ = self.stt.drain_events();
        let _ = self.audio_source.stop_capture();
        self.stt.stop();
        self.retry_stop_pending = true;
        self.source_audio.clear_reliable();
        self.mixed_audio.clear_reliable();
        self.pipeline_audio_generation = self.pipeline_audio_generation.saturating_add(1).max(1);
        self.mixer.reset(self.pipeline_audio_generation);
        self.running_requested = true;
        self.audio_source_error = None;
        self.start_pipeline();
        update.hide_overlay = true;
    }

    /// หยุด capture/STT และล้างเสียงกับ transcript ที่อาจค้างจาก session เดิม
    fn stop_pipeline(&mut self) {
        let _ = self.audio_source.stop_capture();
        self.stt.stop();
        self.stt_ready = false;
        self.desired_capture_ids.clear();
        self.active_captures.clear();
        self.capture_errors.clear();
        self.capture_transition_pending = false;
        self.stt_resume_generation = None;
        self.accepted_audio_generation = None;
        self.applied_targets.clear();
        self.source_audio.clear_reliable();
        self.mixed_audio.clear_reliable();
        self.mixer.reset(self.pipeline_audio_generation);
        self.transcript.clear();
        self.last_subtitle_update = None;
        self.applied_stt_streaming = None;
        self.pipeline_error = None;
        self.set_state(if self.audio_source_error.is_some() {
            ApplicationState::Error
        } else {
            ApplicationState::Stopped
        });
    }

    /// ส่งค่าหน้าต่างเสียงใหม่ไปยัง STT สด ๆ โดยคงโมเดลที่โหลดไว้
    fn sync_stt_streaming_settings(&mut self, update: &mut RuntimeUpdate) {
        if !self.running_requested {
            return;
        }
        let config = self.controller.borrow().config_snapshot();
        let desired = SttStreamingConfig {
            step_ms: config.stt.step_ms,
            window_ms: config.stt.window_ms,
            vad_enabled: config.stt.vad_enabled,
        };
        if self.applied_stt_streaming == Some(desired) {
            return;
        }

        self.begin_capture_transition(update);
        self.stt.update_streaming(desired);
        if !self.applied_targets.is_empty()
            && let Err(error) = self
                .audio_source
                .set_selected(self.applied_targets.clone(), self.pipeline_audio_generation)
        {
            self.pipeline_error = Some(error.to_string());
        }
        self.applied_stt_streaming = Some(desired);
    }

    /// รับเหตุการณ์ graph/capture จาก backend ที่เลือกแล้วสะท้อนกลับไปยัง controller
    fn handle_audio_source_events(&mut self, update: &mut RuntimeUpdate) {
        for event in self.audio_source.drain_events() {
            match event {
                AudioSourceEvent::StreamsChanged(streams) => {
                    self.audio_source_connected = true;
                    self.audio_source_error = None;
                    self.controller.borrow_mut().set_streams(streams);
                    update.streams_changed = true;
                }
                AudioSourceEvent::CaptureStarted {
                    runtime_id,
                    audio_generation,
                } if self.running_requested
                    && self.stt_ready
                    && audio_generation == self.pipeline_audio_generation
                    && self.current_runtime_is_selected(runtime_id) =>
                {
                    if self.capture_transition_pending && self.stt_resume_generation.is_none() {
                        self.source_audio.clear_reliable();
                        self.mixed_audio.clear_reliable();
                        self.mixer.reset(self.pipeline_audio_generation);
                        self.stt_resume_generation =
                            Some(self.stt.resume_audio(self.pipeline_audio_generation));
                    }
                    self.active_captures.insert(runtime_id);
                    self.capture_errors.remove(&runtime_id);
                }
                AudioSourceEvent::CaptureStopped {
                    runtime_id,
                    audio_generation,
                } if audio_generation == self.pipeline_audio_generation => {
                    self.active_captures.remove(&runtime_id);
                    self.capture_errors.remove(&runtime_id);
                }
                AudioSourceEvent::CaptureError {
                    runtime_id,
                    audio_generation,
                    message,
                } => {
                    if audio_generation != self.pipeline_audio_generation {
                        continue;
                    }
                    self.active_captures.remove(&runtime_id);
                    if self.current_runtime_is_selected(runtime_id) {
                        self.capture_errors.insert(runtime_id, message);
                    }
                }
                AudioSourceEvent::Error(error) => {
                    self.audio_source_connected = false;
                    self.audio_source_error = Some(error);
                    self.set_state(ApplicationState::Error);
                    update.hide_overlay = true;
                }
                AudioSourceEvent::CaptureStarted { .. }
                | AudioSourceEvent::CaptureStopped { .. } => {}
            }
        }
    }

    /// รับผลจาก Whisper เฉพาะ audio generation ปัจจุบัน แล้วส่งข้อความที่ reconcile แล้วไป GTK
    fn handle_stt_events(&mut self, update: &mut RuntimeUpdate) {
        for event in self.stt.drain_events() {
            match event {
                SttEvent::Loading if self.running_requested => {
                    self.stt_ready = false;
                    self.set_state(if self.audio_source_error.is_some() {
                        ApplicationState::Error
                    } else {
                        ApplicationState::Starting
                    });
                }
                SttEvent::Ready if self.running_requested => {
                    self.stt_ready = true;
                    self.pipeline_error = None;
                }
                SttEvent::Transcript {
                    audio_generation,
                    audio_origin_at,
                    update: transcript,
                } if self.running_requested
                    && self.stt_ready
                    && !self.capture_transition_pending
                    && self.accepted_audio_generation == Some(audio_generation) =>
                {
                    let is_final = matches!(&transcript, TranscriptUpdate::Final { .. });
                    let final_is_empty = matches!(
                        &transcript,
                        TranscriptUpdate::Final { text, .. } if text.trim().is_empty()
                    );
                    let presentation_changed = self.transcript.apply(&transcript);
                    self.last_subtitle_update = Some(Instant::now());
                    let presentation = self.transcript.presentation_text();
                    if final_is_empty && presentation.is_empty() {
                        update.subtitle = None;
                        update.hide_overlay = true;
                        self.last_subtitle_update = None;
                    } else if presentation_changed || is_final {
                        update.subtitle = Some(SubtitleFrame {
                            text: presentation.to_owned(),
                            is_final,
                            audio_origin_at,
                        });
                    }
                }
                SttEvent::Metrics {
                    audio_generation,
                    audio_buffer_duration,
                    inference_duration,
                } if self.running_requested
                    && !self.capture_transition_pending
                    && self.accepted_audio_generation == Some(audio_generation) =>
                {
                    self.metrics
                        .record_stt(audio_buffer_duration, inference_duration);
                }
                SttEvent::Error(error) if self.running_requested => {
                    self.stt_ready = false;
                    self.active_captures.clear();
                    self.capture_transition_pending = false;
                    self.stt_resume_generation = None;
                    self.accepted_audio_generation = None;
                    let _ = self.audio_source.stop_capture();
                    self.pipeline_error = Some(error);
                    self.set_state(ApplicationState::Error);
                    update.hide_overlay = true;
                }
                SttEvent::Stopped if self.retry_stop_pending => {
                    self.retry_stop_pending = false;
                }
                SttEvent::Stopped if self.running_requested && self.pipeline_error.is_none() => {
                    self.stt_ready = false;
                    self.capture_transition_pending = false;
                    self.stt_resume_generation = None;
                    self.accepted_audio_generation = None;
                    self.pipeline_error =
                        Some("Speech recognition stopped unexpectedly".to_owned());
                    self.set_state(ApplicationState::Error);
                    update.hide_overlay = true;
                }
                SttEvent::Stopped
                | SttEvent::Loading
                | SttEvent::Ready
                | SttEvent::Transcript { .. }
                | SttEvent::Metrics { .. }
                | SttEvent::Error(_) => {}
            }
        }
    }

    /// แยก raw transcript ออกจาก event ปกติ แล้วส่งต่อเฉพาะปลายทางที่ผู้ใช้เปิดไว้
    fn handle_stt_debug_records(&mut self, update: &mut RuntimeUpdate) {
        let records = self.stt.drain_debug_records();
        if records.is_empty() {
            return;
        }
        if self.applied_debug_session.file_logging_enabled {
            self.debug_log_writer.write_records(records.iter().cloned());
        }
        if self.applied_debug_session.live_enabled {
            update.debug_records = records;
        }
    }

    /// ส่งตัวนับคิวตกหล่นเฉพาะเมื่อ snapshot เปลี่ยน เพื่อไม่สร้างงาน UI ทุก poll
    fn sync_debug_drop_counts(&mut self, update: &mut RuntimeUpdate) {
        let counts = DebugDropCounts {
            stt_queue: self.stt.debug_records_dropped(),
            file_queue: self.debug_log_writer.dropped_records(),
        };
        if counts == self.last_debug_drop_counts {
            return;
        }
        self.last_debug_drop_counts = counts;
        update.debug_drop_counts = Some(counts);
    }

    /// คำนวณ stream ที่ตรงกับกฎคงทน และเริ่ม transition เมื่อชุดเป้าหมายเปลี่ยน
    fn sync_capture_targets(&mut self, update: &mut RuntimeUpdate) {
        if !self.running_requested || !self.stt_ready {
            return;
        }
        let (targets, streams) = {
            let controller = self.controller.borrow();
            (controller.selected_targets(), controller.streams().to_vec())
        };
        let desired_capture_ids: HashSet<_> = streams
            .iter()
            .filter(|stream| targets.iter().any(|target| target.matches(stream)))
            .map(|stream| stream.runtime_id)
            .collect();

        if targets.is_empty() {
            let selection_error =
                "Select at least one application or playback stream in Audio Sources";
            if self.pipeline_error.as_deref() != Some(selection_error) {
                self.begin_capture_transition(update);
                self.applied_targets.clear();
                self.desired_capture_ids.clear();
            }
            self.pipeline_error = Some(selection_error.to_owned());
            self.update_capture_state();
            return;
        }

        let selection_changed =
            targets != self.applied_targets || desired_capture_ids != self.desired_capture_ids;
        if selection_changed {
            self.begin_capture_transition(update);
            self.applied_targets = targets.clone();
            self.desired_capture_ids = desired_capture_ids;
            self.pipeline_error = None;
            if let Err(error) = self
                .audio_source
                .set_selected(targets, self.pipeline_audio_generation)
            {
                self.pipeline_error = Some(error.to_string());
            }
        } else {
            self.capture_errors
                .retain(|runtime_id, _| self.desired_capture_ids.contains(runtime_id));
        }
        self.update_capture_state();
    }

    /// สร้างเส้นแบ่ง session เพื่อไม่ให้เสียงหรือ transcript จาก source เดิมรั่วเข้า source ใหม่
    fn begin_capture_transition(&mut self, update: &mut RuntimeUpdate) {
        let _ = self.audio_source.stop_capture();
        self.stt.pause_audio();
        self.source_audio.clear_reliable();
        self.mixed_audio.clear_reliable();
        self.pipeline_audio_generation = self.pipeline_audio_generation.saturating_add(1).max(1);
        self.mixer.reset(self.pipeline_audio_generation);
        self.transcript.clear();
        self.last_subtitle_update = None;
        self.active_captures.clear();
        self.capture_errors.clear();
        self.capture_transition_pending = true;
        self.stt_resume_generation = None;
        self.accepted_audio_generation = None;
        update.hide_overlay = true;
    }

    /// สรุป readiness ของ STT และ capture เป็นสถานะระดับแอป
    fn update_capture_state(&self) {
        let state = if self.pipeline_error.is_some()
            || self.audio_source_error.is_some()
            || !self.capture_errors.is_empty()
        {
            ApplicationState::Error
        } else if !self.running_requested
            || !self.stt_ready
            || self.desired_capture_ids.is_empty()
            || self.capture_transition_pending
            || !self.desired_capture_ids.is_subset(&self.active_captures)
        {
            ApplicationState::Starting
        } else {
            ApplicationState::Running
        };
        self.set_state(state);
    }

    /// ตรวจ runtime ID กับกฎที่เลือกโดยใช้ metadata ปัจจุบัน ไม่ใช้ ID เป็นตัวตนถาวร
    fn current_runtime_is_selected(&self, runtime_id: u32) -> bool {
        let controller = self.controller.borrow();
        let targets = controller.selected_targets();
        controller
            .streams()
            .iter()
            .find(|stream| stream.runtime_id == runtime_id)
            .is_some_and(|stream| targets.iter().any(|target| target.matches(stream)))
    }

    /// รอ worker ยืนยัน generation เพื่อเปิดรับผล STT หลัง capture transition
    fn sync_stt_resume_generation(&mut self) {
        let Some(expected_generation) = self.stt_resume_generation else {
            return;
        };
        if self.stt.audio_generation() == expected_generation {
            self.stt_resume_generation = None;
            self.capture_transition_pending = false;
            self.accepted_audio_generation = Some(expected_generation);
        }
    }

    /// ส่ง config ไป writer เฉพาะเมื่อ snapshot เปลี่ยนจากค่าที่บันทึกล่าสุด
    fn persist_if_changed(&mut self) {
        let config = self.controller.borrow().config_snapshot();
        if config == self.last_persisted_config {
            return;
        }
        if let Some(writer) = &self.config_writer {
            writer.save(&config);
        }
        self.last_persisted_config = config;
    }

    /// ปิด cue ที่เงียบเกินกำหนดโดยไม่ restart สายเสียงทั้งชุด
    fn clear_stale_subtitle(&mut self, update: &mut RuntimeUpdate) {
        let Some(last_update) = self.last_subtitle_update else {
            return;
        };
        if !subtitle_idle_timed_out(last_update, Instant::now()) {
            return;
        }
        self.transcript.clear();
        self.last_subtitle_update = None;
        update.hide_overlay = true;
    }

    /// ส่งสถานะไป tray เมื่อค่าที่แสดงต้องเปลี่ยนเท่านั้น
    fn sync_tray_state(&mut self) {
        let state = match self.state() {
            ApplicationState::Running => TrayState::Active,
            ApplicationState::Error => TrayState::Error,
            ApplicationState::Stopped | ApplicationState::Starting | ApplicationState::Stopping => {
                TrayState::Paused
            }
        };
        if self.last_tray_state == Some(state) {
            return;
        }
        if let Some(tray) = &self.tray {
            tray.set_state(state);
        }
        self.last_tray_state = Some(state);
    }

    /// เปลี่ยนสถานะ controller
    fn set_state(&self, state: ApplicationState) {
        self.controller.borrow_mut().set_state(state);
    }
}

/// คืนค่า true ตั้งแต่วินาทีที่ 3 หลัง subtitle อัปเดตครั้งล่าสุด เพื่อให้ทดสอบขอบเวลาได้แน่นอน
fn subtitle_idle_timed_out(last_update: Instant, now: Instant) -> bool {
    now.checked_duration_since(last_update)
        .is_some_and(|idle| idle >= SUBTITLE_IDLE_TIMEOUT)
}

/// แปลง STT window เป็นจำนวน mixed frames และจำกัดคิวไม่ให้เล็กหรือใหญ่เกินไป
fn mixed_queue_capacity(window_ms: u32) -> usize {
    let retained_ms = usize::try_from(window_ms)
        .unwrap_or(3_000)
        .saturating_add(INFERENCE_HEADROOM_MS);
    retained_ms.div_ceil(MIX_FRAME_DURATION_MS).clamp(64, 2_000)
}

/// แปลง timestamp monotonic จาก main/helper เป็น duration โดยปฏิเสธลำดับเวลาที่ผิด
fn monotonic_duration(origin_micros: i64, rendered_micros: i64) -> Option<Duration> {
    rendered_micros
        .checked_sub(origin_micros)
        .and_then(|duration| u64::try_from(duration).ok())
        .map(Duration::from_micros)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{monotonic_duration, subtitle_idle_timed_out};

    #[test]
    fn subtitle_hides_when_idle_reaches_five_seconds() {
        let last_update = Instant::now();

        assert!(!subtitle_idle_timed_out(
            last_update,
            last_update + Duration::from_millis(4_999)
        ));
        assert!(subtitle_idle_timed_out(
            last_update,
            last_update + Duration::from_secs(5)
        ));
    }

    #[test]
    fn rendered_timestamp_produces_end_to_end_duration() {
        assert_eq!(
            monotonic_duration(1_000_000, 1_275_000),
            Some(Duration::from_millis(275))
        );
        assert_eq!(monotonic_duration(2_000, 1_000), None);
    }
}
