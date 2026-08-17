//! ขอบเขตสำหรับค้นหา graph ของ PipeWire และ capture เฉพาะ stream ที่เลือก

mod capture;
mod service;
mod types;

pub use service::{PipeWireCommandSender, PipeWireService, PipeWireServiceError, spawn_service};
pub use types::{
    ApplicationIdentity, ApplicationKey, CaptureTarget, PipeWireEvent, StreamDiscriminator,
    StreamInfo,
};
