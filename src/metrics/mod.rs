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

/// ชุดตัวอย่างดิบของเวลาการอนุมานและเวลารวมโดยประมาณ
#[derive(Default)]
struct LatencySamples {
    /// เวลาเฉพาะช่วงที่ Whisper ประมวลผลเสียง
    inference: VecDeque<Duration>,
    /// เวลารวมโดยประมาณตั้งแต่เสียงล่าสุดจนสร้างผล STT เสร็จ
    total: VecDeque<Duration>,
}

/// ภาพรวมล่าสุดสำหรับแสดงใน UI โดยไม่มีการเปิดเผยเนื้อหาเสียงหรือคำบรรยาย
#[derive(Clone, Copy, Debug, Default)]
pub struct MetricsSnapshot {
    /// เวลาอนุมานของรอบล่าสุด
    pub inference_latest: Option<Duration>,
    /// ค่ากลางของเวลาอนุมานในหน้าต่างตัวอย่างล่าสุด
    pub inference_p50: Option<Duration>,
    /// เปอร์เซ็นไทล์ที่ 95 ของเวลาอนุมานในหน้าต่างตัวอย่างล่าสุด
    pub inference_p95: Option<Duration>,
    /// เวลารวมโดยประมาณของรอบล่าสุด
    pub total_latest: Option<Duration>,
    /// ค่ากลางของเวลารวมโดยประมาณ
    pub total_p50: Option<Duration>,
    /// เปอร์เซ็นไทล์ที่ 95 ของเวลารวมโดยประมาณ
    pub total_p95: Option<Duration>,
    /// จำนวนชิ้นเสียงจากแหล่งต้นทางที่คิวจำเป็นต้องทิ้ง
    pub source_queue_dropped: u64,
    /// จำนวนชิ้นเสียงหลังผสมที่คิวจำเป็นต้องทิ้ง
    pub mixed_queue_dropped: u64,
}

impl LatencyTracker {
    /// เพิ่มผลจากการอนุมานหนึ่งรอบลงในหน้าต่างตัวอย่างแบบมีขอบเขต
    pub fn record(&self, inference: Duration, total: Duration) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        push_bounded(&mut inner.inference, inference);
        push_bounded(&mut inner.total, total);
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
            inference_latest: inner.inference.back().copied(),
            inference_p50: percentile(&inner.inference, 50),
            inference_p95: percentile(&inner.inference, 95),
            total_latest: inner.total.back().copied(),
            total_p50: percentile(&inner.total, 50),
            total_p95: percentile(&inner.total, 95),
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
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted.get(index).copied()
}
