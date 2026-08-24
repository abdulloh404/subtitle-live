//! การตั้งค่าผู้ใช้แบบมีเวอร์ชัน พร้อมการโหลดและบันทึกอย่างปลอดภัย

mod compute;
mod schema;
mod storage;
mod writer;

pub use compute::ComputeRequest;
pub use schema::{
    AppConfig, ApplicationRule, AudioConfig, CURRENT_CONFIG_VERSION, ConfigValidationError,
    GeneralConfig, PerformanceConfig, SUBTITLE_POSITIONS, SUBTITLE_TEXT_ALIGNMENTS, StreamRule,
    SttConfig, SubtitleConfig,
};
pub use storage::{
    ConfigError, ConfigLoadWarning, LoadedConfig, default_debug_log_path, default_model_path,
    default_path, load_config, load_or_default, save_config,
};
pub use writer::ConfigWriter;
