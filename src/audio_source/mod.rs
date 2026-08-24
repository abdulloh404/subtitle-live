//! โมดูลนี้ทำ audio source discovery และ capture โดยไม่ขึ้นกับ backend
//!
//! PipeWire เป็น backend ค่าเริ่มต้นและรองรับ application stream ที่เลือก ส่วน PulseAudio
//! fallback จะ capture เฉพาะ monitor ของ system output ค่าเริ่มต้น

mod pulse;
mod service;
mod types;

pub use service::{
    AudioSourceBackend, AudioSourceCommandSender, AudioSourceService, AudioSourceServiceError,
    spawn_service,
};
pub use types::{
    ApplicationIdentity, ApplicationKey, AudioSourceEvent, CaptureTarget, StreamDiscriminator,
    StreamInfo,
};
