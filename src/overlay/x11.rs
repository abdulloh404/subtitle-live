//! X11/EWMH properties สำหรับ overlay ที่ทำงานได้ทั้ง Native X11 และ XWayland

use gdk_x11::X11Surface;
use gtk::prelude::*;
use x11rb::{
    connection::Connection,
    properties::WmHints,
    protocol::xproto::{
        Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConfigureWindowAux,
        ConnectionExt as _, EventMask, PropMode, Window,
    },
    wrapper::ConnectionExt as _,
};

/// action ของ EWMH สำหรับเพิ่ม state โดยไม่สลับค่าปัจจุบัน
const NET_WM_STATE_ADD: u32 = 1;
/// source indication ว่าคำขอมาจาก application
const NET_WM_STATE_SOURCE_APPLICATION: u32 = 1;
/// gravity แบบระบุตำแหน่งเทียบ root window โดยตรง
const STATIC_GRAVITY: u32 = 10;
/// bit ที่ระบุว่าคำสั่ง move/resize มีค่า x, y, width และ height ครบ
const NET_MOVERESIZE_ALL_FIELDS: u32 = 0b1111 << 8;

/// ตรวจว่า GTK window นี้ถูกสร้างบน X11 backend
pub(super) fn is_x11_window(window: &gtk::Window) -> bool {
    window
        .surface()
        .is_some_and(|surface| surface.is::<X11Surface>())
}

/// กำหนด type, focus และ EWMH state เฉพาะ XID ของ subtitle overlay
pub(super) fn configure_overlay_window(window: &gtk::Window, mapped: bool) -> Result<(), String> {
    let surface = window
        .surface()
        .ok_or_else(|| "GTK overlay ยังไม่มี GDK surface".to_owned())?;
    let x11_surface = surface
        .downcast_ref::<X11Surface>()
        .ok_or_else(|| "subtitle overlay ไม่ได้ใช้ X11 backend".to_owned())?;
    let xid =
        u32::try_from(x11_surface.xid()).map_err(|_| "X11 window id มีขนาดเกิน 32 บิต".to_owned())?;

    let (connection, screen_index) =
        x11rb::connect(None).map_err(|error| format!("เชื่อมต่อ X11 display ไม่สำเร็จ: {error}"))?;
    let root = connection
        .setup()
        .roots
        .get(screen_index)
        .ok_or_else(|| format!("ไม่พบ X11 screen index {screen_index}"))?
        .root;
    let atoms = Atoms::load(&connection)?;

    set_input_focus_disabled(&connection, xid)?;
    set_focus_activation_disabled(&connection, xid, &atoms)?;
    connection
        .change_property32(
            PropMode::REPLACE,
            xid,
            atoms.net_wm_window_type,
            AtomEnum::ATOM,
            &[atoms.net_wm_window_type_notification],
        )
        .map_err(|error| format!("ส่งชนิดหน้าต่าง X11 ไม่สำเร็จ: {error}"))?
        .check()
        .map_err(|error| format!("ตั้งชนิดหน้าต่าง X11 ไม่สำเร็จ: {error}"))?;

    if mapped {
        add_window_states(
            &connection,
            root,
            xid,
            atoms.net_wm_state,
            atoms.net_wm_state_above,
            atoms.net_wm_state_sticky,
        )?;
        add_window_states(
            &connection,
            root,
            xid,
            atoms.net_wm_state,
            atoms.net_wm_state_skip_taskbar,
            atoms.net_wm_state_skip_pager,
        )?;
    } else {
        // ก่อน map ให้ window manager อ่าน state ทั้งหมดพร้อมการสร้างหน้าต่างครั้งแรก
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
            .map_err(|error| format!("ส่ง X11 window states ไม่สำเร็จ: {error}"))?
            .check()
            .map_err(|error| format!("ตั้ง X11 window states ไม่สำเร็จ: {error}"))?;
    }

    connection
        .flush()
        .map_err(|error| format!("flush คำสั่ง X11 ไม่สำเร็จ: {error}"))
}

