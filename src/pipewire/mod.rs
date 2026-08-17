//! PipeWire graph discovery and selected-stream capture boundary.

mod capture;
mod service;
mod types;

pub use service::{PipeWireCommandSender, PipeWireService, PipeWireServiceError, spawn_service};
pub use types::{
    ApplicationIdentity, ApplicationKey, CaptureTarget, PipeWireEvent, StreamDiscriminator,
    StreamInfo,
};
