//! การตั้งค่าล็อกแบบมีโครงสร้างสำหรับทั้งโปรเซส
//!
//! Raw transcript และ latency ราย inference ถูกควบคุมจากหน้า Debug
//! และปิดเป็นค่าเริ่มต้น จึงไม่ส่งข้อความผู้ใช้เข้า tracing ปกติ

use tracing_subscriber::EnvFilter;

use crate::error::AppError;

/// ติดตั้งตัวรับ tracing รูปแบบ JSON ก่อนเริ่มบริการของแอปพลิเคชัน
pub fn init() -> Result<(), AppError> {
    let default_filter =
        "warn,whisper_rs::whisper_logging_hook=off,whisper_rs::ggml_logging_hook=off";
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        // ชื่อ thread ช่วยยืนยันว่าการวัดเกิดบน worker ของ Whisper
        .with_thread_names(true)
        .with_thread_ids(true)
        .with_target(true)
        .json()
        .try_init()
        .map_err(|error| AppError::Logging(error.to_string()))?;
    // รับ native log ของ whisper.cpp ผ่าน EnvFilter แล้วปิด target ดังกล่าว
    // เพื่อไม่ให้ token dump หรือข้อความถอดเสียงหลุดออกทาง terminal
    whisper_rs::install_logging_hooks();
    Ok(())
}
