//! การแสดงข้อความคำบรรยายและนำค่ารูปลักษณ์ไปใช้บน GTK main thread

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::{config::SubtitleConfig, subtitle::CaptionLineBuffer};

use super::{
    OverlayMonitorInfo,
    platform::x11::{configure_overlay_window, is_x11_window, move_overlay_window},
};

/// ระยะเวลาคงข้อความ final ไว้ก่อนซ่อนเมื่อไม่มีข้อความรุ่นใหม่
const FINAL_HOLD_TIME: Duration = Duration::from_secs(4);
// พื้นหลังเว้นซ้ายและขวาข้างละ 20 พิกเซล จึงต้องหักออกก่อนวัดข้อความ
const CAPTION_HORIZONTAL_PADDING_PX: u32 = 40;
/// callback หนึ่งครั้งที่รอให้ GTK ผ่านช่วง paint
type PaintCallback = Box<dyn FnOnce()>;
/// ช่อง callback ล่าสุดที่ใช้ร่วมกันระหว่าง presenter กับ frame clock
type PendingPaintCallback = Rc<RefCell<Option<PaintCallback>>>;
/// geometry ของ monitor ล่าสุดซึ่งต้องย้ำอีกครั้งหลัง window ถูก map
type TargetMonitorGeometry = Rc<Cell<Option<(i32, i32, i32, i32)>>>;
/// ตำแหน่ง 9 จุดล่าสุดที่ใช้คำนวณพิกัดหน้าต่าง X11 ขนาดจริง
type TargetPosition = Rc<RefCell<String>>;

/// จอจริงของ GDK จับคู่กับข้อมูลที่ส่งผ่าน IPC
struct OverlayMonitor {
    info: OverlayMonitorInfo,
    monitor: gdk::Monitor,
}

/// เจ้าของ GTK widget ทั้งหมดของ overlay ซึ่งต้องเรียกจาก GTK main thread
pub struct OverlayPresenter {
    /// config ล่าสุดที่นำไปใช้เพื่อหลีกเลี่ยงการสร้าง CSS ซ้ำ
    applied_config: RefCell<Option<SubtitleConfig>>,
    /// provider ที่ใช้เปลี่ยนสีพื้นหลังและขนาดตัวอักษรแบบ runtime
    css_provider: gtk::CssProvider,
    /// คิวบรรทัดที่ปิดบรรทัดเต็มแล้วและดันบรรทัดเก่าขึ้นตาม limit
    line_buffer: RefCell<CaptionLineBuffer>,
    /// ป้องกันการแสดงข้อความเมื่อผู้ใช้ปิด overlay
    enabled: Cell<bool>,
    /// token สำหรับยกเลิก timeout ของ final frame รุ่นเก่า
    generation: Rc<Cell<u64>>,
    /// callback ล่าสุดที่ต้องตอบหลัง paint โดย frame ใหม่จะแทน frame เก่าเสมอ
    pending_after_paint: PendingPaintCallback,
    /// ความกว้างข้อความสูงสุดของช่วงที่กำลังแสดง เพื่อให้กล่องโตได้แต่ไม่หด
    reserved_content_width: Cell<i32>,
    /// จำนวนบรรทัดสูงสุดของช่วงที่กำลังแสดง เพื่อรักษาความสูงหลังกล่องโตแล้ว
    reserved_line_count: Cell<usize>,
    /// ป้องกันการสร้าง tick/after-paint handler มากกว่าหนึ่งชุดพร้อมกัน
    after_paint_scheduled: Rc<Cell<bool>>,
    /// ระบุว่า renderer ใช้ X11/XWayland และต้องคง toplevel ไว้เพื่อรักษา stack
    is_x11: Rc<Cell<bool>>,
    /// label ที่รับเฉพาะ plain text ไม่ตีความ markup
    label: gtk::Label,
    /// ข้อความต้นทางล่าสุดสำหรับจัดรูปแบบใหม่เมื่อ config เปลี่ยน
    last_text: RefCell<String>,
    /// รายการจอจาก display ของ helper ซึ่งเป็นแหล่ง identity เพียงจุดเดียว
    monitors: RefCell<Vec<OverlayMonitor>>,
    /// root ที่ใช้วัดขนาดจริงของกล่อง subtitle โดยไม่มีพื้นที่โปร่งใสรอบนอก
    root: gtk::Overlay,
    /// กล่องที่กำหนดตำแหน่ง anchor บนหน้าจอ
    surface: gtk::Box,
    /// geometry ที่เลือกไว้สำหรับย้ำตำแหน่งหลัง map ครั้งแรก
    target_monitor_geometry: TargetMonitorGeometry,
    /// ตำแหน่งที่เลือกไว้สำหรับย้ำพิกัดหลังขนาดข้อความเปลี่ยน
    target_position: TargetPosition,
    /// หน้าต่างโปร่งใสที่ไม่รับ input
    window: gtk::Window,
}

