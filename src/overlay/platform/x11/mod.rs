//! X11/EWMH properties สำหรับ overlay ที่ทำงานได้ทั้ง Native X11 และ XWayland

use gdk_x11::X11Surface;
use gtk::prelude::*;
use x11rb::{
    connection::Connection,
    properties::WmHints,
    protocol::xproto::{
        Atom, AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _,
        PropMode, StackMode, Window,
    },
    wrapper::ConnectionExt as _,
};

/// ระยะห่างของกล่อง subtitle จากขอบ work area
const SCREEN_EDGE_GAP_PX: i64 = 32;

/// ตรวจว่า GTK window นี้ถูกสร้างบน X11 backend
pub(in crate::overlay) fn is_x11_window(window: &gtk::Window) -> bool {
    window
        .surface()
        .is_some_and(|surface| surface.is::<X11Surface>())
}

/// กำหนด type, focus และ EWMH state เฉพาะ XID ของ subtitle overlay
pub(in crate::overlay) fn configure_overlay_window(
    window: &gtk::Window,
    mapped: bool,
) -> Result<(), String> {
    let surface = window
        .surface()
        .ok_or_else(|| "the GTK overlay does not have a GDK surface yet".to_owned())?;
    let x11_surface = surface
        .downcast_ref::<X11Surface>()
        .ok_or_else(|| "the subtitle overlay is not using the X11 backend".to_owned())?;
    let xid = u32::try_from(x11_surface.xid())
        .map_err(|_| "the X11 window ID exceeds 32 bits".to_owned())?;

    let (connection, _) = x11rb::connect(None)
        .map_err(|error| format!("failed to connect to the X11 display: {error}"))?;
    let atoms = Atoms::load(&connection)?;

    if !mapped {
        set_override_redirect(&connection, xid)?;
    }
    set_input_focus_disabled(&connection, xid)?;
    set_focus_activation_disabled(&connection, xid, &atoms)?;
    remove_take_focus_protocol(&connection, xid, &atoms)?;
    connection
        .change_property32(
            PropMode::REPLACE,
            xid,
            atoms.net_wm_window_type,
            AtomEnum::ATOM,
            &[atoms.net_wm_window_type_notification],
        )
        .map_err(|error| format!("failed to send the X11 window type: {error}"))?
        .check()
        .map_err(|error| format!("failed to set the X11 window type: {error}"))?;

    if !mapped {
        // เก็บ intent เดิมไว้ให้เครื่องมือตรวจสอบ แม้ override-redirect ไม่ถูก WM จัดการ
        connection
            .change_property32(
                PropMode::REPLACE,
                xid,
                atoms.net_wm_state,
                AtomEnum::ATOM,
                &[
                    atoms.net_wm_state_above,
                    atoms.net_wm_state_sticky,
                    atoms.net_wm_state_skip_taskbar,
                    atoms.net_wm_state_skip_pager,
                ],
            )
            .map_err(|error| format!("failed to send the X11 window states: {error}"))?
            .check()
            .map_err(|error| format!("failed to set the X11 window states: {error}"))?;
    } else {
        raise_overlay_window(&connection, xid)?;
    }

    connection
        .flush()
        .map_err(|error| format!("failed to flush X11 commands: {error}"))
}

