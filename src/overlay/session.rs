//! การตรวจชนิด desktop session และ backend ที่ overlay helper สามารถใช้งานได้

use std::env;

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

    /// แปลงค่าจาก environment โดยยอมรับตัวพิมพ์เล็กหรือใหญ่
    fn from_session_type(value: Option<&str>) -> Self {
        match value {
            Some(value) if value.eq_ignore_ascii_case("wayland") => Self::Wayland,
            Some(value) if value.eq_ignore_ascii_case("x11") => Self::X11,
            _ => Self::Unknown,
        }
    }
}

/// Backend X11 ที่ subtitle overlay helper สามารถเปิดได้ใน session ปัจจุบัน
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayRuntimeBackend {
    /// X11 ผ่าน XWayland ภายใน Wayland session
    XWayland,
    /// X11 โดยตรงภายใน X11 session
    X11,
    /// ไม่มี X11 display ที่ overlay helper ใช้งานได้
    Unavailable,
}

impl OverlayRuntimeBackend {
    /// ตรวจ backend จากชนิด session และการมีอยู่ของ `DISPLAY`
    pub fn detect(session: DesktopSession) -> Self {
        Self::from_display(
            session,
            env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()),
        )
    }

    /// คืนชื่อสั้นสำหรับ UI และ structured logging
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XWayland => "xwayland",
            Self::X11 => "x11",
            Self::Unavailable => "unavailable",
        }
    }

    /// เลือก backend เฉพาะเมื่อ session ชัดเจนและมี X11 display
    const fn from_display(session: DesktopSession, display_available: bool) -> Self {
        match (session, display_available) {
            (DesktopSession::Wayland, true) => Self::XWayland,
            (DesktopSession::X11, true) => Self::X11,
            _ => Self::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopSession, OverlayRuntimeBackend};

    #[test]
    fn detects_wayland_session_with_xwayland_overlay() {
        let session = DesktopSession::from_session_type(Some("wayland"));

        assert_eq!(session, DesktopSession::Wayland);
        assert_eq!(
            OverlayRuntimeBackend::from_display(session, true),
            OverlayRuntimeBackend::XWayland
        );
    }

    #[test]
    fn detects_x11_session_with_native_x11_overlay() {
        let session = DesktopSession::from_session_type(Some("x11"));

        assert_eq!(session, DesktopSession::X11);
        assert_eq!(
            OverlayRuntimeBackend::from_display(session, true),
            OverlayRuntimeBackend::X11
        );
    }

    #[test]
    fn marks_wayland_without_x11_display_as_unavailable() {
        let session = DesktopSession::from_session_type(Some("WAYLAND"));

        assert_eq!(
            OverlayRuntimeBackend::from_display(session, false),
            OverlayRuntimeBackend::Unavailable
        );
    }

    #[test]
    fn does_not_guess_unknown_session_from_display() {
        let session = DesktopSession::from_session_type(None);

        assert_eq!(session, DesktopSession::Unknown);
        assert_eq!(
            OverlayRuntimeBackend::from_display(session, true),
            OverlayRuntimeBackend::Unavailable
        );
    }
}
