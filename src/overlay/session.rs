//! การตรวจชนิด desktop session และ backend ที่ overlay helper สามารถใช้งานได้

use std::env;

use gtk::gdk::{self, prelude::DisplayExtManual};

use super::platform::OverlayRuntimeBackend;

/// Backend ที่ GTK Settings ใช้งานจริงหลัง GDK เปิด display แล้ว
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayBackend {
    /// Wayland display แบบ native
    Wayland,
    /// X11 display แบบ native
    X11,
    /// Backend อื่นหรือยังไม่มี display
    Unknown,
}

impl DisplayBackend {
    /// ตรวจ backend จาก GDK display ที่ GTK main process เปิดใช้งานจริง
    pub fn detect() -> Self {
        let Some(display) = gdk::Display::default() else {
            return Self::Unknown;
        };
        match display.backend() {
            gdk::Backend::Wayland => Self::Wayland,
            gdk::Backend::X11 => Self::X11,
            _ => Self::Unknown,
        }
    }

    /// คืนชื่อสำหรับหน้า About และ structured log
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
            Self::Unknown => "Unknown",
        }
    }

    /// คืนชื่อที่อ่านง่ายสำหรับหน้า About
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
            Self::Unknown => "Unknown",
        }
    }
}

/// ชนิดของ desktop session ที่ main application กำลังทำงานอยู่
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSession {
    /// GNOME หรือ desktop อื่นที่ทำงานบน Wayland
    Wayland,
    /// Desktop session ที่ทำงานบน X11 โดยตรง
    X11,
    /// ไม่พบค่าที่เชื่อถือได้จาก environment ของ session
    Unknown,
}

impl DesktopSession {
    /// ตรวจ session จาก `XDG_SESSION_TYPE` โดยไม่เดาจาก display socket
    pub fn detect() -> Self {
        Self::from_session_type(env::var("XDG_SESSION_TYPE").ok().as_deref())
    }

    /// คืนชื่อสั้นสำหรับ UI และ structured logging
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
            Self::Unknown => "unknown",
        }
    }

    /// คืนชื่อที่อ่านง่ายสำหรับหน้า About
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
            Self::Unknown => "Unknown",
        }
    }

    /// แปลงค่าจาก environment โดยยอมรับตัวพิมพ์เล็กหรือใหญ่
    fn from_session_type(value: Option<&str>) -> Self {
        match value {
            Some(value) if value.eq_ignore_ascii_case("wayland") => Self::Wayland,
            Some(value) if value.eq_ignore_ascii_case("x11") => Self::X11,
            _ => Self::Unknown,
        }
    }
}

/// Snapshot เดียวสำหรับแสดงและวินิจฉัย desktop/overlay backend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopBackendInfo {
    /// Session จาก desktop environment
    pub session: DesktopSession,
    /// Backend ที่ Settings เปิดจริง
    pub settings_backend: DisplayBackend,
    /// Backend ของ helper ที่ spawn สำเร็จ
    pub overlay_backend: OverlayRuntimeBackend,
}

impl DesktopBackendInfo {
    /// สร้าง snapshot หลัง GTK เปิด display และ main ทราบผล spawn helper แล้ว
    pub const fn new(
        session: DesktopSession,
        settings_backend: DisplayBackend,
        overlay_backend: OverlayRuntimeBackend,
    ) -> Self {
        Self {
            session,
            settings_backend,
            overlay_backend,
        }
    }

    /// ระบุว่า XWayland พร้อมและ helper ถูกเลือกใช้งานสำเร็จ
    pub fn xwayland_available(self) -> bool {
        self.overlay_backend == OverlayRuntimeBackend::XWayland
    }
}

#[cfg(test)]
mod tests {
    use super::DesktopSession;

    #[test]
    fn detects_wayland_session() {
        let session = DesktopSession::from_session_type(Some("wayland"));

        assert_eq!(session, DesktopSession::Wayland);
    }

    #[test]
    fn detects_x11_session() {
        let session = DesktopSession::from_session_type(Some("x11"));

        assert_eq!(session, DesktopSession::X11);
    }
}