/// ย้ายหน้าต่าง override-redirect ไปยัง work area โดยไม่ผ่าน window manager
pub(in crate::overlay) fn move_overlay_window(
    window: &gtk::Window,
    monitor_x: i32,
    monitor_y: i32,
    monitor_width: i32,
    monitor_height: i32,
    overlay_width: i32,
    overlay_height: i32,
    position: &str,
) -> Result<(), String> {
    let monitor_width = u32::try_from(monitor_width)
        .ok()
        .filter(|width| *width > 0)
        .ok_or_else(|| format!("invalid monitor width: {monitor_width}"))?;
    let monitor_height = u32::try_from(monitor_height)
        .ok()
        .filter(|height| *height > 0)
        .ok_or_else(|| format!("invalid monitor height: {monitor_height}"))?;
    let surface = window
        .surface()
        .ok_or_else(|| "the GTK overlay does not have a GDK surface yet".to_owned())?;
    let x11_surface = surface
        .downcast_ref::<X11Surface>()
        .ok_or_else(|| "the subtitle overlay is not using the X11 backend".to_owned())?;
    let xid = u32::try_from(x11_surface.xid())
        .map_err(|_| "the X11 window ID exceeds 32 bits".to_owned())?;
    let (connection, screen_index) = x11rb::connect(None)
        .map_err(|error| format!("failed to connect to the X11 display: {error}"))?;
    let root = connection
        .setup()
        .roots
        .get(screen_index)
        .ok_or_else(|| format!("X11 screen index {screen_index} was not found"))?
        .root;
    let atoms = Atoms::load(&connection)?;
    let monitor = (monitor_x, monitor_y, monitor_width, monitor_height);
    let (workarea_x, workarea_y, workarea_width, workarea_height) =
        monitor_workarea(&connection, root, &atoms, monitor).unwrap_or(monitor);
    let overlay_width = u32::try_from(overlay_width)
        .unwrap_or(1)
        .max(1)
        .min(workarea_width);
    let overlay_height = u32::try_from(overlay_height)
        .unwrap_or(1)
        .max(1)
        .min(workarea_height);
    let (x, y) = anchored_position(
        workarea_x,
        workarea_y,
        workarea_width,
        workarea_height,
        overlay_width,
        overlay_height,
        position,
    );

    connection
        .configure_window(
            xid,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(overlay_width)
                .height(overlay_height)
                .stack_mode(StackMode::ABOVE),
        )
        .map_err(|error| format!("failed to send the X11 overlay move request: {error}"))?
        .check()
        .map_err(|error| format!("failed to move the X11 overlay: {error}"))?;
    connection
        .flush()
        .map_err(|error| format!("failed to flush the X11 overlay move: {error}"))
}

/// คำนวณมุมของหน้าต่างขนาดจริงตามตำแหน่ง 9 จุดภายใน work area ที่เลือก
fn anchored_position(
    workarea_x: i32,
    workarea_y: i32,
    workarea_width: u32,
    workarea_height: u32,
    overlay_width: u32,
    overlay_height: u32,
    position: &str,
) -> (i32, i32) {
    let remaining_width = i64::from(workarea_width.saturating_sub(overlay_width));
    let remaining_height = i64::from(workarea_height.saturating_sub(overlay_height));
    // เมื่อกล่องเกือบเต็มจอ ให้ลด gap ทั้งสองฝั่งเท่ากันเพื่อไม่ให้ตำแหน่งซ้าย/ขวาสลับกัน
    let horizontal_gap = SCREEN_EDGE_GAP_PX.min(remaining_width / 2);
    let vertical_gap = SCREEN_EDGE_GAP_PX.min(remaining_height / 2);
    let horizontal_offset = match position {
        "top-left" | "center-left" | "bottom-left" => horizontal_gap,
        "top-right" | "center-right" | "bottom-right" => remaining_width - horizontal_gap,
        _ => remaining_width / 2,
    };
    let vertical_offset = match position {
        "top-left" | "top-center" | "top-right" => vertical_gap,
        "center-left" | "center" | "center-right" => remaining_height / 2,
        _ => remaining_height - vertical_gap,
    };
    (
        i64::from(workarea_x)
            .saturating_add(horizontal_offset)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        i64::from(workarea_y)
            .saturating_add(vertical_offset)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    )
}

/// ถอนหน้าต่างออกจาก lifecycle ของ window manager ก่อน map เพื่อไม่ให้มี active/focus state
fn set_override_redirect<C: Connection>(connection: &C, window: Window) -> Result<(), String> {
    connection
        .change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().override_redirect(1_u32),
        )
        .map_err(|error| format!("failed to send the X11 override-redirect value: {error}"))?
        .check()
        .map_err(|error| format!("failed to set X11 override-redirect: {error}"))
}

/// ยก subtitle overlay ขึ้นบนสุดโดยไม่ activate หรือเปลี่ยน input focus
fn raise_overlay_window<C: Connection>(connection: &C, window: Window) -> Result<(), String> {
    connection
        .configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )
        .map_err(|error| format!("failed to send the X11 overlay raise request: {error}"))?
        .check()
        .map_err(|error| format!("failed to raise the X11 overlay: {error}"))
}

