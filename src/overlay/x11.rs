//! X11/EWMH properties สำหรับ overlay ที่ทำงานได้ทั้ง Native X11 และ XWayland

use gdk_x11::X11Surface;
use gtk::prelude::*;
use x11rb::{
    connection::Connection,
    properties::WmHints,
    protocol::xproto::{
        Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask,
        PropMode, Window,
    },
    wrapper::ConnectionExt as _,
};

/// action ของ EWMH สำหรับเพิ่ม state โดยไม่สลับค่าปัจจุบัน
const NET_WM_STATE_ADD: u32 = 1;
/// source indication ว่าคำขอมาจาก application
const NET_WM_STATE_SOURCE_APPLICATION: u32 = 1;

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
