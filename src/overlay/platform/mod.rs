//! นโยบายตาม platform และการเชื่อมต่อ window แบบ native สำหรับ overlay

mod wayland;
pub(super) mod x11;

pub use wayland::OverlayRuntimeBackend;
