//! คำสั่งตามความต้องการของผู้ใช้ที่ application controller รองรับ

use crate::{audio_source::AudioSourceBackend, config::ComputeRequest};

#[derive(Clone, Debug, Eq, PartialEq)]
/// คำขอจาก UI หรือ tray ก่อนแปลงเป็นงานของ runtime
pub enum AppCommand {
    /// เปิดการจับเสียงและถอดเสียง
    StartSubtitles,
    /// ปิดการจับเสียงและถอดเสียง
    StopSubtitles,
    /// เริ่ม pipeline ใหม่หลังพบข้อผิดพลาด โดยคงการเลือกแหล่งเสียงเดิม
    RetryPipeline,
    /// กำหนดว่าการปิดหน้าต่างตั้งค่าต้องซ่อนหน้าต่างเท่านั้นหรือไม่
    SetKeepRunningWhenClosed(bool),
    /// เปลี่ยนบริการค้นหาและจับเสียง แล้วสร้าง audio service ใหม่เมื่อกำลังทำงาน
    SetAudioSourceBackend(AudioSourceBackend),
    /// เปลี่ยนระยะเสียงใหม่ขั้นต่ำก่อนเรียก Whisper รอบถัดไป
    SetSttStepMs(u32),
    /// เปลี่ยนความยาวเสียงย้อนหลังที่ส่งเป็นบริบทให้ Whisper
    SetSttWindowMs(u32),
    /// เปลี่ยน Whisper model และโหลด session ใหม่เมื่อ pipeline ทำงานอยู่
    SetSttModel(String),
    /// ขอให้ runtime ดาวน์โหลด Whisper model บน worker thread
    DownloadSttModel(String),
    /// เปลี่ยน compute backend และโหลด Whisper session ใหม่เมื่อ pipeline ทำงานอยู่
    SetSttComputeBackend(ComputeRequest),
    /// เปิดหรือปิดตัวกรองเสียงพลังงานต่ำก่อนเรียก Whisper
    SetVadEnabled(bool),
    /// เปิดหรือปิดการส่งข้อมูลถอดเสียงสดไปยังหน้า Debug เฉพาะเซสชันนี้
    SetDebugLiveEnabled(bool),
    /// เปิดหรือปิดการบันทึกข้อมูลถอดเสียงลงไฟล์เฉพาะเซสชันนี้
    SetDebugFileLoggingEnabled(bool),
    /// เปิดหรือปิด overlay โดยไม่เปลี่ยนสถานะการถอดเสียง
    SetSubtitleVisible(bool),
    /// เลือกจอด้วยรหัสที่ overlay helper รายงาน
    SetSubtitleMonitor(String),
    /// ย้าย overlay ไปยังตำแหน่งหน้าจอที่รองรับ
    SetSubtitlePosition(String),
    /// เปลี่ยนแนวข้อความภายในกล่องคำบรรยาย
    SetSubtitleTextAlignment(String),
    /// เปลี่ยนขนาดตัวอักษรคำบรรยายในหน่วยพอยต์
    SetSubtitleFontSize(u32),
    /// เปลี่ยนความกว้างสูงสุดของกล่องคำบรรยายในหน่วยพิกเซล
    SetSubtitleWidthPx(u32),
    /// เปลี่ยนความทึบพื้นหลัง overlay ในรูปเปอร์เซ็นต์จำนวนเต็ม
    SetSubtitleBackgroundOpacityPercent(u32),
    /// เปลี่ยนจำนวนบรรทัดคำบรรยายสูงสุดที่มองเห็น
    SetSubtitleMaxLines(u32),
    /// นำหน้าต่างตั้งค่ามาไว้ด้านหน้า
    ShowSettings,
    /// ออกจากโปรเซสและหยุดบริการเบื้องหลัง
    Quit,
}
