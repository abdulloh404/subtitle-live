//! การตั้งค่าล็อกแบบมีโครงสร้างสำหรับทั้งโปรเซส
//!
//! รุ่นพัฒนา (รวมถึง `cargo run`) เปิด `debug` เฉพาะโมดูล STT เพื่อรายงาน
//! latency และข้อความจาก Whisper ตามที่ใช้ตรวจสอบการถอดเสียง

use tracing_subscriber::EnvFilter;

use crate::error::AppError;

/// ติดตั้งตัวรับ tracing รูปแบบ JSON ก่อนเริ่มบริการของแอปพลิเคชัน
pub fn init() -> Result<(), AppError> {
    let default_filter = if cfg!(debug_assertions) {
        "warn,subtitle_live::stt=debug,whisper_rs::whisper_logging_hook=off,whisper_rs::ggml_logging_hook=off"
    } else {
        "warn"
    };
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
    // รับ native log ของ whisper.cpp ผ่าน EnvFilter แล้วปิด target ดังกล่าว เพื่อตัด
    // token dump ที่ไม่เกี่ยวข้องออก เหลือเฉพาะ log สามชนิดจากโมดูล STT ของแอป
    whisper_rs::install_logging_hooks();
    Ok(())
}
