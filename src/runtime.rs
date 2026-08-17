//! Application-level orchestration between GTK-facing state and background services.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use crate::{
    app::{AppCommand, ApplicationController, ApplicationState},
    audio::{
        LatestQueue, MixedAudioChunk, MixerHandle, SourceAudioChunk, spawn_mixer,
    },
    config::{AppConfig, ConfigWriter},
    metrics::{LatencyTracker, MetricsSnapshot},
    pipewire::{CaptureTarget, PipeWireEvent, PipeWireService, spawn_service as spawn_pipewire},
    stt::{SttEvent, SttService, SttStartConfig, TranscriptUpdate, spawn_service as spawn_stt},
    subtitle::TranscriptReconciler,
    tray::{TrayCommand, TrayIndicator, TrayState},
};

const SOURCE_QUEUE_CAPACITY: usize = 128;
const MIX_FRAME_DURATION_MS: usize = 20;
const INFERENCE_HEADROOM_MS: usize = 2_000;
const SUBTITLE_IDLE_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Default)]
pub struct RuntimeUpdate {
    pub streams_changed: bool,
    pub subtitle: Option<SubtitleFrame>,
    pub hide_overlay: bool,
    pub show_settings: bool,
    pub quit_requested: bool,
}

#[derive(Debug)]
pub struct SubtitleFrame {
    pub text: String,
    pub is_final: bool,
}

pub struct ApplicationRuntime {
    controller: Rc<RefCell<ApplicationController>>,
    source_audio: LatestQueue<SourceAudioChunk>,
    mixed_audio: LatestQueue<MixedAudioChunk>,
    mixer: MixerHandle,
    pipewire: PipeWireService,
    stt: SttService,
    tray: Option<TrayIndicator>,
    tray_commands: Option<Receiver<TrayCommand>>,
    config_writer: Option<ConfigWriter>,
    default_model_path: PathBuf,
    model_ready: bool,
    last_persisted_config: AppConfig,
    applied_targets: Vec<CaptureTarget>,
    desired_capture_ids: HashSet<u32>,
    active_captures: HashSet<u32>,
    capture_errors: HashMap<u32, String>,
    capture_transition_pending: bool,
    stt_resume_generation: Option<u64>,
    accepted_audio_generation: Option<u64>,
    transcript: TranscriptReconciler,
    last_subtitle_update: Option<Instant>,
    metrics: LatencyTracker,
    running_requested: bool,
    stt_ready: bool,
    pipeline_error: Option<String>,
    pipewire_error: Option<String>,
    startup_warning: Option<String>,
    last_tray_state: Option<TrayState>,
    shutdown_requested: bool,
}

impl ApplicationRuntime {
    pub fn new(
        controller: Rc<RefCell<ApplicationController>>,
        config_path: PathBuf,
        default_model_path: PathBuf,
        model_ready: bool,
        persistence_enabled: bool,
        mut startup_warning: Option<String>,
    ) -> Self {
        let last_persisted_config = controller.borrow().config_snapshot();
        let source_audio = LatestQueue::new(SOURCE_QUEUE_CAPACITY);
        let mixed_audio = LatestQueue::new(mixed_queue_capacity(
            last_persisted_config.stt.window_ms,
        ));
        let mixer = spawn_mixer(source_audio.clone(), mixed_audio.clone());
        let pipewire = spawn_pipewire(source_audio.clone());
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

        Self {
            controller,
            source_audio,
            mixed_audio,
            mixer,
            pipewire,
            stt,
            tray,
            tray_commands,
            config_writer,
            default_model_path,
            model_ready,
            last_persisted_config,
            applied_targets: Vec::new(),
            desired_capture_ids: HashSet::new(),
            active_captures: HashSet::new(),
            capture_errors: HashMap::new(),
            capture_transition_pending: false,
            stt_resume_generation: None,
            accepted_audio_generation: None,
            transcript: TranscriptReconciler::default(),
            last_subtitle_update: None,
            metrics: LatencyTracker::default(),
            running_requested: false,
            stt_ready: false,
            pipeline_error: None,
            pipewire_error: None,
            startup_warning,
            last_tray_state: None,
            shutdown_requested: false,
        }
    }

    pub fn poll(&mut self) -> RuntimeUpdate {
        let mut update = RuntimeUpdate::default();
        if self.shutdown_requested {
            return update;
        }

        self.handle_tray_commands(&mut update);
        self.sync_live_intent(&mut update);
        self.sync_capture_targets(&mut update);
        self.handle_pipewire_events(&mut update);
        self.sync_capture_targets(&mut update);
        self.handle_stt_events(&mut update);
        self.sync_stt_resume_generation();
        self.sync_capture_targets(&mut update);
        self.clear_stale_subtitle(&mut update);
        self.persist_if_changed();
        self.sync_tray_state();
        update
    }

