//! คำสั่งตามความต้องการของผู้ใช้ที่ application controller รองรับ

#[derive(Clone, Debug, Eq, PartialEq)]
/// คำขอจาก UI หรือ tray ก่อนแปลงเป็นงานของ runtime
pub enum AppCommand {
    /// เปิดการจับเสียงและถอดเสียง
    StartSubtitles,
    /// ปิดการจับเสียงและถอดเสียง
    StopSubtitles,
    /// กำหนดว่าการปิดหน้าต่างตั้งค่าต้องซ่อนหน้าต่างเท่านั้นหรือไม่
    SetKeepRunningWhenClosed(bool),
    /// เปิดหรือปิด overlay โดยไม่เปลี่ยนสถานะการถอดเสียง
    SetSubtitleVisible(bool),
    /// ย้าย overlay ไปยังตำแหน่งหน้าจอที่รองรับ
    SetSubtitlePosition(String),
    /// เปลี่ยนขนาดตัวอักษรคำบรรยายในหน่วยพอยต์
    SetSubtitleFontSize(u32),
    /// เปลี่ยนความทึบพื้นหลัง overlay ในรูปเปอร์เซ็นต์จำนวนเต็ม
    SetSubtitleBackgroundOpacityPercent(u32),
    /// เปลี่ยนจำนวนบรรทัดคำบรรยายสูงสุดที่มองเห็น
    SetSubtitleMaxLines(u32),
    /// นำหน้าต่างตั้งค่ามาไว้ด้านหน้า
    ShowSettings,
    /// ออกจากโปรเซสและหยุดบริการเบื้องหลัง
    Quit,
}