impl OverlayPresenter {
    /// สร้าง overlay และนำ config เริ่มต้นไปใช้โดยยังไม่แสดงข้อความ
    pub fn new(application: &adw::Application, config: &SubtitleConfig) -> Self {
        let css_provider = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let label = gtk::Label::builder()
            .justify(gtk::Justification::Left)
            .selectable(false)
            .use_markup(false)
            .valign(gtk::Align::End)
            .yalign(1.0)
            .wrap(false)
            .build();
        label.add_css_class("subtitle-live-text");

        let surface = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .build();
        surface.add_css_class("subtitle-live-surface");
        surface.append(&label);

        let root = gtk::Overlay::builder()
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Fill)
            .build();
        root.add_css_class("subtitle-live-overlay-root");
        let canvas = gtk::Box::builder().hexpand(true).vexpand(true).build();
        root.set_child(Some(&canvas));
        root.add_overlay(&surface);
        // ให้ overlay subtitle มีส่วนต่อ natural size ของหน้าต่างแทน canvas โปร่งใสเต็มจอ
        root.set_measure_overlay(&surface, true);

        let window = gtk::Window::builder()
            .application(application)
            .child(&root)
            .decorated(false)
            .focusable(false)
            .hide_on_close(true)
            .resizable(false)
            .title("Subtitle-live Overlay")
            .build();
        window.add_css_class("subtitle-live-overlay");
        let is_x11 = Rc::new(Cell::new(false));
        let target_monitor_geometry = Rc::new(Cell::new(None));
        let target_position = Rc::new(RefCell::new(config.position.clone()));
        let is_x11_for_realize = Rc::clone(&is_x11);
        window.connect_realize(move |window| {
            if let Some(surface) = window.surface() {
                // พื้นที่ input ว่างทำให้ overlay ไม่ขวางการคลิกหน้าต่างด้านล่าง
                let empty_region = gtk::cairo::Region::create();
                surface.set_input_region(Some(&empty_region));
            }
            if is_x11_window(window) {
                is_x11_for_realize.set(true);
                configure_x11_or_report(window, false);
            }
        });
        let target_monitor_geometry_for_map = Rc::clone(&target_monitor_geometry);
        let target_position_for_map = Rc::clone(&target_position);
        let root_for_map = root.clone();
        window.connect_map(move |window| {
            // ย้ำ property เฉพาะช่วงเริ่ม map เพราะ GTK อาจเขียน window type ปกติทับ
            schedule_x11_configuration(window, Duration::from_millis(100));
            schedule_x11_configuration(window, Duration::from_millis(500));
            if target_monitor_geometry_for_map.get().is_some() {
                // หลัง map ให้วัดกล่องจริงอีกครั้ง เพราะ allocation เริ่มต้นอาจเพิ่งเปลี่ยน
                schedule_monitor_move(
                    window,
                    root_for_map.clone(),
                    Rc::clone(&target_monitor_geometry_for_map),
                    Rc::clone(&target_position_for_map),
                    Duration::from_millis(100),
                );
            }
        });
        let presenter = Self {
            applied_config: RefCell::new(None),
            css_provider,
            line_buffer: RefCell::new(CaptionLineBuffer::default()),
            enabled: Cell::new(false),
            generation: Rc::new(Cell::new(0)),
            pending_after_paint: Rc::new(RefCell::new(None)),
            after_paint_scheduled: Rc::new(Cell::new(false)),
            is_x11,
            label,
            last_text: RefCell::new(String::new()),
            monitors: RefCell::new(Vec::new()),
            root,
            reserved_content_width: Cell::new(0),
            reserved_line_count: Cell::new(0),
            surface,
            target_monitor_geometry,
            target_position,
            window,
        };
        presenter.refresh_monitors();
        presenter.apply_config(config);
        presenter
    }

    /// จัดรูปแบบและแสดง transcript frame
    pub fn show_text(&self, text: &str, is_final: bool) -> bool {
        if !self.enabled.get() {
            return false;
        }
        let source_text = text.trim();
        if source_text.is_empty() {
            return false;
        }
        let config = self.applied_config.borrow().clone().unwrap_or_default();
        let text = self.format_caption(source_text, &config);
        if text.is_empty() {
            return false;
        }

        self.label.set_label(&text);
        self.surface.set_visible(true);
        self.last_text.replace(source_text.to_owned());
        if !self.window.is_visible() {
            if self.is_x11.get() {
                // ย้ำ No Input model ทันที ก่อน map เพื่อไม่ให้ GTK ขอ focus ให้ toplevel
                configure_x11_or_report(&self.window, false);
            }
            // map โดยไม่ร้องขอ active window เพื่อไม่แย่ง focus จากเกมหรือวิดีโอ
            self.window.set_visible(true);
        }
        self.schedule_reposition(Duration::ZERO);

        let next_generation = self.generation.get().wrapping_add(1);
        self.generation.set(next_generation);
        if is_final {
            let generation = Rc::clone(&self.generation);
            let window = self.window.clone();
            let label = self.label.clone();
            let surface = self.surface.clone();
            glib::timeout_add_local_once(FINAL_HOLD_TIME, move || {
                if generation.get() == next_generation {
                    label.set_label("");
                    surface.set_visible(false);
                    window.hide();
                }
            });
        }
        true
    }

    /// เรียก callback หนึ่งครั้งหลัง frame clock ผ่านช่วง paint ถัดไปของ overlay
    pub fn after_next_paint<F>(&self, callback: F)
    where
        F: FnOnce() + 'static,
    {
        self.pending_after_paint.replace(Some(Box::new(callback)));
        self.window.queue_draw();
        if self.after_paint_scheduled.replace(true) {
            return;
        }

        let pending_after_paint = Rc::clone(&self.pending_after_paint);
        let after_paint_scheduled = Rc::clone(&self.after_paint_scheduled);
        self.window.add_tick_callback(move |_, frame_clock| {
            let pending_after_paint = Rc::clone(&pending_after_paint);
            let after_paint_scheduled = Rc::clone(&after_paint_scheduled);
            let handler_slot = Rc::new(RefCell::new(None));
            let handler_slot_for_signal = Rc::clone(&handler_slot);
            let handler_id = frame_clock.connect_after_paint(move |frame_clock| {
                if let Some(handler_id) = handler_slot_for_signal.borrow_mut().take() {
                    frame_clock.disconnect(handler_id);
                }
                after_paint_scheduled.set(false);
                if let Some(callback) = pending_after_paint.borrow_mut().take() {
                    callback();
                }
            });
            handler_slot.replace(Some(handler_id));
            glib::ControlFlow::Break
        });
    }

    /// ยกเลิก acknowledgment ที่ยังไม่ผ่าน paint เมื่อข้อความถูกซ่อน
    pub fn cancel_pending_paint(&self) {
        self.pending_after_paint.borrow_mut().take();
    }

    /// ล้างข้อความและ unmap passive overlay เพื่อไม่ทิ้ง surface เต็มจอไว้โดยไม่จำเป็น
    pub fn hide(&self) {
        self.cancel_pending_paint();
        self.generation.set(self.generation.get().wrapping_add(1));
        self.label.set_label("");
        self.surface.set_visible(false);
        self.line_buffer.borrow_mut().clear();
        self.last_text.borrow_mut().clear();
        self.reset_visual_reservation();
        self.window.hide();
    }

    /// เปิดหรือปิดการรับ subtitle frame ของ overlay
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
        if !enabled {
            self.hide();
        }
    }

    /// นำตำแหน่ง รูปแบบ และขอบเขตบรรทัดใหม่ไปใช้กับข้อความปัจจุบัน
    pub fn apply_config(&self, config: &SubtitleConfig) {
        if self.applied_config.borrow().as_ref() == Some(config) {
            return;
        }

        let layout_changed = self
            .applied_config
            .borrow()
            .as_ref()
            .is_none_or(|previous| {
                previous.font_size != config.font_size
                    || previous.width_px != config.width_px
                    || previous.max_lines != config.max_lines
            });
        let monitor_changed = self
            .applied_config
            .borrow()
            .as_ref()
            .is_none_or(|previous| previous.monitor_id != config.monitor_id);
        let position_changed = self
            .applied_config
            .borrow()
            .as_ref()
            .is_none_or(|previous| previous.position != config.position);
        self.set_enabled(config.visible);
        self.target_position.replace(config.position.clone());
        if monitor_changed {
            self.move_to_configured_monitor(&config.monitor_id);
        }
        apply_position(&self.surface, &config.position);
        apply_text_alignment(&self.label, &config.text_alignment);
        self.surface.set_width_request(-1);
        self.css_provider.load_from_data(&format!(
            ".subtitle-live-overlay, .subtitle-live-overlay-root {{ background: transparent; }}\n\
             .subtitle-live-surface {{ background: rgba(0, 0, 0, {:.3}); border-radius: 12px; padding: 12px 20px; }}\n\
             .subtitle-live-text {{ color: white; font-size: {}pt; font-weight: 600; }}",
            config.background_opacity, config.font_size
        ));
        if layout_changed {
            self.line_buffer.borrow_mut().clear();
            self.reset_visual_reservation();
            let source_text = self.last_text.borrow();
            if !source_text.is_empty() {
                self.label
                    .set_label(&self.format_caption(source_text.as_str(), config));
            }
        }
        self.applied_config.replace(Some(config.clone()));
        if layout_changed || position_changed {
            self.schedule_reposition(Duration::ZERO);
        }
    }

    /// อ่านรายการจอใหม่หลัง hotplug แล้วบังคับใช้จอเป้าหมายอีกครั้งแม้ config ไม่เปลี่ยน
    pub fn refresh_monitors(&self) -> Vec<OverlayMonitorInfo> {
        let detected = detect_overlay_monitors();
        let infos = detected
            .iter()
            .map(|monitor| monitor.info.clone())
            .collect::<Vec<_>>();
        self.monitors.replace(detected);
        if let Some(config) = self.applied_config.borrow().as_ref() {
            self.move_to_configured_monitor(&config.monitor_id);
        }
        infos
    }

    /// คืนข้อมูลจอปัจจุบันสำหรับส่งให้ main process โดยไม่สแกน GDK ซ้ำ
    pub fn monitor_infos(&self) -> Vec<OverlayMonitorInfo> {
        self.monitors
            .borrow()
            .iter()
            .map(|monitor| monitor.info.clone())
            .collect()
    }

    /// เลือก monitor จาก ID ของ helper และ fallback จอแรกเมื่อจอเดิมหายไป
    fn move_to_configured_monitor(&self, monitor_id: &str) {
        let monitors = self.monitors.borrow();
        let selected = monitors
            .iter()
            .find(|monitor| !monitor_id.is_empty() && monitor.info.id == monitor_id)
            .or_else(|| monitors.first());
        if let Some(selected) = selected {
            let geometry = selected.monitor.geometry();
            let geometry = (
                geometry.x(),
                geometry.y(),
                geometry.width(),
                geometry.height(),
            );
            self.target_monitor_geometry.set(Some(geometry));
            gtk::prelude::WidgetExt::realize(&self.window);
            if let Err(error) = move_measured_overlay(
                &self.window,
                &self.root,
                geometry,
                self.target_position.borrow().as_str(),
            ) {
                eprintln!("Failed to move the subtitle overlay to the selected monitor: {error}");
            }
        }
    }

    /// รอให้ GTK คำนวณ natural size ใหม่ก่อนย้ายกล่องตาม work area ที่เลือก
    fn schedule_reposition(&self, delay: Duration) {
        schedule_monitor_move(
            &self.window,
            self.root.clone(),
            Rc::clone(&self.target_monitor_geometry),
            Rc::clone(&self.target_position),
            delay,
        );
    }

    /// วัดข้อความด้วย Pango ตามขนาดฟอนต์จริง แล้วส่งผลให้คิวตัดและดันบรรทัด
    fn format_caption(&self, text: &str, config: &SubtitleConfig) -> String {
        let layout = self.label.create_pango_layout(None);
        let mut font = gtk::pango::FontDescription::new();
        font.set_size(config.font_size as i32 * gtk::pango::SCALE);
        font.set_weight(gtk::pango::Weight::Semibold);
        layout.set_font_description(Some(&font));
        let available_width = caption_content_width(config.width_px);

        let caption = self
            .line_buffer
            .borrow_mut()
            .update(text, config.max_lines, |candidate| {
                layout.set_text(candidate);
                layout.pixel_size().0 <= available_width
            });
        if caption.is_empty() {
            return caption;
        }

        let measured_width = caption
            .lines()
            .map(|line| {
                layout.set_text(line);
                layout.pixel_size().0
            })
            .max()
            .unwrap_or(1)
            .clamp(1, available_width);
        let reserved_width = self
            .reserved_content_width
            .get()
            .max(measured_width)
            .min(available_width);
        self.reserved_content_width.set(reserved_width);
        self.label.set_width_request(reserved_width);

        let line_count = caption.lines().count().max(1);
        let reserved_lines = self.reserved_line_count.get().max(line_count);
        self.reserved_line_count.set(reserved_lines);
        let missing_lines = reserved_lines.saturating_sub(line_count);
        if missing_lines == 0 {
            caption
        } else {
            format!("{}{}", "\n".repeat(missing_lines), caption)
        }
    }

    /// ล้าง high-water mark เมื่อ overlay จบช่วงแสดงหรือเปลี่ยน layout
    fn reset_visual_reservation(&self) {
        self.reserved_content_width.set(0);
        self.reserved_line_count.set(0);
        self.label.set_width_request(-1);
    }
}

