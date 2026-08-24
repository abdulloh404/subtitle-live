//! backend สำหรับ native PipeWire graph discovery และ selected-stream capture

mod capture;
mod service;

pub use service::{PipeWireCommandSender, PipeWireService, PipeWireServiceError, spawn_service};
