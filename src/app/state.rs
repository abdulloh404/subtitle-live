//! สถานะวงจรชีวิตของ pipeline จากเสียงไปเป็นคำบรรยาย

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// สถานะ pipeline ปัจจุบันที่แสดงให้หน้าต่างตั้งค่าและ tray เหมือนกัน
pub enum ApplicationState {
    #[default]
    /// ไม่มี worker จับเสียงหรือถอดเสียงทำงานอยู่
    Stopped,
    /// กำลังเริ่มบริการเบื้องหลัง
    Starting,
    /// กำลังจับและถอดเสียงจากแหล่งที่เลือก
    Running,
    /// กำลังหยุดบริการเบื้องหลัง
    Stopping,
    /// การเริ่มระบบหรืองาน runtime ล้มเหลวและต้องให้ผู้ใช้ตรวจสอบ
    Error,
}
