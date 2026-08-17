use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::{LatestQueue, MIX_FRAME_SAMPLES, MixedAudioChunk, SourceAudioChunk};

const SOURCE_IDLE_TIMEOUT: Duration = Duration::from_millis(250);
const MIX_SYNC_GRACE: Duration = Duration::from_millis(30);

pub struct MixerHandle {
    reset_generation: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
}

impl MixerHandle {
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    pub fn reset(&self) {
        self.reset_generation.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for MixerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn spawn_mixer(
    input: LatestQueue<SourceAudioChunk>,
    output: LatestQueue<MixedAudioChunk>,
) -> MixerHandle {
    let running = Arc::new(AtomicBool::new(true));
    let worker_running = Arc::clone(&running);
    let reset_generation = Arc::new(AtomicU64::new(0));
    let worker_reset_generation = Arc::clone(&reset_generation);

    thread::Builder::new()
        .name("audio-mixer".to_owned())
        .spawn(move || {
            run_mixer(worker_running, worker_reset_generation, input, output);
        })
        .expect("failed to spawn audio mixer worker");

    MixerHandle {
        reset_generation,
        running,
    }
}

fn run_mixer(
    running: Arc<AtomicBool>,
    reset_generation: Arc<AtomicU64>,
    input: LatestQueue<SourceAudioChunk>,
    output: LatestQueue<MixedAudioChunk>,
) {
    let mut sources: HashMap<u32, SourceBuffer> = HashMap::new();
    let mut observed_reset = reset_generation.load(Ordering::Acquire);
    let mut frame_wait_started: Option<Instant> = None;
    let mut next_frame_at: Option<Instant> = None;

    while running.load(Ordering::Acquire) {
        let requested_reset = reset_generation.load(Ordering::Acquire);
        if requested_reset != observed_reset {
            sources.clear();
            frame_wait_started = None;
            next_frame_at = None;
            observed_reset = requested_reset;
        }

        for chunk in input.drain() {
            sources
                .entry(chunk.source_id)
                .or_insert_with(|| SourceBuffer::new(chunk.captured_at))
                .append(chunk.samples, chunk.captured_at);
        }

        let now = Instant::now();
        sources.retain(|_, source| {
            now.saturating_duration_since(source.last_seen) < SOURCE_IDLE_TIMEOUT
        });

        loop {
            let Some(earliest_sample) = sources
                .values()
                .filter(|source| !source.samples.is_empty())
                .map(|source| source.started_at)
                .min()
            else {
                frame_wait_started = None;
                break;
            };
            let mut frame_start = next_frame_at.unwrap_or(earliest_sample);
            let frame_duration = samples_duration(MIX_FRAME_SAMPLES);
            let mut frame_end = frame_start + frame_duration;
            let covering_sources = sources
                .values()
                .filter(|source| {
                    source.started_at < frame_end && source.buffered_until() >= frame_end
                })
                .count();
            if covering_sources == 0 && earliest_sample >= frame_end {
                frame_start = earliest_sample;
                frame_end = frame_start + frame_duration;
            }

            let covering_sources = sources
                .values()
                .filter(|source| {
                    source.started_at < frame_end && source.buffered_until() >= frame_end
                })
                .count();
            if covering_sources == 0 {
                break;
            }
            if covering_sources < sources.len() {
                let wait_started = frame_wait_started.get_or_insert_with(Instant::now);
                if wait_started.elapsed() < MIX_SYNC_GRACE {
                    break;
                }
            }
            if let Some(chunk) = mix_frame(&mut sources, frame_start) {
                output.push_latest(chunk);
            }
            next_frame_at = Some(frame_end);
            frame_wait_started = None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn mix_frame(
    sources: &mut HashMap<u32, SourceBuffer>,
    frame_start: Instant,
) -> Option<MixedAudioChunk> {
    let mut mixed = vec![0.0_f32; MIX_FRAME_SAMPLES];
    let mut contributing_sources = 0_u32;
    let frame_end = frame_start + samples_duration(MIX_FRAME_SAMPLES);

    for source in sources.values_mut() {
        source.discard_before(frame_start);
        if source.samples.is_empty() || source.started_at >= frame_end {
            continue;
        }

        let leading_samples = if source.started_at > frame_start {
            duration_samples(source.started_at.duration_since(frame_start))
                .min(MIX_FRAME_SAMPLES)
        } else {
            0
        };
        let available_samples = MIX_FRAME_SAMPLES.saturating_sub(leading_samples);
        let samples_to_mix = available_samples.min(source.samples.len());
        if samples_to_mix == 0 {
            continue;
        }
        contributing_sources += 1;
        for mixed_sample in mixed
            .iter_mut()
            .skip(leading_samples)
            .take(samples_to_mix)
        {
            *mixed_sample += source.samples.pop_front().unwrap_or_default();
        }
        source.advance_timestamp(samples_to_mix);
    }

    if contributing_sources == 0 {
        return None;
    }

    let divisor = contributing_sources as f32;
    for sample in &mut mixed {
        *sample = (*sample / divisor).clamp(-1.0, 1.0);
    }

    Some(MixedAudioChunk {
        samples: mixed,
        captured_at: frame_start,
    })
}

struct SourceBuffer {
    samples: VecDeque<f32>,
    started_at: Instant,
    last_seen: Instant,
}

impl SourceBuffer {
    fn new(captured_at: Instant) -> Self {
        Self {
            samples: VecDeque::new(),
            started_at: captured_at,
            last_seen: captured_at,
        }
    }

    fn append(&mut self, samples: Vec<f32>, captured_at: Instant) {
        if self.samples.is_empty() {
            self.started_at = captured_at;
        } else {
            let expected_at = self.buffered_until();
            if captured_at.saturating_duration_since(expected_at) > MIX_SYNC_GRACE {
                self.samples.clear();
                self.started_at = captured_at;
            }
        }
        self.last_seen = captured_at;
        self.samples.extend(samples);
    }

    fn buffered_until(&self) -> Instant {
        self.started_at + samples_duration(self.samples.len())
    }

    fn discard_before(&mut self, frame_start: Instant) {
        if self.started_at >= frame_start {
            return;
        }
        let samples_to_discard = duration_samples(frame_start.duration_since(self.started_at))
            .min(self.samples.len());
        self.samples.drain(..samples_to_discard);
        self.advance_timestamp(samples_to_discard);
    }

    fn advance_timestamp(&mut self, samples: usize) {
        self.started_at += samples_duration(samples);
    }
}

fn samples_duration(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / super::SAMPLE_RATE_HZ as f64)
}

fn duration_samples(duration: Duration) -> usize {
    (duration.as_secs_f64() * super::SAMPLE_RATE_HZ as f64).round() as usize
}
