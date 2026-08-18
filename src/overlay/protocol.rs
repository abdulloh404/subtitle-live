//! รูปแบบคำสั่งและ event สำหรับ IPC ระหว่าง main process กับ overlay helper

use std::{
    error::Error,
    fmt,
    io::{self, BufRead, Write},
};

use serde::{Serialize, de::DeserializeOwned};

use crate::config::SubtitleConfig;

/// ขนาดสูงสุดของ JSON message หนึ่งรายการเพื่อจำกัดหน่วยความจำจาก IPC ที่เสียหาย
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// คำสั่งที่ main process ส่งให้ overlay renderer
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayCommand {
    /// แสดง presentation text ล่าสุดโดยไม่ทำ transcript reconciliation ซ้ำ
    ShowText {
        /// รหัส frame สำหรับจับคู่ rendered acknowledgment ในภายหลัง
        frame_id: u64,
        /// ข้อความที่ main runtime จัดการ partial/final เรียบร้อยแล้ว
        text: String,
        /// ระบุว่า frame นี้ปิด segment แล้วหรือยัง
        is_final: bool,
    },
    /// ล้างข้อความที่มองเห็น แต่ helper ยังทำงานต่อ
    Hide,
    /// นำค่ารูปลักษณ์ล่าสุดไปใช้กับ renderer
    ApplyConfig {
        /// การตั้งค่า subtitle ที่ผ่าน validation จาก main process แล้ว
        config: SubtitleConfig,
    },
    /// ขอให้ helper ปิดหน้าต่างและออกอย่างสะอาด
    Shutdown,
    /// ตรวจว่า IPC และ GTK main loop ของ helper ยังตอบสนอง
    Ping {
        /// ค่าอ้างอิงที่ helper ต้องส่งกลับโดยไม่แก้ไข
        nonce: u64,
    },
}

/// Event ที่ overlay helper ส่งกลับ main process
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayEvent {
    /// Helper เปิด X11 display และพร้อมรับคำสั่งแล้ว
    Ready,
    /// คำตอบของ Ping สำหรับตรวจสุขภาพ process
    Pong {
        /// ค่าอ้างอิงจากคำสั่ง Ping
        nonce: u64,
    },
    /// GTK frame clock ผ่านช่วง paint หลังนำข้อความของ frame นี้ไปใช้แล้ว
    Rendered {
        /// รหัส frame จากคำสั่ง ShowText
        frame_id: u64,
        /// เวลา monotonic ของ GLib เมื่อ helper ผ่านช่วง paint ใช้ร่วมกันได้บนเครื่องเดียวกัน
        rendered_at_micros: i64,
    },
    /// Helper พบข้อผิดพลาดที่ main process ควรแสดงแก่ผู้ใช้
    Error {
        /// ข้อความสั้นที่ไม่รวม subtitle หรือข้อมูลเสียง
        message: String,
    },
}

/// ข้อผิดพลาดจากการเข้ารหัส ถอดรหัส หรืออ่านเขียน IPC frame
#[derive(Debug)]
pub enum OverlayProtocolError {
    /// การอ่านหรือเขียน stream ล้มเหลว
    Io(io::Error),
    /// JSON message ไม่ตรงกับ protocol
    Json(serde_json::Error),
    /// Message ใหญ่เกินขอบเขตที่ protocol อนุญาต
    MessageTooLarge,
}

impl fmt::Display for OverlayProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "overlay IPC I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid overlay IPC message: {error}"),
            Self::MessageTooLarge => write!(
                formatter,
                "overlay IPC message exceeds {MAX_MESSAGE_BYTES} bytes"
            ),
        }
    }
}

impl Error for OverlayProtocolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::MessageTooLarge => None,
        }
    }
}

impl From<io::Error> for OverlayProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for OverlayProtocolError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// เขียน JSON หนึ่งบรรทัดและ flush เพื่อให้ process ปลายทางรับคำสั่งทันที
pub fn write_message<W, T>(writer: &mut W, message: &T) -> Result<(), OverlayProtocolError>
where
    W: Write,
    T: Serialize,
{
    let encoded = serde_json::to_vec(message)?;
    if encoded.len() + 1 > MAX_MESSAGE_BYTES {
        return Err(OverlayProtocolError::MessageTooLarge);
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// อ่าน JSON หนึ่งบรรทัด โดยคืน `None` เมื่อปลายทางปิด stream ตามปกติ
pub fn read_message<R, T>(reader: &mut R) -> Result<Option<T>, OverlayProtocolError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    let mut encoded = Vec::new();
    let mut limited = io::Read::take(&mut *reader, (MAX_MESSAGE_BYTES + 1) as u64);
    let bytes_read = limited.read_until(b'\n', &mut encoded)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    if bytes_read > MAX_MESSAGE_BYTES || encoded.last() != Some(&b'\n') {
        return Err(OverlayProtocolError::MessageTooLarge);
    }
    encoded.pop();
    if encoded.last() == Some(&b'\r') {
        encoded.pop();
    }
    serde_json::from_slice(&encoded)
        .map(Some)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::{OverlayCommand, OverlayEvent, read_message, write_message};
    use crate::config::SubtitleConfig;

    #[test]
    fn command_round_trip_preserves_subtitle_text_and_frame() {
        let command = OverlayCommand::ShowText {
            frame_id: 42,
            text: "first line\nsecond line".to_owned(),
            is_final: false,
        };
        let mut bytes = Vec::new();
        write_message(&mut bytes, &command).expect("command should serialize");

        let mut reader = BufReader::new(Cursor::new(bytes));
        let decoded = read_message::<_, OverlayCommand>(&mut reader)
            .expect("command should deserialize")
            .expect("stream should contain one command");

        assert_eq!(decoded, command);
    }

    #[test]
    fn protocol_reads_multiple_messages_without_merging_frames() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &OverlayCommand::Ping { nonce: 7 })
            .expect("ping should serialize");
        write_message(&mut bytes, &OverlayCommand::Hide).expect("hide should serialize");

        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(
            read_message(&mut reader).expect("ping should deserialize"),
            Some(OverlayCommand::Ping { nonce: 7 })
        );
        assert_eq!(
            read_message(&mut reader).expect("hide should deserialize"),
            Some(OverlayCommand::Hide)
        );
        assert_eq!(
            read_message::<_, OverlayCommand>(&mut reader).expect("EOF should be valid"),
            None
        );
    }

    #[test]
    fn apply_config_and_rendered_event_use_shared_protocol() {
        let command = OverlayCommand::ApplyConfig {
            config: SubtitleConfig::default(),
        };
        let event = OverlayEvent::Rendered {
            frame_id: 99,
            rendered_at_micros: 123_456,
        };
        let mut command_bytes = Vec::new();
        let mut event_bytes = Vec::new();

        write_message(&mut command_bytes, &command).expect("config should serialize");
        write_message(&mut event_bytes, &event).expect("event should serialize");

        assert!(
            read_message::<_, OverlayCommand>(&mut BufReader::new(Cursor::new(command_bytes)))
                .expect("config should deserialize")
                .is_some()
        );
        assert_eq!(
            read_message(&mut BufReader::new(Cursor::new(event_bytes)))
                .expect("event should deserialize"),
            Some(event)
        );
    }
}
