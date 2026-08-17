use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk::{gdk, glib, pango};

use crate::config::SubtitleConfig;

const FINAL_HOLD_TIME: Duration = Duration::from_secs(3);

pub struct OverlayPresenter {
    enabled: Cell<bool>,
    generation: Rc<Cell<u64>>,
    label: gtk::Label,
    last_text: RefCell<String>,
    window: gtk::Window,
}

impl OverlayPresenter {
    pub fn new(application: &adw::Application, config: &SubtitleConfig) -> Self {
        install_styles(config);

        let label = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::End)
            .justify(gtk::Justification::Center)
            .lines(config.max_lines as i32)
            .max_width_chars(72)
            .selectable(false)
            .use_markup(false)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .build();
        label.add_css_class("subtitle-live-text");

        let surface = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .build();
        surface.add_css_class("subtitle-live-surface");
        surface.append(&label);

        let root = gtk::Box::builder()
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Fill)
            .build();
        root.add_css_class("subtitle-live-overlay-root");
        root.append(&surface);

        let window = gtk::Window::builder()
            .application(application)
            .child(&root)
            .decorated(false)
            .default_height(120)
            .default_width(900)
            .focusable(false)
            .hide_on_close(true)
            .resizable(false)
            .title("Subtitle-live Overlay")
            .build();
        window.add_css_class("subtitle-live-overlay");

        Self {
            enabled: Cell::new(config.visible),
            generation: Rc::new(Cell::new(0)),
            label,
            last_text: RefCell::new(String::new()),
            window,
        }
    }

    pub fn show_text(&self, text: &str, is_final: bool) {
        let text = text.trim();
        if !self.enabled.get() || text.is_empty() {
            if text.is_empty() {
                self.hide();
            }
            return;
        }

        self.label.set_label(text);
        self.last_text.replace(text.to_owned());
        if !self.window.is_visible() {
            self.window.present();
        }

        let next_generation = self.generation.get().wrapping_add(1);
        self.generation.set(next_generation);
        if is_final {
            let generation = Rc::clone(&self.generation);
            let window = self.window.clone();
            let label = self.label.clone();
            glib::timeout_add_local_once(FINAL_HOLD_TIME, move || {
                if generation.get() == next_generation {
                    label.set_label("");
                    window.hide();
                }
            });
        }
    }

    pub fn hide(&self) {
        self.generation
            .set(self.generation.get().wrapping_add(1));
        self.label.set_label("");
        self.last_text.borrow_mut().clear();
        self.window.hide();
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
        if !enabled {
            self.hide();
        }
    }
}

fn install_styles(config: &SubtitleConfig) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&format!(
        ".subtitle-live-overlay, .subtitle-live-overlay-root {{ background: transparent; }}\n\
         .subtitle-live-overlay-root {{ padding: 20px; }}\n\
         .subtitle-live-surface {{ background: rgba(0, 0, 0, {:.3}); border-radius: 12px; padding: 12px 20px; }}\n\
         .subtitle-live-text {{ color: white; font-size: {}px; font-weight: 600; }}",
        config.background_opacity, config.font_size
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