/// ย้ายหน้าต่าง overlay ไปยัง work area ของจอเป้าหมายผ่าน window manager
pub(super) fn move_overlay_window(
    window: &gtk::Window,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<(), String> {
    let width = u32::try_from(width)
        .ok()
        .filter(|width| *width > 0)
        .ok_or_else(|| format!("ความกว้างจอไม่ถูกต้อง: {width}"))?;
    let height = u32::try_from(height)
        .ok()
        .filter(|height| *height > 0)
        .ok_or_else(|| format!("ความสูงจอไม่ถูกต้อง: {height}"))?;
    let surface = window
        .surface()
        .ok_or_else(|| "GTK overlay ยังไม่มี GDK surface".to_owned())?;
    let x11_surface = surface
        .downcast_ref::<X11Surface>()
        .ok_or_else(|| "subtitle overlay ไม่ได้ใช้ X11 backend".to_owned())?;
    let xid =
        u32::try_from(x11_surface.xid()).map_err(|_| "X11 window id มีขนาดเกิน 32 บิต".to_owned())?;
    let (connection, screen_index) =
        x11rb::connect(None).map_err(|error| format!("เชื่อมต่อ X11 display ไม่สำเร็จ: {error}"))?;
    let root = connection
        .setup()
        .roots
        .get(screen_index)
        .ok_or_else(|| format!("ไม่พบ X11 screen index {screen_index}"))?
        .root;
    let atoms = Atoms::load(&connection)?;
    let (x, y, width, height) = monitor_workarea(&connection, root, &atoms, (x, y, width, height))
        .unwrap_or((x, y, width, height));

    if window.is_mapped() {
        // หน้าต่างที่ map แล้วต้องขอผ่าน window manager; ConfigureWindow ตรง ๆ
        // อาจถูก Mutter เมินหรือคืน geometry เดิมเมื่อเปลี่ยนจอ
        let flags =
            STATIC_GRAVITY | NET_MOVERESIZE_ALL_FIELDS | (NET_WM_STATE_SOURCE_APPLICATION << 12);
        let event = ClientMessageEvent::new(
            32,
            xid,
            atoms.net_moveresize_window,
            ClientMessageData::from([
                flags,
                u32::from_ne_bytes(x.to_ne_bytes()),
                u32::from_ne_bytes(y.to_ne_bytes()),
                width,
                height,
            ]),
        );
        connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .map_err(|error| format!("ส่ง EWMH move/resize ไม่สำเร็จ: {error}"))?
            .check()
            .map_err(|error| format!("window manager ปฏิเสธการย้าย overlay: {error}"))?;
    } else {
        // ก่อน map ยังไม่มี window manager จัดการ จึงวาง geometry เริ่มต้นที่ XID ได้โดยตรง
        connection
            .configure_window(
                xid,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(width)
                    .height(height),
            )
            .map_err(|error| format!("ส่งคำขอย้าย X11 overlay ไม่สำเร็จ: {error}"))?
            .check()
            .map_err(|error| format!("ย้าย X11 overlay ไม่สำเร็จ: {error}"))?;
    }
    connection
        .flush()
        .map_err(|error| format!("flush คำสั่งย้าย X11 overlay ไม่สำเร็จ: {error}"))
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
        .map_err(|error| format!("ส่งคำขออ่าน WM_HINTS ไม่สำเร็จ: {error}"))?
        .reply()
        .map_err(|error| format!("อ่าน WM_HINTS ไม่สำเร็จ: {error}"))?
        .unwrap_or_default();
    hints.input = Some(false);
    hints
        .set(connection, window)
        .map_err(|error| format!("ส่ง WM_HINTS ไม่รับ focus ไม่สำเร็จ: {error}"))?
        .check()
        .map_err(|error| format!("ตั้ง WM_HINTS ไม่รับ focus ไม่สำเร็จ: {error}"))
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
        .map_err(|error| format!("ส่งคำขอล้าง X11 user-time window ไม่สำเร็จ: {error}"))?
        .check()
        .map_err(|error| format!("ล้าง X11 user-time window ไม่สำเร็จ: {error}"))?;
    connection
        .delete_property(window, atoms.net_startup_id)
        .map_err(|error| format!("ส่งคำขอล้าง X11 startup id ไม่สำเร็จ: {error}"))?
        .check()
        .map_err(|error| format!("ล้าง X11 startup id ไม่สำเร็จ: {error}"))?;
    // EWMH กำหนดค่า 0 เพื่อขอไม่ให้หน้าต่างใหม่รับ focus ตอน map
    connection
        .change_property32(
            PropMode::REPLACE,
            window,
            atoms.net_wm_user_time,
            AtomEnum::CARDINAL,
            &[0],
        )
        .map_err(|error| format!("ส่ง X11 user time แบบไม่รับ focus ไม่สำเร็จ: {error}"))?
        .check()
        .map_err(|error| format!("ตั้ง X11 user time แบบไม่รับ focus ไม่สำเร็จ: {error}"))
}

/// ส่ง `_NET_WM_STATE_ADD` ครั้งละสอง state ตามรูปแบบ EWMH
fn add_window_states<C: Connection>(
    connection: &C,
    root: Window,
    window: Window,
    state_property: Atom,
    first: Atom,
    second: Atom,
) -> Result<(), String> {
    let event = ClientMessageEvent::new(
        32,
        window,
        state_property,
        ClientMessageData::from([
            NET_WM_STATE_ADD,
            first,
            second,
            NET_WM_STATE_SOURCE_APPLICATION,
            0,
        ]),
    );
    connection
        .send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .map_err(|error| format!("ส่ง EWMH window state ไม่สำเร็จ: {error}"))?
        .check()
        .map_err(|error| format!("window manager ปฏิเสธ EWMH state: {error}"))?;
    Ok(())
}

/// Atom ที่ใช้กำหนด type, layer, workspace และรายการหน้าต่าง
struct Atoms {
    net_current_desktop: Atom,
    net_moveresize_window: Atom,
    net_startup_id: Atom,
    net_wm_user_time: Atom,
    net_wm_user_time_window: Atom,
    net_wm_window_type: Atom,
    net_wm_window_type_notification: Atom,
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
            net_moveresize_window: intern_atom(connection, b"_NET_MOVERESIZE_WINDOW")?,
            net_startup_id: intern_atom(connection, b"_NET_STARTUP_ID")?,
            net_wm_user_time: intern_atom(connection, b"_NET_WM_USER_TIME")?,
            net_wm_user_time_window: intern_atom(connection, b"_NET_WM_USER_TIME_WINDOW")?,
            net_wm_window_type: intern_atom(connection, b"_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_notification: intern_atom(
                connection,
                b"_NET_WM_WINDOW_TYPE_NOTIFICATION",
            )?,
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
        .map_err(|error| format!("ส่งคำขอ X11 atom ไม่สำเร็จ: {error}"))?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| format!("อ่านค่า X11 atom ไม่สำเร็จ: {error}"))
}
