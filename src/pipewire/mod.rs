//! backend สำหรับ native PipeWire graph discovery และ selected-stream capture

mod capture;
mod service;

pub use service::{PipeWireCommandSender, PipeWireService, spawn_service};
