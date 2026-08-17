mod schema;
mod storage;

pub use schema::{
    AppConfig, ApplicationRule, AudioConfig, CURRENT_CONFIG_VERSION, ConfigValidationError,
    GeneralConfig, PerformanceConfig, StreamRule, SttConfig, SubtitleConfig,
};
pub use storage::{
    ConfigError, ConfigLoadWarning, LoadedConfig, default_path, load_config, load_or_default,
    save_config,
};
