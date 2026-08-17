//! Local whisper.cpp worker and application-facing STT events.

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

const EVENT_QUEUE_CAPACITY: usize = 64;
const STABLE_PASSES_TO_FINAL: u8 = 2;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SttStartConfig {
    pub model_path: PathBuf,
    pub language: String,
    pub step_ms: u32,
    pub window_ms: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptUpdate {
    Partial { segment_id: u64, text: String },
    Final { segment_id: u64, text: String },
}

impl TranscriptUpdate {
    pub const fn segment_id(&self) -> u64 {
        match self {
            Self::Partial { segment_id, .. } | Self::Final { segment_id, .. } => *segment_id,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Self::Partial { text, .. } | Self::Final { text, .. } => text,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SttEvent {
    Loading,
    Ready,
    Transcript {
        audio_generation: u64,
        update: TranscriptUpdate,
    },
    Metrics {
        audio_generation: u64,
        inference_duration: Duration,
        approximate_total: Duration,
    },
    Stopped,
    Error(String),
}

pub struct SttService {
    commands: Sender<WorkerCommand>,
    events: LatestQueue<SttEvent>,
    audio_generation: Arc<AtomicU64>,
    requested_audio_generation: AtomicU64,
}

impl SttService {
    pub fn start(&self, config: SttStartConfig) {
        let _ = self.commands.send(WorkerCommand::Start(config));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(WorkerCommand::Stop);
    }

    pub fn pause_audio(&self) {
        let _ = self.commands.send(WorkerCommand::PauseAudio);
    }

    pub fn resume_audio(&self) -> u64 {
        let generation = self
            .requested_audio_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let _ = self
            .commands
            .send(WorkerCommand::ResumeAudio(generation));
        generation
    }

    pub fn audio_generation(&self) -> u64 {
        self.audio_generation.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        let _ = self.commands.send(WorkerCommand::Shutdown);
    }

    pub fn drain_events(&self) -> Vec<SttEvent> {
        self.events.drain()
    }
}

impl Drop for SttService {
    fn drop(&mut self) {
        let _ = self.commands.send(WorkerCommand::Shutdown);
    }
}

pub fn spawn_service(input: LatestQueue<MixedAudioChunk>) -> SttService {
    let (commands, command_receiver) = mpsc::channel();
    let events = LatestQueue::new(EVENT_QUEUE_CAPACITY);
    let worker_events = events.clone();
    let audio_generation = Arc::new(AtomicU64::new(0));
    let worker_generation = Arc::clone(&audio_generation);
    thread::Builder::new()
        .name("whisper-stt".to_owned())
        .spawn(move || run_worker(input, command_receiver, worker_events, worker_generation))
        .expect("failed to spawn STT worker");

    SttService {
        commands,
        events,
        audio_generation,
        requested_audio_generation: AtomicU64::new(0),
    }
}

enum WorkerCommand {
    Start(SttStartConfig),
    Stop,
    PauseAudio,
    ResumeAudio(u64),
    Shutdown,
}

fn run_worker(
    input: LatestQueue<MixedAudioChunk>,
    commands: Receiver<WorkerCommand>,
    events: LatestQueue<SttEvent>,
    audio_generation: Arc<AtomicU64>,
) {
    let mut active: Option<ActiveStt> = None;
    let mut audio_paused = false;
    let mut current_audio_generation = 0;

    loop {
        match next_command(&commands, active.is_some()) {
            CommandState::Command(WorkerCommand::Start(config)) => {
                active = None;
                audio_paused = false;
                current_audio_generation = 0;
                input.clear();
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
                audio_paused = false;
                current_audio_generation = 0;
                input.clear();
                events.push_latest_reliable(SttEvent::Stopped);
            }
            CommandState::Command(WorkerCommand::PauseAudio) => {
                audio_paused = true;
                input.clear();
                if let Some(stt) = active.as_mut() {
                    stt.reset_discontinuity();
                }
            }
            CommandState::Command(WorkerCommand::ResumeAudio(generation)) => {
                input.clear();
                if let Some(stt) = active.as_mut() {
                    stt.reset_discontinuity();
                    audio_paused = false;
                    current_audio_generation = generation;
                    audio_generation.store(generation, Ordering::Release);
                }
            }
            CommandState::Command(WorkerCommand::Shutdown) | CommandState::Disconnected => break,
            CommandState::Idle => {}
        }

        let Some(stt) = active.as_mut() else {
            continue;
        };
        if audio_paused {
            continue;
        }

        match stt.consume_latest(&input) {
            Ok(Some(result)) => {
                if let Some(update) = result.update {
                    events.push_latest(SttEvent::Transcript {
                        audio_generation: current_audio_generation,
                        update,
                    });
                }
                events.push_latest(SttEvent::Metrics {
                    audio_generation: current_audio_generation,
                    inference_duration: result.inference_duration,
                    approximate_total: result.approximate_total,
                });
            }
            Ok(None) => {}
            Err(error) => {
                active = None;
                input.clear();
                events.push_latest_reliable(SttEvent::Error(error));
                events.push_latest_reliable(SttEvent::Stopped);
            }
        }
    }
}

enum CommandState {
    Command(WorkerCommand),
    Idle,
    Disconnected,
}

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

struct ActiveStt {
    state: WhisperState,
    config: SttStartConfig,
    rolling_audio: VecDeque<f32>,
    window_samples: usize,
    step_samples: usize,
    samples_since_inference: usize,
    latest_audio_at: Option<Instant>,
    observed_dropped: u64,
    segment_id: u64,
    previous_hypothesis: String,
    stable_passes: u8,
}

struct InferenceResult {
    update: Option<TranscriptUpdate>,
    inference_duration: Duration,
    approximate_total: Duration,
}

impl ActiveStt {
    fn load(config: SttStartConfig, observed_dropped: u64) -> Result<Self, String> {
        validate_config(&config)?;
        File::open(&config.model_path).map_err(|error| {
            format!(
                "Whisper model is not readable at {}: {error}",
                config.model_path.display()
            )
        })?;

        let context = WhisperContext::new_with_params(
            &config.model_path,
            WhisperContextParameters::default(),
        )
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
            rolling_audio: VecDeque::with_capacity(window_samples),
            window_samples,
            step_samples,
            samples_since_inference: 0,
            latest_audio_at: None,
            observed_dropped,
            segment_id: 1,
            previous_hypothesis: String::new(),
            stable_passes: 0,
        })
    }

    fn consume_latest(
        &mut self,
        input: &LatestQueue<MixedAudioChunk>,
    ) -> Result<Option<InferenceResult>, String> {
        let dropped = input.dropped();
        if dropped != self.observed_dropped {
            self.reset_discontinuity();
            self.observed_dropped = dropped;
        }

        let chunks = input.drain();
        if chunks.is_empty() {
            return Ok(None);
        }

        for chunk in chunks {
            let sample_duration =
                Duration::from_secs_f64(chunk.samples.len() as f64 / SAMPLE_RATE_HZ as f64);
            self.latest_audio_at = Some(chunk.captured_at + sample_duration);
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
        self.samples_since_inference = 0;

        let audio = self.rolling_audio.make_contiguous();
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.config.language));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        let inference_started = Instant::now();
        self.state
            .full(params, audio)
            .map_err(|error| format!("Whisper inference failed: {error}"))?;
        let inference_duration = inference_started.elapsed();
        let approximate_total = self
            .latest_audio_at
            .map_or(inference_duration, |captured_at| {
                Instant::now().saturating_duration_since(captured_at)
            });
        let hypothesis = collect_hypothesis(&self.state)?;

        Ok(Some(InferenceResult {
            update: self.reconcile_hypothesis(hypothesis),
            inference_duration,
            approximate_total,
        }))
    }

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

    fn reset_audio(&mut self) {
        self.rolling_audio.clear();
        self.samples_since_inference = 0;
        self.latest_audio_at = None;
    }

    fn reset_discontinuity(&mut self) {
        self.reset_audio();
        self.previous_hypothesis.clear();
        self.stable_passes = 0;
        self.segment_id = self.segment_id.saturating_add(1);
    }
}

fn validate_config(config: &SttStartConfig) -> Result<(), String> {
    if config.language != "en" {
        return Err(format!(
            "Unsupported STT language '{}'; Phase 1 supports English ('en') only",
            config.language
        ));
    }
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

fn milliseconds_to_samples(milliseconds: u32) -> usize {
    (u64::from(milliseconds) * u64::from(SAMPLE_RATE_HZ) / 1_000) as usize
}

fn collect_hypothesis(state: &WhisperState) -> Result<String, String> {
    let mut text = String::new();
    for segment in state.as_iter() {
        let segment = segment
            .to_str_lossy()
            .map_err(|error| format!("Failed to read Whisper transcript: {error}"))?;
        text.push_str(&segment);
    }
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}
