//! ข้อผิดพลาดที่เกิดขึ้นระหว่างเริ่มบริการหลักของแอปพลิเคชัน

use std::{error::Error, fmt};

use crate::config::ConfigError;

#[derive(Debug)]
/// ข้อผิดพลาดร้ายแรงที่ทำให้แอปพลิเคชันเริ่มทำงานไม่ได้
pub enum AppError {
    /// การค้นหา อ่าน ตรวจสอบ หรือบันทึกการตั้งค่าล้มเหลว
    Config(ConfigError),
    /// ไม่สามารถติดตั้งตัวรับ tracing ได้
    Logging(String),
    /// ไม่สามารถเริ่ม process สำหรับวาด subtitle overlay ได้
    Overlay(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "configuration error: {error}"),
            Self::Logging(error) => write!(formatter, "failed to initialize logging: {error}"),
            Self::Overlay(error) => write!(formatter, "overlay error: {error}"),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Logging(_) | Self::Overlay(_) => None,
        }
    }
}

impl From<ConfigError> for AppError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}
