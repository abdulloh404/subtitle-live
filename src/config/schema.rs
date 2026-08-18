//! โครงสร้างไฟล์ตั้งค่า ค่าเริ่มต้น และกฎตรวจสอบสำหรับ Phase 1

use std::{error::Error, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

/// เวอร์ชัน schema ที่โปรแกรมรุ่นนี้อ่านและเขียนได้
pub const CURRENT_CONFIG_VERSION: u32 = 1;
/// ตำแหน่ง anchor ของ overlay ที่ UI และ controller รองรับ
pub const SUBTITLE_POSITIONS: [&str; 9] = [
    "top-left",
    "top-center",
    "top-right",
    "center-left",
    "center",
    "center-right",
    "bottom-left",
    "bottom-center",
    "bottom-right",
];
/// แนวข้อความภายในกล่องคำบรรยายที่ UI และ controller รองรับ
pub const SUBTITLE_TEXT_ALIGNMENTS: [&str; 3] = ["left", "center", "right"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// การตั้งค่าระดับบนสุดซึ่งแบ่งตามความรับผิดชอบของแต่ละ pipeline stage
pub struct AppConfig {
    /// เวอร์ชัน schema สำหรับป้องกันการอ่านรูปแบบที่ไม่รองรับ
    pub config_version: u32,
    /// พฤติกรรมทั่วไปและสถานะเปิดใช้งาน
    #[serde(default)]
    pub general: GeneralConfig,
    /// รูปแบบเสียงเป้าหมายและกฎเลือกแหล่งเสียง
    #[serde(default)]
    pub audio: AudioConfig,
    /// โมเดลและช่วงเวลาการประมวลผล STT
    #[serde(default)]
    pub stt: SttConfig,
    /// รูปลักษณ์และตำแหน่งคำบรรยาย
    #[serde(default)]
    pub subtitle: SubtitleConfig,
    /// การแสดงข้อมูลประสิทธิภาพ
    #[serde(default)]
    pub performance: PerformanceConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            general: GeneralConfig::default(),
            audio: AudioConfig::default(),
            stt: SttConfig::default(),
            subtitle: SubtitleConfig::default(),
            performance: PerformanceConfig::default(),
        }
    }
}

impl AppConfig {
    /// ตรวจสอบ invariant ที่ทุกบริการใน Phase 1 ใช้ร่วมกัน
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.audio.capture_mode != "selected" {
            return Err(ConfigValidationError::new(
                "audio.capture_mode",
                "must be selected",
            ));
        }
        if self.audio.target_sample_rate != 16_000 {
            return Err(ConfigValidationError::new(
                "audio.target_sample_rate",
                "must be 16000 for the Phase 1 STT pipeline",
            ));
        }
        if self.audio.target_channels != 1 {
            return Err(ConfigValidationError::new(
                "audio.target_channels",
                "must be mono",
            ));
        }
        if self.stt.language != "en" {
            return Err(ConfigValidationError::new(
                "stt.language",
                "must be en during Phase 1",
            ));
        }
        if self.stt.model.trim().is_empty() {
            return Err(ConfigValidationError::new("stt.model", "must not be empty"));
        }
        if self
            .stt
            .model_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigValidationError::new(
                "stt.model_path",
                "must not be empty when configured",
            ));
        }
        if self.stt.backend.trim().is_empty() {
            return Err(ConfigValidationError::new(
                "stt.backend",
                "must not be empty",
            ));
        }
        if self.stt.step_ms == 0 {
            return Err(ConfigValidationError::new(
                "stt.step_ms",
                "must be greater than zero",
            ));
        }
        if self.stt.window_ms < self.stt.step_ms {
            return Err(ConfigValidationError::new(
                "stt.window_ms",
                "must be greater than or equal to stt.step_ms",
            ));
        }
        if self.stt.window_ms > 30_000 {
            return Err(ConfigValidationError::new(
                "stt.window_ms",
                "must not exceed 30000",
            ));
        }
        if self.subtitle.position.trim().is_empty() {
            return Err(ConfigValidationError::new(
                "subtitle.position",
                "must not be empty",
            ));
        }
        if self.subtitle.text_alignment.trim().is_empty() {
            return Err(ConfigValidationError::new(
                "subtitle.text_alignment",
                "must not be empty",
            ));
        }
        if self.subtitle.font_size == 0 {
            return Err(ConfigValidationError::new(
                "subtitle.font_size",
                "must be greater than zero",
            ));
        }
        if !(320..=3_840).contains(&self.subtitle.width_px) {
            return Err(ConfigValidationError::new(
                "subtitle.width_px",
                "must be between 320 and 3840",
            ));
        }
        if !self.subtitle.background_opacity.is_finite()
            || !(0.0..=1.0).contains(&self.subtitle.background_opacity)
        {
            return Err(ConfigValidationError::new(
                "subtitle.background_opacity",
                "must be between 0 and 1",
            ));
        }
        if !(1..=5).contains(&self.subtitle.max_lines) {
            return Err(ConfigValidationError::new(
                "subtitle.max_lines",
                "must be between 1 and 5",
            ));
        }

        for rule in &self.audio.rules {
            if !has_text(&rule.application_id)
                && !has_text(&rule.process_binary)
                && !has_text(&rule.application_name)
            {
                return Err(ConfigValidationError::new(
                    "audio.rules",
                    "each application rule must contain stable identity metadata",
                ));
            }

            if rule.streams.iter().any(|stream| {
                !has_text(&stream.media_name) && !has_text(&stream.node_name)
            })
            {
                return Err(ConfigValidationError::new(
                    "audio.rules.streams",
                    "each stream rule must contain a media or node name",
                ));
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// รายละเอียดฟิลด์การตั้งค่าที่ไม่ผ่าน validation
pub struct ConfigValidationError {
    field: &'static str,
    reason: &'static str,
}

impl ConfigValidationError {
    const fn new(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }
}

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.reason)
    }
}

