use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::schema::{AppConfig, CURRENT_CONFIG_VERSION, ConfigValidationError};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum ConfigError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Serialize(toml::ser::Error),
    InvalidValues(ConfigValidationError),
    UnsupportedVersion(u32),
    InvalidPath(PathBuf),
    ConfigDirectoryUnavailable,
    DataDirectoryUnavailable,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} configuration at {}: {source}",
                path.display()
            ),
            Self::Serialize(source) => {
                write!(formatter, "failed to serialize configuration: {source}")
            }
            Self::InvalidValues(source) => write!(formatter, "invalid configuration: {source}"),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "cannot save unsupported configuration version {version}; expected {CURRENT_CONFIG_VERSION}"
            ),
            Self::InvalidPath(path) => {
                write!(
                    formatter,
                    "configuration path has no file name: {}",
                    path.display()
                )
            }
            Self::ConfigDirectoryUnavailable => write!(
                formatter,
                "cannot determine configuration directory because XDG_CONFIG_HOME and HOME are unavailable"
            ),
            Self::DataDirectoryUnavailable => write!(
                formatter,
                "cannot determine data directory because XDG_DATA_HOME and HOME are unavailable"
            ),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialize(source) => Some(source),
            Self::InvalidValues(source) => Some(source),
            Self::UnsupportedVersion(_)
            | Self::InvalidPath(_)
            | Self::ConfigDirectoryUnavailable
            | Self::DataDirectoryUnavailable => None,
        }
    }
}

#[derive(Debug)]
pub enum ConfigLoadWarning {
    InvalidToml(toml::de::Error),
    InvalidValues(ConfigValidationError),
    UnsupportedVersion(u32),
}

impl fmt::Display for ConfigLoadWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToml(source) => write!(
                formatter,
                "configuration is invalid and was left unchanged; using safe defaults: {source}"
            ),
            Self::InvalidValues(source) => write!(
                formatter,
                "configuration contains invalid values and was left unchanged; using safe defaults: {source}"
            ),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "configuration version {version} is unsupported; using safe defaults for version {CURRENT_CONFIG_VERSION}"
            ),
        }
    }
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub warning: Option<ConfigLoadWarning>,
}

pub fn default_path() -> Result<PathBuf, ConfigError> {
    if let Some(base) = absolute_environment_path("XDG_CONFIG_HOME") {
        return Ok(base.join("subtitle-live/config.toml"));
    }

    absolute_environment_path("HOME")
        .map(|home| home.join(".config/subtitle-live/config.toml"))
        .ok_or(ConfigError::ConfigDirectoryUnavailable)
}

pub fn default_model_path() -> Result<PathBuf, ConfigError> {
    if let Some(base) = absolute_environment_path("XDG_DATA_HOME") {
        return Ok(base.join("subtitle-live/models/ggml-small.en.bin"));
    }

    absolute_environment_path("HOME")
        .map(|home| home.join(".local/share/subtitle-live/models/ggml-small.en.bin"))
        .ok_or(ConfigError::DataDirectoryUnavailable)
}

pub fn load_or_default(path: impl AsRef<Path>) -> Result<AppConfig, ConfigError> {
    let path = path.as_ref();
    let loaded = load_config(path)?;
    if let Some(warning) = loaded.warning {
        tracing::warn!(
            config_path = %path.display(),
            warning = %warning,
            "Configuration could not be loaded"
        );
    }
    Ok(loaded.config)
}

pub fn load_config(path: impl AsRef<Path>) -> Result<LoadedConfig, ConfigError> {
    let path = path.as_ref();
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedConfig {
                config: AppConfig::default(),
                warning: None,
            });
        }
        Err(source) => return Err(io_error("read", path, source)),
    };

    let config = match toml::from_str::<AppConfig>(&contents) {
        Ok(config) => config,
        Err(source) => {
            return Ok(LoadedConfig {
                config: AppConfig::default(),
                warning: Some(ConfigLoadWarning::InvalidToml(source)),
            });
        }
    };

    if config.config_version != CURRENT_CONFIG_VERSION {
        return Ok(LoadedConfig {
            warning: Some(ConfigLoadWarning::UnsupportedVersion(config.config_version)),
            config: AppConfig::default(),
        });
    }

    if let Err(source) = config.validate() {
        return Ok(LoadedConfig {
            warning: Some(ConfigLoadWarning::InvalidValues(source)),
            config: AppConfig::default(),
        });
    }

    Ok(LoadedConfig {
        config,
        warning: None,
    })
}

pub fn save_config(path: impl AsRef<Path>, config: &AppConfig) -> Result<(), ConfigError> {
    if config.config_version != CURRENT_CONFIG_VERSION {
        return Err(ConfigError::UnsupportedVersion(config.config_version));
    }
    config.validate().map_err(ConfigError::InvalidValues)?;

    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|source| io_error("create parent directory for", path, source))?;

    let serialized = toml::to_string_pretty(config).map_err(ConfigError::Serialize)?;
    let temporary_path = temporary_path(path)?;

    let write_result = write_temporary_file(&temporary_path, serialized.as_bytes());
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    if let Err(source) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(io_error("replace", path, source));
    }

    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync parent directory for", path, source))?;

    Ok(())
}

fn write_temporary_file(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error("create temporary", path, source))?;
    file.write_all(contents)
        .map_err(|source| io_error("write temporary", path, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync temporary", path, source))
}

fn temporary_path(path: &Path) -> Result<PathBuf, ConfigError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| ConfigError::InvalidPath(path.to_path_buf()))?;
    let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary_name = format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        sequence
    );
    Ok(path.with_file_name(temporary_name))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn absolute_environment_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    path.is_absolute().then_some(path)
}
