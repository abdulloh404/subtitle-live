//! ผลลัพธ์ที่ส่งออกหลังประมวลผลคำสั่งของแอปพลิเคชัน

use super::ApplicationState;

#[derive(Clone, Debug, Eq, PartialEq)]
/// ผลลัพธ์จาก controller ที่ runtime และชั้นแสดงผล GTK นำไปใช้
pub enum AppEvent {
    /// สถานะวงจรชีวิตของ pipeline เปลี่ยนแปลง
    StateChanged(ApplicationState),
    /// ฟิลด์การตั้งค่าที่ระบุเปลี่ยนแปลงและต้องบันทึกหรือนำไปใช้
    ConfigChanged(&'static str),
    /// สถานะชั่วคราวของเซสชันเปลี่ยน โดยไม่ต้องบันทึก config
    SessionChanged,
    /// ต้องแสดงหน้าต่างตั้งค่า
    SettingsRequested,
    /// ต้องปิดแอปพลิเคชัน
    QuitRequested,
    /// คำสั่งถูกปฏิเสธพร้อมคำอธิบายสำหรับผู้ใช้
    Error(String),
}