/// คืนความกว้างพื้นที่ข้อความหลังหัก padding ของกล่อง โดยไม่ยอมให้เหลือศูนย์
fn caption_content_width(box_width_px: u32) -> i32 {
    let width = box_width_px
        .saturating_sub(CAPTION_HORIZONTAL_PADDING_PX)
        .max(1);
    i32::try_from(width).unwrap_or(i32::MAX)
}

/// ตรวจจอจาก display ของ helper แล้วสร้าง ID ที่คงตาม connector เมื่อมีข้อมูล
fn detect_overlay_monitors() -> Vec<OverlayMonitor> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let monitors = display.monitors();
    let mut detected = (0..monitors.n_items())
        .filter_map(|index| {
            monitors
                .item(index)
                .and_then(|item| item.downcast::<gdk::Monitor>().ok())
        })
        .collect::<Vec<_>>();
    // เรียงตาม desktop layout เพื่อให้ชื่อ Display N อ่านตรงกับตำแหน่งจริง
    detected.sort_by_key(|monitor| {
        let geometry = monitor.geometry();
        let identity = monitor
            .connector()
            .or_else(|| monitor.model())
            .unwrap_or_default();
        (geometry.x(), geometry.y(), identity)
    });
    let mut used_ids = Vec::<String>::new();
    detected
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| {
            let geometry = monitor.geometry();
            let connector = monitor.connector().filter(|value| !value.trim().is_empty());
            let model = monitor.model().filter(|value| !value.trim().is_empty());
            let base_id = connector.as_ref().map_or_else(
                || {
                    format!(
                        "geometry:{}:{}:{}:{}",
                        geometry.x(),
                        geometry.y(),
                        geometry.width(),
                        geometry.height()
                    )
                },
                |connector| format!("connector:{connector}"),
            );
            let duplicate_count = used_ids
                .iter()
                .filter(|id| id.as_str() == base_id.as_str())
                .count();
            used_ids.push(base_id.clone());
            let id = if duplicate_count == 0 {
                base_id
            } else {
                format!("{base_id}#{}", duplicate_count + 1)
            };
            let identity = connector.or(model);
            let label = identity.map_or_else(
                || {
                    format!(
                        "Display {} · {}×{}",
                        index + 1,
                        geometry.width(),
                        geometry.height()
                    )
                },
                |identity| {
                    format!(
                        "Display {} · {} · {}×{}",
                        index + 1,
                        identity,
                        geometry.width(),
                        geometry.height()
                    )
                },
            );
            OverlayMonitor {
                info: OverlayMonitorInfo {
                    id,
                    label,
                    x: geometry.x(),
                    y: geometry.y(),
                    width: geometry.width(),
                    height: geometry.height(),
                },
                monitor,
            }
        })
        .collect()
}

