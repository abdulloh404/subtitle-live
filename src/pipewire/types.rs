//! ชนิดข้อมูลกลางสำหรับแยกตัวตน application, กฎการเลือก และ PipeWire event

use serde::{Deserialize, Serialize};

/// metadata ของ application ที่ค่อนข้างคงที่ เรียงจากตัวตนที่แข็งแรงไปอ่อนที่สุด
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApplicationIdentity {
    /// ID ที่ application ประกาศ เช่น desktop application ID
    pub application_id: Option<String>,
    /// ชื่อ binary ของ process ใช้เมื่อไม่มี application ID
    pub process_binary: Option<String>,
    /// ชื่อสำหรับแสดงผลและเป็น fallback ลำดับสุดท้าย
    pub application_name: Option<String>,
}

impl ApplicationIdentity {
    /// เลือก key ที่แข็งแรงที่สุดซึ่งสามารถเก็บข้ามการเปิด application ใหม่ได้
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

    /// เลือกชื่อที่เหมาะสำหรับ UI จาก metadata ที่มีอยู่
    pub fn display_name(&self) -> Option<&str> {
        non_empty(&self.application_name)
            .or_else(|| non_empty(&self.application_id))
            .or_else(|| non_empty(&self.process_binary))
    }
}

/// key แบบคงทนที่แข็งแรงที่สุดสำหรับจัดกลุ่ม stream ของ application
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApplicationKey {
    /// จับคู่ด้วย application ID
    ApplicationId(String),
    /// จับคู่ด้วยชื่อ process binary
    ProcessBinary(String),
    /// จับคู่ด้วยชื่อ application
    ApplicationName(String),
}

impl ApplicationKey {
    /// คืนข้อความดิบของ key โดยไม่สนใจชนิด discriminator
    pub fn value(&self) -> &str {
        match self {
            Self::ApplicationId(value)
            | Self::ProcessBinary(value)
            | Self::ApplicationName(value) => value,
        }
    }
}

/// PipeWire playback stream ที่ normalize แล้ว โดย runtime ID ไม่ใช่ตัวตนถาวร
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// node ID ที่ใช้ได้เฉพาะระหว่าง PipeWire session ปัจจุบัน
    pub runtime_id: u32,
    /// serial ของ PipeWire object เมื่อ node ประกาศค่าไว้
    pub object_serial: Option<String>,
    /// ชื่อทางเทคนิคของ PipeWire node
    pub node_name: Option<String>,
    /// ชื่อสื่อหรือ session ที่ application ประกาศ
    pub media_name: Option<String>,
    /// ตัวตน application ที่ normalize แล้วสำหรับจัดกลุ่มและจับคู่
    pub application: ApplicationIdentity,
}

impl StreamInfo {
    /// คืน application key ที่แข็งแรงที่สุดของ stream นี้
    pub fn application_key(&self) -> Option<ApplicationKey> {
        self.application.stable_key()
    }

    /// คืนชื่อ stream ที่อ่านง่ายที่สุด หรือข้อความ fallback เมื่อไม่มี metadata
    pub fn display_name(&self) -> &str {
        self.media_name
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| self.application.display_name())
            .or_else(|| self.node_name.as_deref().filter(|value| !value.is_empty()))
            .unwrap_or("Unknown audio stream")
    }
}

/// กฎ application แบบคงทน พร้อม discriminator สำหรับเลือก stream เดียวเมื่อมีค่า
///
/// `runtime_id` เป็นเพียง fallback ของ session ปัจจุบันเมื่อไม่มี metadata ที่คงทน
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CaptureTarget {
    /// metadata ของ application ที่ต้องตรงกัน
    pub application: ApplicationIdentity,
    /// metadata เพิ่มเติมเมื่อเลือก stream เดียวแทนทั้ง application
    pub stream_match: Option<StreamDiscriminator>,
    /// fallback ชั่วคราวเมื่อสร้าง discriminator แบบคงทนไม่ได้
    pub runtime_id: Option<u32>,
}

/// metadata เฉพาะ stream ที่ใช้แยกหลาย playback stream ของ application เดียวกัน
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamDiscriminator {
    /// จับคู่ด้วย PipeWire object serial
    ObjectSerial(String),
    /// จับคู่ด้วยชื่อ node
    NodeName(String),
    /// จับคู่ด้วยชื่อ media
    MediaName(String),
}

impl CaptureTarget {
    /// สร้างกฎที่เลือกทุก stream ซึ่งตรงกับ application นี้
    pub fn application(application: ApplicationIdentity) -> Self {
        Self {
            application,
            stream_match: None,
            runtime_id: None,
        }
    }

    /// สร้างกฎที่เจาะจง stream เดียวโดยเลือก metadata ที่เหมาะสมที่สุด
    pub fn stream(info: &StreamInfo) -> Self {
        let stream_match = info
            .node_name
            .clone()
            .map(StreamDiscriminator::NodeName)
            .or_else(|| {
                info.media_name
                    .clone()
                    .map(StreamDiscriminator::MediaName)
            })
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

    /// ตรวจว่า stream ปัจจุบันตรงกับ application และ discriminator ของกฎหรือไม่
    pub fn matches(&self, stream: &StreamInfo) -> bool {
        if let Some(runtime_id) = self.runtime_id
            && runtime_id != stream.runtime_id
        {
            return false;
        }

        let application_matches = strongest_common_identity_matches(
            &self.application,
            &stream.application,
        );
        if !application_matches {
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

/// event ที่ PipeWire service ส่งกลับไปให้ application runtime
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipeWireEvent {
    /// snapshot ใหม่ของ playback stream ทั้งหมดที่ค้นพบ
    StreamsChanged(Vec<StreamInfo>),
    /// capture ของ runtime node เริ่มส่งเสียงแล้ว
    CaptureStarted(u32),
    /// capture ของ runtime node ถูกหยุดหรือตัดการเชื่อมต่อแล้ว
    CaptureStopped(u32),
    /// capture ของ node เดียวล้มเหลว แต่ service อาจยังทำงานต่อได้
    CaptureError { runtime_id: u32, message: String },
    /// ข้อผิดพลาดระดับ PipeWire service หรือ core
    Error(String),
}

/// คืน string slice เฉพาะเมื่อ option มีข้อความที่ไม่ว่าง
fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

/// เปรียบเทียบ metadata ที่แข็งแรงที่สุดซึ่งทั้งสองฝั่งมีร่วมกัน
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
