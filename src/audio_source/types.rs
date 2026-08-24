//! โมดูลนี้กำหนด audio source identity, selection rule และ runtime event โดยไม่ขึ้นกับ backend

use serde::{Deserialize, Serialize};

/// application metadata คงที่ที่เรียงตามความน่าเชื่อถือของ identity จากมากไปน้อย
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApplicationIdentity {
    /// desktop application ID ที่ source รายงาน หากมี
    pub application_id: Option<String>,
    /// process binary ที่ใช้เมื่อไม่มี application ID
    pub process_binary: Option<String>,
    /// ชื่อ application ที่อ่านง่าย และใช้เป็น identity fallback ลำดับสุดท้าย
    pub application_name: Option<String>,
}

impl ApplicationIdentity {
    /// คืน identity ที่เหมาะสำหรับ persistence ข้าม session มากที่สุด
    pub fn stable_key(&self) -> Option<ApplicationKey> {
        if let Some(value) = non_empty(&self.application_id) {
            return Some(ApplicationKey::ApplicationId(value.to_owned()));
        }
        if let Some(value) = non_empty(&self.process_binary) {
            return Some(ApplicationKey::ProcessBinary(value.to_owned()));
        }
        non_empty(&self.application_name)
            .map(|value| ApplicationKey::ApplicationName(value.to_owned()))
    }

    /// คืนชื่อ application ที่เหมาะที่สุดสำหรับแสดงผล
    pub fn display_name(&self) -> Option<&str> {
        non_empty(&self.application_name)
            .or_else(|| non_empty(&self.application_id))
            .or_else(|| non_empty(&self.process_binary))
    }
}

/// persistent key ที่น่าเชื่อถือที่สุดของ application
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApplicationKey {
    ApplicationId(String),
    ProcessBinary(String),
    ApplicationName(String),
}

impl ApplicationKey {
    /// คืนค่า key ดิบโดยไม่มี discriminator
    pub fn value(&self) -> &str {
        match self {
            Self::ApplicationId(value)
            | Self::ProcessBinary(value)
            | Self::ApplicationName(value) => value,
        }
    }
}

/// playback stream ที่ normalized จาก audio source backend ใดก็ได้
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// backend-local ID ที่ใช้ได้เฉพาะ service session ปัจจุบัน
    pub runtime_id: u32,
    /// object serial คงที่ เมื่อ backend มีให้
    pub object_serial: Option<String>,
    /// ชื่อเชิงเทคนิคของ source หรือ node
    pub node_name: Option<String>,
    /// ชื่อ media หรือ session ที่อ่านง่าย
    pub media_name: Option<String>,
    /// application identity ที่ normalized สำหรับ grouping และ persistence
    pub application: ApplicationIdentity,
}

impl StreamInfo {
    pub fn application_key(&self) -> Option<ApplicationKey> {
        self.application.stable_key()
    }

    pub fn display_name(&self) -> &str {
        self.media_name
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| self.application.display_name())
            .or_else(|| self.node_name.as_deref().filter(|value| !value.is_empty()))
            .unwrap_or("Unknown audio stream")
    }
}

/// selection rule แบบถาวรสำหรับ application หรือ stream
///
/// `runtime_id` เป็น fallback ระดับ session เมื่อไม่มี metadata ที่คงที่
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub application: ApplicationIdentity,
    pub stream_match: Option<StreamDiscriminator>,
    pub runtime_id: Option<u32>,
}

/// metadata ที่ backend ให้มาเพื่อเลือก stream จาก application
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamDiscriminator {
    ObjectSerial(String),
    NodeName(String),
    MediaName(String),
}

impl CaptureTarget {
    pub fn application(application: ApplicationIdentity) -> Self {
        Self {
            application,
            stream_match: None,
            runtime_id: None,
        }
    }

