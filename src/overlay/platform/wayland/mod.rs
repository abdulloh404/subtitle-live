//! เลือกเส้นทาง X11 overlay สำหรับ native X11 session และ Wayland/XWayland session
//!
//! overlay ยังไม่รองรับ native Wayland และใช้งานบน Wayland desktop ได้เมื่อ
//! มี display ของ XWayland ผ่านตัวแปร `DISPLAY` เท่านั้น

use std::env;

use crate::overlay::session::DesktopSession;

/// backend ที่เข้ากันได้กับ X11 ซึ่ง overlay helper ใช้งานได้
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayRuntimeBackend {
    /// X11 ผ่าน XWayland ภายใน session Wayland
    XWayland,
    /// native X11 ภายใน X11 session
    X11,
    /// ไม่มี X11 display ให้ overlay helper ใช้งาน
    Unavailable,
}

impl OverlayRuntimeBackend {
    /// ตรวจหา backend จาก desktop session และการมีอยู่ของ `DISPLAY`
    pub fn detect(session: DesktopSession) -> Self {
        Self::from_display(
            session,
            env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()),
        )
    }

    /// คืน identifier ที่คงที่สำหรับ UI และ structured logging
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XWayland => "xwayland",
            Self::X11 => "x11",
            Self::Unavailable => "unavailable",
        }
    }

    /// คืนชื่อ backend สำหรับแสดงแก่ผู้ใช้
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::XWayland => "XWayland",
            Self::X11 => "X11",
            Self::Unavailable => "Unavailable",
        }
    }

    /// เลือก backend เฉพาะเมื่อทราบชนิดของ session และมี X11 ให้ใช้งาน
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
    use super::OverlayRuntimeBackend;
    use crate::overlay::session::DesktopSession;

    #[test]
    fn selects_xwayland_for_wayland_with_an_x11_display() {
        assert_eq!(
            OverlayRuntimeBackend::from_display(DesktopSession::Wayland, true),
            OverlayRuntimeBackend::XWayland
        );
    }

    #[test]
    fn selects_native_x11_for_an_x11_session() {
        assert_eq!(
            OverlayRuntimeBackend::from_display(DesktopSession::X11, true),
            OverlayRuntimeBackend::X11
        );
    }

    #[test]
    fn rejects_wayland_without_xwayland() {
        assert_eq!(
            OverlayRuntimeBackend::from_display(DesktopSession::Wayland, false),
            OverlayRuntimeBackend::Unavailable
        );
    }

    #[test]
    fn does_not_guess_an_unknown_session_from_display() {
        assert_eq!(
            OverlayRuntimeBackend::from_display(DesktopSession::Unknown, true),
            OverlayRuntimeBackend::Unavailable
        );
    }
}
