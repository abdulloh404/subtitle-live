//! โครงสร้างพื้นฐานสำหรับส่งและผสมเสียงระหว่าง PipeWire capture กับ STT
//!
//! เสียงทุก sample ที่ผ่านขอบเขตนี้เป็น mono `f32` ที่ [`SAMPLE_RATE_HZ`]
//! ส่วน timestamp คือเวลารับโดยประมาณของ sample แรก เพื่อให้ขั้นตอนถัดไปวัด
//! latency สะสมได้โดยไม่ต้องเก็บหรือเขียนเสียงจริงลง log

mod mixer;
mod queue;

use std::{
    fmt,
    sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
    time::Instant,
};

pub use mixer::{MixerHandle, spawn_mixer};
pub use queue::LatestQueue;

/// Sample rate ที่ Whisper ต้องการและใช้ร้องขอจาก PipeWire
pub const SAMPLE_RATE_HZ: u32 = 16_000;
/// จำนวน sample ต่อหนึ่ง mixer frame ซึ่งเท่ากับ 20 ms ที่ 16 kHz
pub const MIX_FRAME_SAMPLES: usize = 320;

/// pool บัฟเฟอร์เสียงแบบ bounded ซึ่ง callback ขอใช้ได้โดยไม่รอ
///
/// บัฟเฟอร์ทั้งหมดถูกจองหน่วยความจำก่อนเริ่ม PipeWire stream ถ้า pool ว่าง
/// หรือ chunk ใหญ่กว่าความจุที่เตรียมไว้ callback ต้องทิ้งเสียงรอบนั้นทันที
pub(crate) struct AudioBufferPool {
    available: Receiver<Vec<f32>>,
    return_to: SyncSender<Vec<f32>>,
    max_samples: usize,
}

impl AudioBufferPool {
    /// สร้างบัฟเฟอร์ทุกก้อนล่วงหน้านอก real-time callback
    pub(crate) fn new(buffer_count: usize, max_samples: usize) -> Self {
        assert!(buffer_count > 0, "buffer count must be greater than zero");
        assert!(max_samples > 0, "buffer capacity must be greater than zero");

        let (return_to, available) = sync_channel(buffer_count);
        for _ in 0..buffer_count {
            return_to
                .try_send(Vec::with_capacity(max_samples))
                .expect("new audio buffer pool must have enough slots");
        }

        Self {
            available,
            return_to,
            max_samples,
        }
    }

    /// ขอ ownership ของบัฟเฟอร์โดยไม่บล็อก และไม่ขยาย heap ภายใน callback
    pub(crate) fn try_acquire(&self, required_samples: usize) -> Option<PooledAudioSamples> {
        if required_samples == 0 || required_samples > self.max_samples {
            return None;
        }

        match self.available.try_recv() {
            Ok(mut samples) => {
                samples.clear();
                Some(PooledAudioSamples {
                    samples: Some(samples),
                    return_to: self.return_to.clone(),
                })
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

/// ownership ของ sample ที่คืนหน่วยความจำกลับ pool อัตโนมัติเมื่อเลิกใช้
pub struct PooledAudioSamples {
    samples: Option<Vec<f32>>,
    return_to: SyncSender<Vec<f32>>,
}

impl PooledAudioSamples {
    /// เพิ่ม sample โดยผู้เรียกต้องตรวจความจุก่อนเพื่อไม่ให้ `Vec` ขยาย heap
    pub(crate) fn push(&mut self, sample: f32) {
        let samples = self.samples.as_mut().expect("audio buffer is owned");
        debug_assert!(samples.len() < samples.capacity());
        samples.push(sample);
    }

    /// คืนมุมมองแบบ slice ให้ mixer คัดลอกเข้าบัฟเฟอร์สะสมของ source
    pub(crate) fn as_slice(&self) -> &[f32] {
        self.samples.as_deref().unwrap_or_default()
    }

    /// คืนจำนวน sample จริงในก้อนนี้
    pub fn len(&self) -> usize {
        self.samples.as_ref().map_or(0, Vec::len)
    }

    /// ตรวจว่าก้อนนี้ไม่มี sample หรือไม่
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for PooledAudioSamples {
    fn drop(&mut self) {
        let Some(mut samples) = self.samples.take() else {
            return;
        };
        samples.clear();
        // `try_send` ไม่รอ consumer; กรณี pool ถูกปิดให้ Vec ถูกทำลายตามปกติ
        let _ = self.return_to.try_send(samples);
    }
}

impl fmt::Debug for PooledAudioSamples {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PooledAudioSamples")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// ก้อนเสียงมาตรฐานที่มาจาก PipeWire stream ที่เลือกหนึ่งรายการ
#[derive(Debug)]
pub struct SourceAudioChunk {
    /// รุ่น pipeline ที่ runtime กำหนด ใช้ปฏิเสธเสียงจาก capture รอบเก่า
    pub generation: u64,
    /// PipeWire node ID ของ session ปัจจุบัน ใช้แยก input ของ mixer เท่านั้น
    pub source_id: u32,
    /// sample mono `f32` ที่ normalize แล้วและห้ามเขียนค่าจริงลง log
    pub samples: PooledAudioSamples,
    /// เวลารับโดยประมาณของ sample แรกใน `samples`
    pub captured_at: Instant,
}

#[cfg(test)]
mod tests {
    use super::AudioBufferPool;

    #[test]
    fn pooled_buffer_returns_after_owner_is_dropped() {
        let pool = AudioBufferPool::new(1, 4);
        let samples = pool.try_acquire(4).expect("first buffer");

        assert!(pool.try_acquire(1).is_none());
        drop(samples);
        assert!(pool.try_acquire(1).is_some());
    }

    #[test]
    fn oversized_request_does_not_consume_pool_buffer() {
        let pool = AudioBufferPool::new(1, 4);

        assert!(pool.try_acquire(5).is_none());
        assert!(pool.try_acquire(4).is_some());
    }
}

/// frame ขนาดคงที่ที่รวมเสียงจาก source ที่ active ทั้งหมดแล้ว
#[derive(Debug)]
pub struct MixedAudioChunk {
    /// รุ่น pipeline เดียวกับ source ที่นำมาผสม
    pub generation: u64,
    /// sample mono ที่พร้อมส่งเข้า rolling buffer ของ STT
    pub samples: Vec<f32>,
    /// timestamp ของเสียงที่ส่งต่อมาจากจุดเริ่มต้นของ mix frame
    pub captured_at: Instant,
}
