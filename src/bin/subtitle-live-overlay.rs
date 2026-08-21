//! Binary ขนาดเล็กสำหรับวาด subtitle ผ่าน X11/XWayland แยกจาก main application

use std::process::ExitCode;

fn main() -> ExitCode {
    match subtitle_live::overlay::run_overlay_helper() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ไม่สามารถเริ่ม subtitle overlay helper ได้: {error}");
            ExitCode::FAILURE
        }
    }
}