/// แปลงชื่อ position เป็น GTK alignment ของกล่องข้อความ
fn apply_position(surface: &gtk::Box, position: &str) {
    let (horizontal, vertical) = match position {
        "top-left" => (gtk::Align::Start, gtk::Align::Start),
        "top-center" => (gtk::Align::Center, gtk::Align::Start),
        "top-right" => (gtk::Align::End, gtk::Align::Start),
        "center-left" => (gtk::Align::Start, gtk::Align::Center),
        "center" => (gtk::Align::Center, gtk::Align::Center),
        "center-right" => (gtk::Align::End, gtk::Align::Center),
        "bottom-left" => (gtk::Align::Start, gtk::Align::End),
        "bottom-right" => (gtk::Align::End, gtk::Align::End),
        _ => (gtk::Align::Center, gtk::Align::End),
    };
    surface.set_halign(horizontal);
    surface.set_valign(vertical);
}

/// จัดแนวแต่ละบรรทัดและตำแหน่งข้อความภายในพื้นที่ของ label
fn apply_text_alignment(label: &gtk::Label, alignment: &str) {
    let (justification, xalign) = match alignment {
        "center" => (gtk::Justification::Center, 0.5),
        "right" => (gtk::Justification::Right, 1.0),
        _ => (gtk::Justification::Left, 0.0),
    };
    label.set_justify(justification);
    label.set_xalign(xalign);
}