    pub fn state(&self) -> ApplicationState {
        self.controller.borrow().state()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.pipeline_error
            .as_deref()
            .or_else(|| self.capture_errors.values().next().map(String::as_str))
            .or(self.pipewire_error.as_deref())
            .or(self.startup_warning.as_deref())
    }

    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot(
            self.source_audio.dropped(),
            self.mixed_audio.dropped(),
        )
    }

    pub fn model_path(&self) -> PathBuf {
        self.controller
            .borrow()
            .config()
            .stt
            .model_path
            .clone()
            .unwrap_or_else(|| self.default_model_path.clone())
    }

    pub const fn model_exists(&self) -> bool {
        self.model_ready
    }

    pub fn request_shutdown(&mut self) {
        if self.shutdown_requested {
            return;
        }
        self.shutdown_requested = true;
        self.persist_if_changed();
        let _ = self.pipewire.stop_capture();
        let _ = self.pipewire.shutdown();
        self.stt.stop();
        self.stt.shutdown();
        self.mixer.stop();
        if let Some(tray) = &self.tray {
            tray.shutdown();
        }
    }

    /// Completes pending configuration I/O after `Application::run` has returned.
    pub fn finish(&mut self) {
        self.request_shutdown();
        if let Some(writer) = &mut self.config_writer {
            writer.finish();
        }
    }

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
                TrayCommand::ShowSettings => update.show_settings = true,
                TrayCommand::Quit => update.quit_requested = true,
            }
        }
    }

    fn sync_live_intent(&mut self, update: &mut RuntimeUpdate) {
        let requested = self
            .controller
            .borrow()
            .config()
            .general
            .live_subtitles;
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

    fn start_pipeline(&mut self) {
        let config = self.controller.borrow().config_snapshot();
        self.pipeline_error = None;
        self.stt_ready = false;
        self.desired_capture_ids.clear();
        self.active_captures.clear();
        self.capture_errors.clear();
        self.capture_transition_pending = false;
        self.stt_resume_generation = None;
        self.accepted_audio_generation = None;
        self.applied_targets.clear();
        self.source_audio.clear();
        self.mixed_audio.clear();
        self.mixer.reset();
        self.transcript.clear();
        self.last_subtitle_update = None;
        let _ = self.pipewire.stop_capture();
        self.stt.start(SttStartConfig {
            model_path: config
                .stt
                .model_path
                .unwrap_or_else(|| self.default_model_path.clone()),
            language: config.stt.language,
            step_ms: config.stt.step_ms,
            window_ms: config.stt.window_ms,
        });
        self.set_state(if self.pipewire_error.is_some() {
            ApplicationState::Error
        } else {
            ApplicationState::Starting
        });
    }

    fn stop_pipeline(&mut self) {
        let _ = self.pipewire.stop_capture();
        self.stt.stop();
        self.stt_ready = false;
        self.desired_capture_ids.clear();
        self.active_captures.clear();
        self.capture_errors.clear();
        self.capture_transition_pending = false;
        self.stt_resume_generation = None;
        self.accepted_audio_generation = None;
        self.applied_targets.clear();
        self.source_audio.clear();
        self.mixed_audio.clear();
        self.mixer.reset();
        self.transcript.clear();
        self.last_subtitle_update = None;
        self.pipeline_error = None;
        self.set_state(if self.pipewire_error.is_some() {
            ApplicationState::Error
        } else {
            ApplicationState::Stopped
        });
    }

    fn handle_pipewire_events(&mut self, update: &mut RuntimeUpdate) {
        for event in self.pipewire.drain_events() {
            match event {
                PipeWireEvent::StreamsChanged(streams) => {
                    self.controller.borrow_mut().set_streams(streams);
                    update.streams_changed = true;
                }
                PipeWireEvent::CaptureStarted(runtime_id)
                    if self.running_requested
                        && self.stt_ready
                        && self.current_runtime_is_selected(runtime_id) =>
                {
                    if self.capture_transition_pending && self.stt_resume_generation.is_none() {
                        self.source_audio.clear();
                        self.mixed_audio.clear();
                        self.mixer.reset();
                        self.stt_resume_generation = Some(self.stt.resume_audio());
                    }
                    self.active_captures.insert(runtime_id);
                    self.capture_errors.remove(&runtime_id);
                }
                PipeWireEvent::CaptureStopped(runtime_id) => {
                    self.active_captures.remove(&runtime_id);
                    self.capture_errors.remove(&runtime_id);
                }
                PipeWireEvent::CaptureError {
                    runtime_id,
                    message,
                } => {
                    self.active_captures.remove(&runtime_id);
                    if self.current_runtime_is_selected(runtime_id) {
                        self.capture_errors.insert(runtime_id, message);
                    }
                }
                PipeWireEvent::Error(error) => {
                    self.pipewire_error = Some(error);
                    self.set_state(ApplicationState::Error);
                    update.hide_overlay = true;
                }
                PipeWireEvent::CaptureStarted(_) => {}
            }
        }
    }

    fn handle_stt_events(&mut self, update: &mut RuntimeUpdate) {
        for event in self.stt.drain_events() {
            match event {
                SttEvent::Loading if self.running_requested => {
                    self.stt_ready = false;
                    self.set_state(if self.pipewire_error.is_some() {
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
                    update: transcript,
                }
                    if self.running_requested
                        && self.stt_ready
                        && !self.capture_transition_pending
                        && self.accepted_audio_generation == Some(audio_generation) =>
                {
                    let is_final = matches!(&transcript, TranscriptUpdate::Final { .. });
                    let presentation_changed = self.transcript.apply(&transcript);
                    self.last_subtitle_update = Some(Instant::now());
                    if presentation_changed || is_final {
                        update.subtitle = Some(SubtitleFrame {
                            text: self.transcript.presentation_text().to_owned(),
                            is_final,
                        });
                    }
                }
                SttEvent::Metrics {
                    audio_generation,
                    inference_duration,
                    approximate_total,
                } if self.running_requested
                    && !self.capture_transition_pending
                    && self.accepted_audio_generation == Some(audio_generation) =>
                {
                    self.metrics.record(inference_duration, approximate_total);
                }
                SttEvent::Error(error) if self.running_requested => {
                    self.stt_ready = false;
                    self.active_captures.clear();
                    self.capture_transition_pending = false;
                    self.stt_resume_generation = None;
                    self.accepted_audio_generation = None;
                    let _ = self.pipewire.stop_capture();
                    self.pipeline_error = Some(error);
                    self.set_state(ApplicationState::Error);
                    update.hide_overlay = true;
                }
                SttEvent::Stopped if self.running_requested && self.pipeline_error.is_none() => {
                    self.stt_ready = false;
                    self.capture_transition_pending = false;
                    self.stt_resume_generation = None;
                    self.accepted_audio_generation = None;
                    self.pipeline_error = Some("Speech recognition stopped unexpectedly".to_owned());
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

        let selection_changed = targets != self.applied_targets
            || desired_capture_ids != self.desired_capture_ids;
        if selection_changed {
            self.begin_capture_transition(update);
            self.applied_targets = targets.clone();
            self.desired_capture_ids = desired_capture_ids;
            self.pipeline_error = None;
            if let Err(error) = self.pipewire.set_selected(targets) {
                self.pipeline_error = Some(error.to_string());
            }
        } else {
            self.capture_errors
                .retain(|runtime_id, _| self.desired_capture_ids.contains(runtime_id));
        }
        self.update_capture_state();
    }

    fn begin_capture_transition(&mut self, update: &mut RuntimeUpdate) {
        let _ = self.pipewire.stop_capture();
        self.stt.pause_audio();
        self.source_audio.clear();
        self.mixed_audio.clear();
        self.mixer.reset();
        self.transcript.clear();
        self.last_subtitle_update = None;
        self.active_captures.clear();
        self.capture_errors.clear();
        self.capture_transition_pending = true;
        self.stt_resume_generation = None;
        self.accepted_audio_generation = None;
        update.hide_overlay = true;
    }

    fn update_capture_state(&self) {
        let state = if self.pipeline_error.is_some()
            || self.pipewire_error.is_some()
            || !self.capture_errors.is_empty()
        {
            ApplicationState::Error
        } else if !self.running_requested || !self.stt_ready {
            ApplicationState::Starting
        } else if self.desired_capture_ids.is_empty()
            || self.capture_transition_pending
            || !self
                .desired_capture_ids
                .is_subset(&self.active_captures)
        {
            ApplicationState::Starting
        } else {
            ApplicationState::Running
        };
        self.set_state(state);
    }

    fn current_runtime_is_selected(&self, runtime_id: u32) -> bool {
        let controller = self.controller.borrow();
        let targets = controller.selected_targets();
        controller
            .streams()
            .iter()
            .find(|stream| stream.runtime_id == runtime_id)
            .is_some_and(|stream| targets.iter().any(|target| target.matches(stream)))
    }

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

    fn clear_stale_subtitle(&mut self, update: &mut RuntimeUpdate) {
        let Some(last_update) = self.last_subtitle_update else {
            return;
        };
        if last_update.elapsed() < SUBTITLE_IDLE_TIMEOUT {
            return;
        }
        self.last_subtitle_update = None;
        self.transcript.clear();
        self.stt.pause_audio();
        self.source_audio.clear();
        self.mixed_audio.clear();
        self.mixer.reset();
        self.capture_transition_pending = true;
        self.accepted_audio_generation = None;
        self.stt_resume_generation = Some(self.stt.resume_audio());
        update.hide_overlay = true;
    }

    fn sync_tray_state(&mut self) {
        let state = match self.state() {
            ApplicationState::Running => TrayState::Active,
            ApplicationState::Error => TrayState::Error,
            ApplicationState::Stopped
            | ApplicationState::Starting
            | ApplicationState::Stopping => TrayState::Paused,
        };
        if self.last_tray_state == Some(state) {
            return;
        }
        if let Some(tray) = &self.tray {
            tray.set_state(state);
        }
        self.last_tray_state = Some(state);
    }

    fn set_state(&self, state: ApplicationState) {
        self.controller.borrow_mut().set_state(state);
    }
}

fn mixed_queue_capacity(window_ms: u32) -> usize {
    let retained_ms = usize::try_from(window_ms)
        .unwrap_or(3_000)
        .saturating_add(INFERENCE_HEADROOM_MS);
    retained_ms.div_ceil(MIX_FRAME_DURATION_MS).clamp(64, 2_000)
}
