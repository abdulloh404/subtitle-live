//! หน้าต่าง overlay สำหรับแสดงคำบรรยายโดยไม่รับ keyboard หรือ pointer input

mod client;
mod platform;
mod process;
mod protocol;
mod session;
mod window;

pub use client::{OverlayClient, OverlayClientStatus};
pub use platform::OverlayRuntimeBackend;
pub use process::run_overlay_helper;
pub use protocol::{
    OverlayCommand, OverlayEvent, OverlayMonitorInfo, OverlayProtocolError, read_message,
    write_message,
};
pub use session::{DesktopBackendInfo, DesktopSession, DisplayBackend};
pub use window::OverlayPresenter;