impl Error for ConfigValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// พฤติกรรมการเริ่มและปิดหน้าต่างหลัก
pub struct GeneralConfig {
    /// กำหนดว่าควรเริ่ม pipeline คำบรรยายหรือไม่
    pub live_subtitles: bool,
    /// ให้โปรเซสทำงานต่อเมื่อผู้ใช้ปิดหน้าต่างตั้งค่า
    pub keep_running_when_closed: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            live_subtitles: false,
            keep_running_when_closed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// รูปแบบเสียงกลางและกฎเลือก application/stream
pub struct AudioConfig {
    /// โหมดการจับเสียง ซึ่ง Phase 1 อนุญาตเฉพาะ `selected`
    pub capture_mode: String,
    /// sample rate ที่ส่งเข้า Whisper
    pub target_sample_rate: u32,
    /// จำนวน channel หลังแปลงรูปแบบเสียง
    pub target_channels: u16,
    /// กฎถาวรสำหรับเลือก application และ stream
    pub rules: Vec<ApplicationRule>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            capture_mode: "selected".to_owned(),
            target_sample_rate: 16_000,
            target_channels: 1,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// กฎเลือก application ที่อาศัย metadata เสถียรแทน node ID ชั่วคราว
pub struct ApplicationRule {
    /// เลือกทุก playback stream ของ application นี้
    pub enabled: bool,
    /// application ID ที่ PipeWire รายงาน หากมี
    pub application_id: Option<String>,
    /// ชื่อ executable ของ process หากมี
    pub process_binary: Option<String>,
    /// ชื่อ application สำหรับ fallback สุดท้าย
    pub application_name: Option<String>,
    /// กฎราย stream เมื่อไม่ได้เลือกทั้ง application
    pub streams: Vec<StreamRule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// กฎเลือก playback stream ภายใน application
pub struct StreamRule {
    /// ระบุว่ากฎนี้มีผลหรือไม่
    pub enabled: bool,
    /// ชื่อสื่อสำหรับ fallback เมื่อไม่มี node name
    pub media_name: Option<String>,
    /// ชื่อ node ที่ใช้จับคู่เป็นลำดับแรก
    pub node_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// การตั้งค่าโมเดลและหน้าต่างเสียงสำหรับ speech-to-text
pub struct SttConfig {
    /// รหัสภาษาที่ Whisper ใช้ถอดเสียง
    pub language: String,
    /// ชื่อโมเดลที่แสดงใน UI และใช้ค้นหาไฟล์เริ่มต้น
    pub model: String,
    /// path โมเดลแบบกำหนดเอง หรือ `None` เพื่อใช้ตำแหน่งมาตรฐาน
    pub model_path: Option<PathBuf>,
    /// backend ประมวลผลที่ร้องขอ เช่น `auto`
    pub backend: String,
    /// ระยะห่างระหว่างรอบส่งเสียงเข้า STT
    pub step_ms: u32,
    /// ความยาวเสียงย้อนหลังสูงสุดที่ใช้เป็น context
    pub window_ms: u32,
    /// เปิดตัวกรองกิจกรรมเสียงด้วยระดับพลังงาน RMS ก่อนเรียก STT
    pub vad_enabled: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            language: "en".to_owned(),
            model: "small.en".to_owned(),
            model_path: None,
            backend: "auto".to_owned(),
            step_ms: 150,
            window_ms: 3_000,
            vad_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// การตั้งค่ารูปลักษณ์และขอบเขตข้อความของ overlay
pub struct SubtitleConfig {
    /// แสดงหรือซ่อน overlay
    pub visible: bool,
    /// anchor บนหน้าจอตามค่าใน [`SUBTITLE_POSITIONS`]
    pub position: String,
    /// แนวข้อความภายในกล่องตามค่าใน [`SUBTITLE_TEXT_ALIGNMENTS`]
    pub text_alignment: String,
    /// ขนาดตัวอักษรในหน่วยพอยต์
    pub font_size: u32,
    /// ความกว้างสูงสุดของกล่องคำบรรยายในหน่วยพิกเซล
    pub width_px: u32,
    /// ความทึบพื้นหลังช่วง `0.0..=1.0`
    pub background_opacity: f32,
    /// จำนวนบรรทัดคำบรรยายสูงสุด
    pub max_lines: u32,
}

impl Default for SubtitleConfig {
    fn default() -> Self {
        Self {
            visible: true,
            position: "bottom-center".to_owned(),
            text_alignment: "left".to_owned(),
            font_size: 28,
            width_px: 960,
            background_opacity: 0.60,
            max_lines: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// ตัวเลือกการแสดงข้อมูลประสิทธิภาพแก่ผู้ใช้
pub struct PerformanceConfig {
    /// แสดง latency และสถานะคิวในหน้าต่างตั้งค่า
    pub show_metrics: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self { show_metrics: true }
    }
}

/// ตรวจว่า metadata ทางเลือกมีข้อความที่ใช้งานได้
fn has_text(value: &Option<String>) -> bool {
    value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}
