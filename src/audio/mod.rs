//! โครงสร้างพื้นฐานสำหรับส่งและผสมเสียงระหว่าง PipeWire capture กับ STT
//!
//! เสียงทุก sample ที่ผ่านขอบเขตนี้เป็น mono `f32` ที่ [`SAMPLE_RATE_HZ`]
//! ส่วน timestamp คือเวลารับโดยประมาณของ sample แรก เพื่อให้ขั้นตอนถัดไปวัด
//! latency สะสมได้โดยไม่ต้องเก็บหรือเขียนเสียงจริงลง log

mod mixer;
mod queue;

use std::time::Instant;

pub use mixer::{MixerHandle, spawn_mixer};
pub use queue::LatestQueue;

/// Sample rate ที่ Whisper ต้องการและใช้ร้องขอจาก PipeWire
pub const SAMPLE_RATE_HZ: u32 = 16_000;
/// จำนวน sample ต่อหนึ่ง mixer frame ซึ่งเท่ากับ 20 ms ที่ 16 kHz
pub const MIX_FRAME_SAMPLES: usize = 320;

/// ก้อนเสียงมาตรฐานที่มาจาก PipeWire stream ที่เลือกหนึ่งรายการ
#[derive(Debug)]
pub struct SourceAudioChunk {
    /// รุ่น pipeline ที่ runtime กำหนด ใช้ปฏิเสธเสียงจาก capture รอบเก่า
    pub generation: u64,
    /// PipeWire node ID ของ session ปัจจุบัน ใช้แยก input ของ mixer เท่านั้น
    pub source_id: u32,
    /// sample mono `f32` ที่ normalize แล้วและห้ามเขียนค่าจริงลง log
    pub samples: Vec<f32>,
    /// เวลารับโดยประมาณของ sample แรกใน `samples`
    pub captured_at: Instant,
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
