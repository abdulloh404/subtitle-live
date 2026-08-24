//! Facade นี้ทำให้ application code ไม่ขึ้นกับ audio backend ที่เลือก

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    audio::{LatestQueue, SourceAudioChunk},
    pipewire::{PipeWireCommandSender, PipeWireService},
};

use super::{
    AudioSourceEvent, CaptureTarget,
    pulse::{PulseAudioCommandSender, PulseAudioService},
};

/// implementation ของ audio graph และ capture ที่เลือกตอนเริ่ม service
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioSourceBackend {
    /// PipeWire ใช้ native graph discovery และ capture application stream ที่เลือก
    #[default]
    #[serde(rename = "pipewire", alias = "pipe_wire")]
    PipeWire,
    /// monitor ของ system output ค่าเริ่มต้นผ่าน PulseAudio-compatible utilities
    #[serde(rename = "pulse", alias = "pulse_audio")]
    PulseAudio,
}

impl AudioSourceBackend {
    /// identifier คงที่ที่เก็บในไฟล์ config
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PipeWire => "pipewire",
            Self::PulseAudio => "pulse",
        }
    }

    /// ชื่อ backend ที่อ่านง่ายสำหรับใช้ใน Settings และ diagnostics
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::PipeWire => "PipeWire",
            Self::PulseAudio => "PulseAudio",
        }
    }

    /// คำอธิบาย capability แบบสั้นข้าง backend selector
    pub const fn description(self) -> &'static str {
        match self {
            Self::PipeWire => "Select individual applications or playback streams",
            Self::PulseAudio => "Capture the default system output as one source",
        }
    }
}

#[derive(Clone)]
enum CommandSenderInner {
    PipeWire(PipeWireCommandSender),
    PulseAudio(PulseAudioCommandSender),
}

/// command endpoint ที่ clone ได้และไม่ขึ้นกับ backend
#[derive(Clone)]
pub struct AudioSourceCommandSender {
    inner: CommandSenderInner,
}

impl AudioSourceCommandSender {
    pub fn set_selected(
        &self,
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    ) -> Result<(), AudioSourceServiceError> {
        match &self.inner {
            CommandSenderInner::PipeWire(commands) => commands
                .set_selected(targets, audio_generation)
                .map_err(AudioSourceServiceError::backend),
            CommandSenderInner::PulseAudio(commands) => commands
                .set_selected(targets, audio_generation)
                .map_err(AudioSourceServiceError::backend),
        }
    }

    pub fn stop_capture(&self) -> Result<(), AudioSourceServiceError> {
        match &self.inner {
            CommandSenderInner::PipeWire(commands) => commands
                .stop_capture()
                .map_err(AudioSourceServiceError::backend),
            CommandSenderInner::PulseAudio(commands) => commands
                .stop_capture()
                .map_err(AudioSourceServiceError::backend),
        }
    }

    pub fn shutdown(&self) -> Result<(), AudioSourceServiceError> {
        match &self.inner {
            CommandSenderInner::PipeWire(commands) => commands
                .shutdown()
                .map_err(AudioSourceServiceError::backend),
            CommandSenderInner::PulseAudio(commands) => commands
                .shutdown()
                .map_err(AudioSourceServiceError::backend),
        }
    }
}

enum ServiceInner {
    PipeWire(PipeWireService),
    PulseAudio(PulseAudioService),
}

/// เป็นเจ้าของ audio backend หนึ่งตัวและให้ API คงที่แก่ application runtime
pub struct AudioSourceService {
    backend: AudioSourceBackend,
    inner: ServiceInner,
}

impl AudioSourceService {
    pub fn backend(&self) -> AudioSourceBackend {
        self.backend
    }

    pub fn command_sender(&self) -> AudioSourceCommandSender {
        let inner = match &self.inner {
            ServiceInner::PipeWire(service) => {
                CommandSenderInner::PipeWire(service.command_sender())
            }
            ServiceInner::PulseAudio(service) => {
                CommandSenderInner::PulseAudio(service.command_sender())
            }
        };
        AudioSourceCommandSender { inner }
    }

    pub fn set_selected(
        &self,
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    ) -> Result<(), AudioSourceServiceError> {
        self.command_sender()
            .set_selected(targets, audio_generation)
    }

    pub fn stop_capture(&self) -> Result<(), AudioSourceServiceError> {
        self.command_sender().stop_capture()
    }

    pub fn shutdown(&self) -> Result<(), AudioSourceServiceError> {
        self.command_sender().shutdown()
    }

    pub fn finish(&mut self) -> Result<(), String> {
        match &mut self.inner {
            ServiceInner::PipeWire(service) => service.finish(),
            ServiceInner::PulseAudio(service) => service.finish(),
        }
    }

    /// method นี้ไม่รอ backend thread
    pub fn is_finished(&self) -> bool {
        match &self.inner {
            ServiceInner::PipeWire(service) => service.is_finished(),
            ServiceInner::PulseAudio(service) => service.is_finished(),
        }
    }

    /// นำ event ที่ค้างใน queue ออกทั้งหมดโดยไม่รอ event ใหม่
    pub fn drain_events(&self) -> Vec<AudioSourceEvent> {
        match &self.inner {
            ServiceInner::PipeWire(service) => service.drain_events(),
            ServiceInner::PulseAudio(service) => service.drain_events(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSourceServiceError {
    message: String,
}

impl AudioSourceServiceError {
    fn backend(error: impl fmt::Display) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl fmt::Display for AudioSourceServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AudioSourceServiceError {}

/// เริ่ม backend ที่เลือกและคืนค่าทันที
pub fn spawn_service(
    backend: AudioSourceBackend,
    audio: LatestQueue<SourceAudioChunk>,
) -> AudioSourceService {
    let inner = match backend {
        AudioSourceBackend::PipeWire => {
            ServiceInner::PipeWire(crate::pipewire::spawn_service(audio))
        }
        AudioSourceBackend::PulseAudio => {
            ServiceInner::PulseAudio(super::pulse::spawn_service(audio))
        }
    };
    AudioSourceService { backend, inner }
}

#[cfg(test)]
mod tests {
    use super::AudioSourceBackend;

    #[test]
    fn pipewire_is_the_default_backend() {
        assert_eq!(AudioSourceBackend::default(), AudioSourceBackend::PipeWire);
    }

    #[test]
    fn backend_names_are_stable_in_configuration() {
        assert_eq!(
            serde_json::to_string(&AudioSourceBackend::PulseAudio).unwrap(),
            "\"pulse\""
        );
    }
}
