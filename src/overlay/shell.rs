//! สะพาน D-Bus ระหว่างแอปกับ GNOME Shell Extension ที่วาด subtitle เหนือหน้าต่างแอป

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gtk::{
    gio::{self, prelude::*},
    glib::variant::ToVariant,
};

use crate::config::SubtitleConfig;

const OBJECT_PATH: &str = "/io/github/subtitle_live/Overlay";
const INTERFACE_NAME: &str = "io.github.subtitle_live.Overlay1";
const INTROSPECTION_XML: &str = r#"
<node>
  <interface name="io.github.subtitle_live.Overlay1">
    <method name="RegisterRenderer">
      <arg name="enabled" type="b" direction="out"/>
      <arg name="visible" type="b" direction="out"/>
      <arg name="text" type="s" direction="out"/>
      <arg name="is_final" type="b" direction="out"/>
      <arg name="position" type="s" direction="out"/>
      <arg name="font_size" type="u" direction="out"/>
      <arg name="width_px" type="u" direction="out"/>
      <arg name="background_opacity" type="d" direction="out"/>
      <arg name="max_lines" type="u" direction="out"/>
    </method>
    <method name="UnregisterRenderer"/>
    <signal name="Show">
      <arg name="text" type="s"/>
      <arg name="is_final" type="b"/>
    </signal>
    <signal name="Hide"/>
    <signal name="Configure">
      <arg name="enabled" type="b"/>
      <arg name="position" type="s"/>
      <arg name="font_size" type="u"/>
      <arg name="width_px" type="u"/>
      <arg name="background_opacity" type="d"/>
      <arg name="max_lines" type="u"/>
    </signal>
  </interface>
</node>
"#;

/// snapshot ล่าสุดที่ Extension จะได้รับทันทีเมื่อเริ่มเชื่อมต่อ
struct ShellOverlayState {
    renderer_active: Cell<bool>,
    config: RefCell<SubtitleConfig>,
    visible: Cell<bool>,
    text: RefCell<String>,
    is_final: Cell<bool>,
}

/// ส่งสถานะ overlay บน session bus โดยไม่ขวาง GTK main thread
pub(super) struct ShellOverlayBridge {
    connection: gio::DBusConnection,
    registration_id: Option<gio::RegistrationId>,
    state: Rc<ShellOverlayState>,
}

impl ShellOverlayBridge {
    /// export D-Bus object บน bus name ของ GApplication ที่ลงทะเบียนแล้ว
    pub(super) fn new(
        application: &adw::Application,
        config: &SubtitleConfig,
    ) -> Result<Self, String> {
        let connection = application
            .dbus_connection()
            .ok_or_else(|| "GApplication ไม่มี session D-Bus connection".to_owned())?;
        let interface = gio::DBusNodeInfo::for_xml(INTROSPECTION_XML)
            .map_err(|error| format!("อ่าน D-Bus interface ของ overlay ไม่ได้: {error}"))?
            .lookup_interface(INTERFACE_NAME)
            .ok_or_else(|| "ไม่พบ D-Bus interface ของ overlay".to_owned())?;
        let state = Rc::new(ShellOverlayState {
            renderer_active: Cell::new(false),
            config: RefCell::new(config.clone()),
            visible: Cell::new(false),
            text: RefCell::new(String::new()),
            is_final: Cell::new(false),
        });
        let method_state = Rc::clone(&state);
        let registration_id = connection
            .register_object(OBJECT_PATH, &interface)
            .method_call(
                move |_, _, _, _, method_name, _, invocation| match method_name {
                    "RegisterRenderer" => {
                        method_state.renderer_active.set(true);
                        let config = method_state.config.borrow();
                        let text = method_state.text.borrow();
                        let response = (
                            config.visible,
                            method_state.visible.get(),
                            text.as_str(),
                            method_state.is_final.get(),
                            config.position.as_str(),
                            config.font_size,
                            config.width_px,
                            f64::from(config.background_opacity),
                            config.max_lines,
                        )
                            .to_variant();
                        invocation.return_value(Some(&response));
                    }
                    "UnregisterRenderer" => {
                        method_state.renderer_active.set(false);
                        invocation.return_value(Some(&().to_variant()));
                    }
                    _ => invocation.return_dbus_error(
                        "io.github.subtitle_live.Error.UnknownMethod",
                        "ไม่รู้จัก method นี้",
                    ),
                },
            )
            .build()
            .map_err(|error| format!("export D-Bus object ของ overlay ไม่ได้: {error}"))?;

        Ok(Self {
            connection,
            registration_id: Some(registration_id),
            state,
        })
    }

    /// ตรวจว่า Shell Extension รับหน้าที่วาดแทน GTK overlay แล้วหรือยัง
    pub(super) fn renderer_active(&self) -> bool {
        self.state.renderer_active.get()
    }

    /// เก็บ config ล่าสุดและแจ้ง Extension ที่เชื่อมต่ออยู่
    pub(super) fn configure(&self, config: &SubtitleConfig) {
        self.state.config.replace(config.clone());
        if !self.renderer_active() {
            return;
        }
        let parameters = (
            config.visible,
            config.position.as_str(),
            config.font_size,
            config.width_px,
            f64::from(config.background_opacity),
            config.max_lines,
        )
            .to_variant();
        self.emit("Configure", &parameters);
    }

    /// เก็บและส่ง subtitle frame ที่จัดบรรทัดเสร็จแล้ว
    pub(super) fn show(&self, text: &str, is_final: bool) {
        self.state.visible.set(true);
        self.state.text.replace(text.to_owned());
        self.state.is_final.set(is_final);
        if !self.renderer_active() {
            return;
        }
        self.emit("Show", &(text, is_final).to_variant());
    }

    /// ล้าง snapshot และสั่งซ่อน actor
    pub(super) fn hide(&self) {
        self.state.visible.set(false);
        self.state.text.borrow_mut().clear();
        self.state.is_final.set(false);
        if self.renderer_active() {
            self.emit("Hide", &().to_variant());
        }
    }

    /// ส่ง signal แบบ fire-and-forget; ถ้า Extension หาย GTK fallback จะกลับมาใน poll ถัดไป
    fn emit(&self, signal_name: &str, parameters: &gtk::glib::Variant) {
        if let Err(error) = self.connection.emit_signal(
            None,
            OBJECT_PATH,
            INTERFACE_NAME,
            signal_name,
            Some(parameters),
        ) {
            self.state.renderer_active.set(false);
            tracing::warn!(
                signal = signal_name,
                error = %error,
                "ส่งสถานะไป GNOME Shell overlay ไม่สำเร็จ"
            );
        }
    }
}

impl Drop for ShellOverlayBridge {
    fn drop(&mut self) {
        if let Some(registration_id) = self.registration_id.take() {
            let _ = self.connection.unregister_object(registration_id);
        }
    }
}
