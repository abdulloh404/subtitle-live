//! mixer ที่จัดแนวเสียงจาก application stream ที่เลือกด้วย timestamp
//!
//! worker จะจัด source buffer เป็น frame คงที่ขนาด 20 ms รอ source ที่ active
//! ชั่วครู่ให้เวลาเสียงตรงกัน เฉลี่ยระดับเสียงเพื่อป้องกัน clipping และส่ง frame
//! ล่าสุดต่อไปโดยไม่ขวาง capture callback

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

/// นำ source ออกจากชุดที่กำลังผสมเมื่อไม่มีเสียงนานเกินค่านี้
const SOURCE_IDLE_TIMEOUT: Duration = Duration::from_millis(250);
/// เวลารอสูงสุดให้ source อื่นมีข้อมูลครอบคลุม frame เดียวกัน
const MIX_SYNC_GRACE: Duration = Duration::from_millis(30);
/// ตัวควบคุมอายุและการ reset สถานะของ mixer worker เบื้องหลัง
pub struct MixerHandle {
    /// generation ที่เพิ่มขึ้นทุกครั้งเมื่อ caller ขอ reset
    reset_generation: Arc<AtomicU64>,
    /// รุ่นเสียงปัจจุบันที่ mixer อนุญาตให้ผ่านไปยัง STT
    audio_generation: Arc<AtomicU64>,
    /// flag สำหรับขอให้ worker ออกจาก loop
    running: Arc<AtomicBool>,
}

impl MixerHandle {
    /// ขอให้ worker หยุดอย่างร่วมมือโดยไม่บล็อกผู้เรียก
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// ล้าง source buffer หลังเปลี่ยนรายการ capture หรือเริ่ม pipeline ใหม่
    pub fn reset(&self, generation: u64) {
        if generation < self.audio_generation.load(Ordering::Acquire) {
            return;
        }
        self.audio_generation.store(generation, Ordering::Release);
        self.reset_generation.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for MixerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// เริ่ม mixer thread แยกต่างหากและคืน handle สำหรับควบคุม
pub fn spawn_mixer(
    input: LatestQueue<SourceAudioChunk>,
    output: LatestQueue<MixedAudioChunk>,
) -> MixerHandle {
    let running = Arc::new(AtomicBool::new(true));
    let worker_running = Arc::clone(&running);
    let reset_generation = Arc::new(AtomicU64::new(0));
    let worker_reset_generation = Arc::clone(&reset_generation);
    let audio_generation = Arc::new(AtomicU64::new(0));
    let worker_audio_generation = Arc::clone(&audio_generation);

    thread::Builder::new()
        .name("audio-mixer".to_owned())
        .spawn(move || {
            run_mixer(
                worker_running,
                worker_reset_generation,
                worker_audio_generation,
                input,
                output,
            );
        })
        .expect("failed to spawn audio mixer worker");

    MixerHandle {
        reset_generation,
        audio_generation,
        running,
    }
}

fn run_mixer(
    running: Arc<AtomicBool>,
    reset_generation: Arc<AtomicU64>,
    audio_generation: Arc<AtomicU64>,
    input: LatestQueue<SourceAudioChunk>,
    output: LatestQueue<MixedAudioChunk>,
) {
    // `next_frame_at` รักษา timeline ของ output ให้ต่อเนื่องข้าม callback ส่วน
    // timestamp ของ source ใช้ตัดสินว่าข้อมูลแต่ละก้อนอยู่ตำแหน่งใดบน timeline
    let mut sources: HashMap<u32, SourceBuffer> = HashMap::new();
    let mut observed_reset = reset_generation.load(Ordering::Acquire);
    let mut current_generation = audio_generation.load(Ordering::Acquire);
    let mut frame_wait_started: Option<Instant> = None;
    let mut next_frame_at: Option<Instant> = None;
    while running.load(Ordering::Acquire) {
        let requested_reset = reset_generation.load(Ordering::Acquire);
        if requested_reset != observed_reset {
            sources.clear();
            frame_wait_started = None;
            next_frame_at = None;
            observed_reset = requested_reset;
            current_generation = audio_generation.load(Ordering::Acquire);
        }

        for chunk in input.drain() {
            if !generation_is_current(chunk.generation, current_generation) {
                continue;
            }
            sources
                .entry(chunk.source_id)
                .or_insert_with(|| SourceBuffer::new(chunk.captured_at))
                .append(chunk.samples.as_slice(), chunk.captured_at);
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
            if let Some(chunk) = mix_frame(&mut sources, frame_start, current_generation) {
                output.push_latest(chunk);
            }
            next_frame_at = Some(frame_end);
            frame_wait_started = None;
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/// สร้างหนึ่ง frame ที่ `frame_start` โดยใช้เฉพาะ sample ที่มีเวลาซ้อนกับ frame
fn mix_frame(
    sources: &mut HashMap<u32, SourceBuffer>,
    frame_start: Instant,
    generation: u64,
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

    // การเฉลี่ยรักษา headroom เมื่อหลาย application มีเสียงพร้อมกัน
    let divisor = contributing_sources as f32;
    for sample in &mut mixed {
        *sample = (*sample / divisor).clamp(-1.0, 1.0);
    }

    Some(MixedAudioChunk {
        generation,
        samples: mixed,
        captured_at: frame_start,
    })
}

/// sample ที่พักไว้และ timeline ของ PipeWire node หนึ่งรายการใน session ปัจจุบัน
struct SourceBuffer {
    /// sample ที่ยังไม่ถูกนำไปผสม เรียงตามเวลา
    samples: VecDeque<f32>,
    /// timestamp ของ sample แรกใน `samples`
    started_at: Instant,
    /// timestamp ล่าสุดที่ได้รับข้อมูลจาก source
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

    fn append(&mut self, samples: &[f32], captured_at: Instant) {
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

/// รับเฉพาะเสียงของรุ่นที่ runtime เปิดใช้งานอยู่เท่านั้น
const fn generation_is_current(generation: u64, current_generation: u64) -> bool {
    generation == current_generation
}

#[cfg(test)]
mod tests {
    use super::generation_is_current;

    #[test]
    fn accepts_current_generation_and_discards_stale_generation() {
        assert!(generation_is_current(4, 4));
        assert!(!generation_is_current(3, 4));
    }
}
