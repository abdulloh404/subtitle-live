//! การสะสมและสรุปค่าหน่วงเวลาของกระบวนการสำหรับหน้าประสิทธิภาพ

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

// เก็บรอบล่าสุดแบบมีขอบเขตเพื่อให้เปอร์เซ็นไทล์สะท้อนสภาพปัจจุบันและใช้หน่วยความจำคงที่
const SAMPLE_CAPACITY: usize = 256;

/// ตัวบันทึกค่าหน่วงเวลาที่ใช้ร่วมกับตัวทำงานหลักได้อย่างปลอดภัย
#[derive(Clone, Default)]
pub struct LatencyTracker {
    inner: Arc<Mutex<LatencySamples>>,
}

/// ชุดตัวอย่างดิบของเวลาในแต่ละช่วงของ pipeline
#[derive(Default)]
struct LatencySamples {
    /// เวลาที่เสียงค้างใน buffer ก่อนเริ่มถอดความ
    audio_buffer: VecDeque<Duration>,
    /// เวลาเฉพาะช่วงที่ Whisper ประมวลผลเสียง
    whisper: VecDeque<Duration>,
    /// เวลาจากต้น audio step จน overlay ผ่านช่วง paint
    end_to_end: VecDeque<Duration>,
}

/// ภาพรวมล่าสุดสำหรับแสดงใน UI โดยไม่มีการเปิดเผยเนื้อหาเสียงหรือคำบรรยาย
#[derive(Clone, Copy, Debug, Default)]
pub struct MetricsSnapshot {
    /// เวลารอใน audio buffer ของรอบล่าสุด
    pub audio_buffer_latest: Option<Duration>,
    /// ค่า p50 ของเวลารอใน audio buffer
    pub audio_buffer_p50: Option<Duration>,
    /// ค่า p95 ของเวลารอใน audio buffer
    pub audio_buffer_p95: Option<Duration>,
    /// เวลาประมวลผลของ Whisper ในรอบล่าสุด
    pub whisper_latest: Option<Duration>,
    /// ค่า p50 ของเวลาประมวลผล Whisper
    pub whisper_p50: Option<Duration>,
    /// ค่า p95 ของเวลาประมวลผล Whisper
    pub whisper_p95: Option<Duration>,
    /// เวลา end-to-end จากต้น audio step ของรอบล่าสุด
    pub end_to_end_latest: Option<Duration>,
    /// ค่า p50 ของเวลา end-to-end
    pub end_to_end_p50: Option<Duration>,
    /// ค่า p95 ของเวลา end-to-end
    pub end_to_end_p95: Option<Duration>,
    /// จำนวนชิ้นเสียงจากแหล่งต้นทางที่คิวจำเป็นต้องทิ้ง
    pub source_queue_dropped: u64,
    /// จำนวนชิ้นเสียงหลังผสมที่คิวจำเป็นต้องทิ้ง
    pub mixed_queue_dropped: u64,
}

impl LatencyTracker {
    /// เก็บเวลา buffer และ Whisper จากการถอดความหนึ่งรอบ
    pub fn record_stt(&self, audio_buffer: Duration, whisper: Duration) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        push_bounded(&mut inner.audio_buffer, audio_buffer);
        push_bounded(&mut inner.whisper, whisper);
    }

    /// เก็บเวลารวมหลัง overlay ยืนยันการแสดงผลแล้ว
    pub fn record_end_to_end(&self, duration: Duration) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        push_bounded(&mut inner.end_to_end, duration);
    }

    /// คำนวณค่าล่าสุด p50 และ p95 พร้อมตัวนับการทิ้งคิวโดยไม่รอหากมีผู้ใช้ล็อกอยู่
    pub fn snapshot(&self, source_dropped: u64, mixed_dropped: u64) -> MetricsSnapshot {
        let Ok(inner) = self.inner.try_lock() else {
            return MetricsSnapshot {
                source_queue_dropped: source_dropped,
                mixed_queue_dropped: mixed_dropped,
                ..MetricsSnapshot::default()
            };
        };

        MetricsSnapshot {
            audio_buffer_latest: inner.audio_buffer.back().copied(),
            audio_buffer_p50: percentile(&inner.audio_buffer, 50),
            audio_buffer_p95: percentile(&inner.audio_buffer, 95),
            whisper_latest: inner.whisper.back().copied(),
            whisper_p50: percentile(&inner.whisper, 50),
            whisper_p95: percentile(&inner.whisper, 95),
            end_to_end_latest: inner.end_to_end.back().copied(),
            end_to_end_p50: percentile(&inner.end_to_end, 50),
            end_to_end_p95: percentile(&inner.end_to_end, 95),
            source_queue_dropped: source_dropped,
            mixed_queue_dropped: mixed_dropped,
        }
    }
}

