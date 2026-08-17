mod mixer;
mod queue;

use std::time::Instant;

pub use mixer::{MixerHandle, spawn_mixer};
pub use queue::LatestQueue;

pub const SAMPLE_RATE_HZ: u32 = 16_000;
pub const MIX_FRAME_SAMPLES: usize = 320;

#[derive(Debug)]
pub struct SourceAudioChunk {
    pub source_id: u32,
    pub samples: Vec<f32>,
    pub captured_at: Instant,
}

#[derive(Debug)]
pub struct MixedAudioChunk {
    pub samples: Vec<f32>,
    pub captured_at: Instant,
}
