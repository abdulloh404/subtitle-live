//! ไลบรารีของแอปพลิเคชันคำบรรยายสดที่ทำงานภายในเครื่อง
//!
//! แต่ละโมดูลแยกตามหน้าที่เพื่อให้โค้ด GTK ติดต่อบริการผ่านคำสั่งและ event
//! แทนการควบคุม PipeWire หรือ Whisper โดยตรง

/// คำสั่ง event สถานะ และกฎการเลือกแหล่งเสียงของแอปพลิเคชัน
pub mod app;
/// การจับ แปลง ผสมเสียง และคิวเสียงแบบจำกัดขนาด
pub mod audio;
/// ขอบเขตกลางสำหรับเลือกและควบคุม PipeWire หรือ PulseAudio capture backend
pub mod audio_source;
/// การตั้งค่าผู้ใช้แบบมีเวอร์ชันและการบันทึกแบบเบื้องหลัง
pub mod config;
/// ตัวเขียนไฟล์ debug แบบไม่บล็อกเธรด GTK และ STT
pub mod debug_log;
/// ข้อผิดพลาดร้ายแรงขณะเริ่มแอปพลิเคชัน
pub mod error;
/// การเริ่มระบบ tracing แบบมีโครงสร้างของทั้งโปรเซส
pub mod logging;
/// การวัด latency และสถานะของคิว
pub mod metrics;
/// Catalog และ download service สำหรับ Whisper model ภายในเครื่อง
pub mod model;
/// การแสดงหน้าต่างคำบรรยายแบบไม่รับอินพุต
pub mod overlay;
/// implementation ของ native PipeWire ซึ่งเข้าถึงผ่าน `audio_source` เท่านั้น
mod pipewire;
/// การประสานงานระหว่างสถานะ UI และบริการเบื้องหลัง
pub mod runtime;
/// การเชื่อมต่อ Whisper เพื่อถอดเสียงภายในเครื่อง
pub mod stt;
/// การรวมผลถอดเสียงและจัดรูปแบบบรรทัดคำบรรยาย
pub mod subtitle;
/// ไอคอนสถานะบนเดสก์ท็อปและคำสั่งจากผู้ใช้
pub mod tray;
/// หน้าต่างตั้งค่าที่สร้างด้วย GTK
pub mod ui;