/// อ่าน `_GTK_WORKAREAS_D<n>` ของ Mutter แล้วเลือกพื้นที่ที่ทับกับ monitor เป้าหมายมากที่สุด
fn monitor_workarea<C: Connection>(
    connection: &C,
    root: Window,
    atoms: &Atoms,
    monitor: (i32, i32, u32, u32),
) -> Option<(i32, i32, u32, u32)> {
    let desktop = connection
        .get_property(
            false,
            root,
            atoms.net_current_desktop,
            AtomEnum::CARDINAL,
            0,
            1,
        )
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()? as usize;
    let property = intern_atom(connection, format!("_GTK_WORKAREAS_D{desktop}").as_bytes()).ok()?;
    let reply = connection
        .get_property(false, root, property, AtomEnum::CARDINAL, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    let values = reply.value32()?.collect::<Vec<_>>();
    values
        .chunks_exact(4)
        .filter_map(|values| {
            let workarea = (
                i32::from_ne_bytes(values[0].to_ne_bytes()),
                i32::from_ne_bytes(values[1].to_ne_bytes()),
                values[2],
                values[3],
            );
            intersect_rectangles(monitor, workarea)
        })
        .max_by_key(|area| u64::from(area.2) * u64::from(area.3))
}

/// คืนพื้นที่ทับซ้อนของ monitor กับ work area โดยใช้ i64 ป้องกันพิกัดล้น
fn intersect_rectangles(
    first: (i32, i32, u32, u32),
    second: (i32, i32, u32, u32),
) -> Option<(i32, i32, u32, u32)> {
    let left = i64::from(first.0).max(i64::from(second.0));
    let top = i64::from(first.1).max(i64::from(second.1));
    let right =
        (i64::from(first.0) + i64::from(first.2)).min(i64::from(second.0) + i64::from(second.2));
    let bottom =
        (i64::from(first.1) + i64::from(first.3)).min(i64::from(second.1) + i64::from(second.3));
    if right <= left || bottom <= top {
        return None;
    }
    Some((
        i32::try_from(left).ok()?,
        i32::try_from(top).ok()?,
        u32::try_from(right - left).ok()?,
        u32::try_from(bottom - top).ok()?,
    ))
}

/// ปิด ICCCM input hint โดยรักษา hint อื่นที่ GTK ใส่มาแล้ว
fn set_input_focus_disabled<C: Connection>(connection: &C, window: Window) -> Result<(), String> {
    let mut hints = WmHints::get(connection, window)
        .map_err(|error| format!("failed to request WM_HINTS: {error}"))?
        .reply()
        .map_err(|error| format!("failed to read WM_HINTS: {error}"))?
        .unwrap_or_default();
    hints.input = Some(false);
    hints
        .set(connection, window)
        .map_err(|error| format!("failed to send non-focusable WM_HINTS: {error}"))?
        .check()
        .map_err(|error| format!("failed to set non-focusable WM_HINTS: {error}"))
}

/// ป้องกัน window manager มองการ map overlay ว่าเป็น user action แล้วแย่ง active window
fn set_focus_activation_disabled<C: Connection>(
    connection: &C,
    window: Window,
    atoms: &Atoms,
) -> Result<(), String> {
    // บังคับให้ Mutter อ่านเวลาจาก toplevel นี้แทน GDK user-time window ที่อาจมีค่าเก่า
    connection
        .delete_property(window, atoms.net_wm_user_time_window)
        .map_err(|error| {
            format!("failed to request deletion of the X11 user-time window: {error}")
        })?
        .check()
        .map_err(|error| format!("failed to delete the X11 user-time window: {error}"))?;
    connection
        .delete_property(window, atoms.net_startup_id)
        .map_err(|error| format!("failed to request deletion of the X11 startup ID: {error}"))?
        .check()
        .map_err(|error| format!("failed to delete the X11 startup ID: {error}"))?;
    // EWMH กำหนดค่า 0 เพื่อขอไม่ให้หน้าต่างใหม่รับ focus ตอน map
    connection
        .change_property32(
            PropMode::REPLACE,
            window,
            atoms.net_wm_user_time,
            AtomEnum::CARDINAL,
            &[0],
        )
        .map_err(|error| format!("failed to send the non-focusable X11 user time: {error}"))?
        .check()
        .map_err(|error| format!("failed to set the non-focusable X11 user time: {error}"))
}

/// ทำให้ ICCCM input model เป็น No Input โดยไม่ลบ protocol ปิดหน้าต่างหรือ ping ของ GTK
fn remove_take_focus_protocol<C: Connection>(
    connection: &C,
    window: Window,
    atoms: &Atoms,
) -> Result<(), String> {
    let reply = connection
        .get_property(
            false,
            window,
            atoms.wm_protocols,
            AtomEnum::ATOM,
            0,
            u32::MAX,
        )
        .map_err(|error| format!("failed to request WM_PROTOCOLS: {error}"))?
        .reply()
        .map_err(|error| format!("failed to read WM_PROTOCOLS: {error}"))?;
    let Some(protocols) = reply.value32() else {
        return Ok(());
    };
    let protocols = protocols
        .filter(|protocol| *protocol != atoms.wm_take_focus)
        .collect::<Vec<_>>();
    connection
        .change_property32(
            PropMode::REPLACE,
            window,
            atoms.wm_protocols,
            AtomEnum::ATOM,
            &protocols,
        )
        .map_err(|error| format!("failed to send non-focusable WM_PROTOCOLS: {error}"))?
        .check()
        .map_err(|error| format!("failed to set non-focusable WM_PROTOCOLS: {error}"))
}

/// Atom ที่ใช้กำหนด type, layer, workspace และรายการหน้าต่าง
struct Atoms {
    net_current_desktop: Atom,
    net_startup_id: Atom,
    net_wm_user_time: Atom,
    net_wm_user_time_window: Atom,
    net_wm_window_type: Atom,
    net_wm_window_type_notification: Atom,
    wm_protocols: Atom,
    wm_take_focus: Atom,
    net_wm_state: Atom,
    net_wm_state_above: Atom,
    net_wm_state_sticky: Atom,
    net_wm_state_skip_taskbar: Atom,
    net_wm_state_skip_pager: Atom,
}

impl Atoms {
    /// โหลด atom จาก X server สำหรับ connection ปัจจุบัน
    fn load<C: Connection>(connection: &C) -> Result<Self, String> {
        Ok(Self {
            net_current_desktop: intern_atom(connection, b"_NET_CURRENT_DESKTOP")?,
            net_startup_id: intern_atom(connection, b"_NET_STARTUP_ID")?,
            net_wm_user_time: intern_atom(connection, b"_NET_WM_USER_TIME")?,
            net_wm_user_time_window: intern_atom(connection, b"_NET_WM_USER_TIME_WINDOW")?,
            net_wm_window_type: intern_atom(connection, b"_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_notification: intern_atom(
                connection,
                b"_NET_WM_WINDOW_TYPE_NOTIFICATION",
            )?,
            wm_protocols: intern_atom(connection, b"WM_PROTOCOLS")?,
            wm_take_focus: intern_atom(connection, b"WM_TAKE_FOCUS")?,
            net_wm_state: intern_atom(connection, b"_NET_WM_STATE")?,
            net_wm_state_above: intern_atom(connection, b"_NET_WM_STATE_ABOVE")?,
            net_wm_state_sticky: intern_atom(connection, b"_NET_WM_STATE_STICKY")?,
            net_wm_state_skip_taskbar: intern_atom(connection, b"_NET_WM_STATE_SKIP_TASKBAR")?,
            net_wm_state_skip_pager: intern_atom(connection, b"_NET_WM_STATE_SKIP_PAGER")?,
        })
    }
}

/// แปลงชื่อ EWMH เป็น atom ของ X server ปัจจุบัน
fn intern_atom<C: Connection>(connection: &C, name: &[u8]) -> Result<Atom, String> {
    connection
        .intern_atom(false, name)
        .map_err(|error| format!("failed to request an X11 atom: {error}"))?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| format!("failed to read an X11 atom: {error}"))
}