/// ตั้ง X11 property หลัง map โดยไม่ raise ซ้ำตาม subtitle frame
fn schedule_x11_configuration(window: &gtk::Window, delay: Duration) {
    if !is_x11_window(window) {
        return;
    }
    let window = window.clone();
    glib::timeout_add_local_once(delay, move || {
        if window.is_mapped() {
            configure_x11_or_report(&window, true);
        }
    });
}

/// ย้ำตำแหน่งหลัง map เพราะก่อนหน้านั้น window manager ยังไม่รู้จัก X11 toplevel
fn schedule_monitor_move(
    window: &gtk::Window,
    root: gtk::Overlay,
    target_monitor_geometry: TargetMonitorGeometry,
    target_position: TargetPosition,
    delay: Duration,
) {
    if !is_x11_window(window) {
        return;
    }
    let window = window.clone();
    glib::timeout_add_local_once(delay, move || {
        if let Some(geometry) = target_monitor_geometry.get()
            && window.is_mapped()
            && let Err(error) =
                move_measured_overlay(&window, &root, geometry, target_position.borrow().as_str())
        {
            eprintln!("Failed to reapply the subtitle overlay position after mapping: {error}");
        }
    });
}

/// วัด natural size ของกล่องข้อความ แล้วขยับเฉพาะหน้าต่างขนาดนั้นบน X11
fn move_measured_overlay(
    window: &gtk::Window,
    root: &gtk::Overlay,
    monitor: (i32, i32, i32, i32),
    position: &str,
) -> Result<(), String> {
    let (_, natural_width, _, _) = root.measure(gtk::Orientation::Horizontal, -1);
    let (_, natural_height, _, _) = root.measure(gtk::Orientation::Vertical, natural_width);
    move_overlay_window(
        window,
        monitor.0,
        monitor.1,
        monitor.2,
        monitor.3,
        natural_width.max(1),
        natural_height.max(1),
        position,
    )
}

/// แสดงเฉพาะข้อผิดพลาดของการตั้งค่า window โดยไม่บันทึกข้อความ subtitle
fn configure_x11_or_report(window: &gtk::Window, mapped: bool) {
    if let Err(error) = configure_overlay_window(window, mapped) {
        eprintln!("Failed to configure the X11 subtitle overlay: {error}");
    }
}