    pub fn stream(info: &StreamInfo) -> Self {
        let stream_match = info
            .node_name
            .clone()
            .map(StreamDiscriminator::NodeName)
            .or_else(|| info.media_name.clone().map(StreamDiscriminator::MediaName))
            .or_else(|| {
                info.object_serial
                    .clone()
                    .map(StreamDiscriminator::ObjectSerial)
            });
        let runtime_id = stream_match.is_none().then_some(info.runtime_id);

        Self {
            application: info.application.clone(),
            stream_match,
            runtime_id,
        }
    }

    pub fn matches(&self, stream: &StreamInfo) -> bool {
        if let Some(runtime_id) = self.runtime_id
            && runtime_id != stream.runtime_id
        {
            return false;
        }

        if !strongest_common_identity_matches(&self.application, &stream.application) {
            return false;
        }

        let Some(stream_match) = &self.stream_match else {
            return self.application.stable_key().is_some() || self.runtime_id.is_some();
        };

        match stream_match {
            StreamDiscriminator::ObjectSerial(expected) => {
                non_empty(&stream.object_serial) == Some(expected)
            }
            StreamDiscriminator::NodeName(expected) => {
                non_empty(&stream.node_name) == Some(expected)
            }
            StreamDiscriminator::MediaName(expected) => {
                non_empty(&stream.media_name) == Some(expected)
            }
        }
    }
}

/// event ที่ audio source backend ทุกตัวส่งออก
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioSourceEvent {
    StreamsChanged(Vec<StreamInfo>),
    CaptureStarted {
        runtime_id: u32,
        audio_generation: u64,
    },
    CaptureStopped {
        runtime_id: u32,
        audio_generation: u64,
    },
    CaptureError {
        runtime_id: u32,
        audio_generation: u64,
        message: String,
    },
    Error(String),
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

fn strongest_common_identity_matches(
    expected: &ApplicationIdentity,
    actual: &ApplicationIdentity,
) -> bool {
    if let (Some(expected), Some(actual)) = (
        non_empty(&expected.application_id),
        non_empty(&actual.application_id),
    ) {
        return expected == actual;
    }
    if let (Some(expected), Some(actual)) = (
        non_empty(&expected.process_binary),
        non_empty(&actual.process_binary),
    ) {
        return expected == actual;
    }
    if let (Some(expected), Some(actual)) = (
        non_empty(&expected.application_name),
        non_empty(&actual.application_name),
    ) {
        return expected == actual;
    }
    expected.stable_key().is_none()
}

#[cfg(test)]
mod tests {
    use super::{ApplicationIdentity, CaptureTarget, StreamDiscriminator, StreamInfo};

    fn stream() -> StreamInfo {
        StreamInfo {
            runtime_id: 42,
            object_serial: Some("1001".to_owned()),
            node_name: Some("chrome-output".to_owned()),
            media_name: Some("Video audio".to_owned()),
            application: ApplicationIdentity {
                application_id: Some("com.google.Chrome".to_owned()),
                process_binary: Some("chrome".to_owned()),
                application_name: Some("Google Chrome".to_owned()),
            },
        }
    }

    #[test]
    fn discriminator_does_not_match_a_different_metadata_field() {
        let target = CaptureTarget {
            application: stream().application,
            stream_match: Some(StreamDiscriminator::NodeName("Video audio".to_owned())),
            runtime_id: None,
        };

        assert!(!target.matches(&stream()));
    }

    #[test]
    fn process_rule_matches_when_current_stream_also_has_application_id() {
        let target = CaptureTarget::application(ApplicationIdentity {
            application_id: None,
            process_binary: Some("chrome".to_owned()),
            application_name: None,
        });

        assert!(target.matches(&stream()));
    }

    #[test]
    fn differing_shared_application_ids_do_not_fall_back_to_process_name() {
        let target = CaptureTarget::application(ApplicationIdentity {
            application_id: Some("org.example.Other".to_owned()),
            process_binary: Some("chrome".to_owned()),
            application_name: None,
        });

        assert!(!target.matches(&stream()));
    }
}
