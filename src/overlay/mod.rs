//! หน้าต่าง overlay สำหรับแสดงคำบรรยายโดยไม่รับ keyboard หรือ pointer input

mod protocol;
mod session;
mod window;

pub use protocol::{
    OverlayCommand, OverlayEvent, OverlayProtocolError, read_message, write_message,
};
pub use session::{DesktopSession, OverlayRuntimeBackend};
pub use window::OverlayPresenter;
