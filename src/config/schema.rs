use std::{error::Error, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const CURRENT_CONFIG_VERSION: u32 = 1;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub config_version: u32,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub subtitle: SubtitleConfig,
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
        if self.subtitle.font_size == 0 {
            return Err(ConfigValidationError::new(
                "subtitle.font_size",
                "must be greater than zero",
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
pub struct GeneralConfig {
    pub live_subtitles: bool,
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
pub struct AudioConfig {
    pub capture_mode: String,
    pub target_sample_rate: u32,
    pub target_channels: u16,
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
pub struct ApplicationRule {
    pub enabled: bool,
    pub application_id: Option<String>,
    pub process_binary: Option<String>,
    pub application_name: Option<String>,
    pub streams: Vec<StreamRule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamRule {
    pub enabled: bool,
    pub media_name: Option<String>,
    pub node_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttConfig {
    pub language: String,
    pub model: String,
    pub model_path: Option<PathBuf>,
    pub backend: String,
    pub step_ms: u32,
    pub window_ms: u32,
    pub vad_enabled: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            language: "en".to_owned(),
            model: "small.en".to_owned(),
            model_path: None,
            backend: "auto".to_owned(),
            step_ms: 250,
            window_ms: 3_000,
            vad_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleConfig {
    pub visible: bool,
    pub position: String,
    pub font_size: u32,
    pub background_opacity: f32,
    pub max_lines: u32,
}

impl Default for SubtitleConfig {
    fn default() -> Self {
        Self {
            visible: true,
            position: "bottom-center".to_owned(),
            font_size: 28,
            background_opacity: 0.60,
            max_lines: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    pub show_metrics: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self { show_metrics: true }
    }
}

fn has_text(value: &Option<String>) -> bool {
    value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}
