//! หน้าต่าง overlay สำหรับแสดงคำบรรยายโดยไม่รับ keyboard หรือ pointer input

mod session;
mod window;

pub use session::{DesktopSession, OverlayRuntimeBackend};
pub use window::OverlayPresenter;