/// เพิ่มค่าท้ายคิวและลบค่าที่เก่าสุดเมื่อหน้าต่างเต็ม
fn push_bounded(values: &mut VecDeque<Duration>, value: Duration) {
    if values.len() == SAMPLE_CAPACITY {
        values.pop_front();
    }
    values.push_back(value);
}

/// คำนวณเปอร์เซ็นไทล์แบบอันดับใกล้สุดจากสำเนาที่เรียงแล้ว โดยไม่เปลี่ยนลำดับเวลาต้นฉบับ
fn percentile(values: &VecDeque<Duration>, percentile: usize) -> Option<Duration> {
    if values.is_empty() {
        return None;
    }
    let mut sorted: Vec<_> = values.iter().copied().collect();
    sorted.sort_unstable();
    let index = (sorted.len() * percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ตรวจว่าการคำนวณเปอร์เซ็นไทล์ไม่ขึ้นกับลำดับที่บันทึกตัวอย่าง
    #[test]
    fn percentile_uses_sorted_nearest_rank() {
        let values = VecDeque::from([
            Duration::from_millis(40),
            Duration::from_millis(10),
            Duration::from_millis(30),
            Duration::from_millis(20),
        ]);

        assert_eq!(percentile(&values, 50), Some(Duration::from_millis(20)));
        assert_eq!(percentile(&values, 95), Some(Duration::from_millis(40)));
    }

    /// ตรวจว่าการบันทึก STT และ end-to-end ไม่เขียนทับชุดตัวอย่างของกัน
    #[test]
    fn latency_groups_are_recorded_independently() {
        let tracker = LatencyTracker::default();
        tracker.record_stt(Duration::from_millis(120), Duration::from_millis(45));

        let stt_snapshot = tracker.snapshot(2, 3);
        assert_eq!(
            stt_snapshot.audio_buffer_latest,
            Some(Duration::from_millis(120))
        );
        assert_eq!(
            stt_snapshot.whisper_latest,
            Some(Duration::from_millis(45))
        );
        assert_eq!(stt_snapshot.end_to_end_latest, None);

        tracker.record_end_to_end(Duration::from_millis(210));
        let complete_snapshot = tracker.snapshot(2, 3);
        assert_eq!(
            complete_snapshot.end_to_end_latest,
            Some(Duration::from_millis(210))
        );
        assert_eq!(complete_snapshot.audio_buffer_p50, Some(Duration::from_millis(120)));
        assert_eq!(complete_snapshot.whisper_p50, Some(Duration::from_millis(45)));
        assert_eq!(complete_snapshot.source_queue_dropped, 2);
        assert_eq!(complete_snapshot.mixed_queue_dropped, 3);
    }

    /// ตรวจว่าทุกชุดตัวอย่างเก็บเฉพาะ 256 รอบล่าสุด
    #[test]
    fn latency_groups_keep_a_bounded_window() {
        let tracker = LatencyTracker::default();
        for value in 0..=SAMPLE_CAPACITY {
            let duration = Duration::from_millis(value as u64);
            tracker.record_stt(duration, duration);
            tracker.record_end_to_end(duration);
        }

        let inner = tracker.inner.lock().expect("ล็อกชุดตัวอย่างได้");
        assert_eq!(inner.audio_buffer.len(), SAMPLE_CAPACITY);
        assert_eq!(inner.whisper.len(), SAMPLE_CAPACITY);
        assert_eq!(inner.end_to_end.len(), SAMPLE_CAPACITY);
        assert_eq!(inner.audio_buffer.front(), Some(&Duration::from_millis(1)));
        assert_eq!(inner.whisper.front(), Some(&Duration::from_millis(1)));
        assert_eq!(inner.end_to_end.front(), Some(&Duration::from_millis(1)));
    }
}
