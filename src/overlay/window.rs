use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::{config::SubtitleConfig, subtitle::format_live_caption};

const FINAL_HOLD_TIME: Duration = Duration::from_secs(3);

pub struct OverlayPresenter {
    applied_config: RefCell<Option<SubtitleConfig>>,
    css_provider: gtk::CssProvider,
    enabled: Cell<bool>,
    generation: Rc<Cell<u64>>,
    label: gtk::Label,
    last_text: RefCell<String>,
    surface: gtk::Box,
    window: gtk::Window,
}

impl OverlayPresenter {
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
            .justify(gtk::Justification::Center)
            .max_width_chars(42)
            .selectable(false)
            .use_markup(false)
            .wrap(false)
            .build();
        label.add_css_class("subtitle-live-text");

        let surface = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .build();
        surface.add_css_class("subtitle-live-surface");
        surface.set_margin_bottom(32);
        surface.set_margin_end(32);
        surface.set_margin_start(32);
        surface.set_margin_top(32);
        surface.append(&label);

        let root = gtk::Overlay::builder()
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Fill)
            .build();
        root.add_css_class("subtitle-live-overlay-root");
        let canvas = gtk::Box::builder()
            .hexpand(true)
            .vexpand(true)
            .build();
        root.set_child(Some(&canvas));
        root.add_overlay(&surface);

        let window = gtk::Window::builder()
            .application(application)
            .child(&root)
            .decorated(false)
            .default_height(600)
            .default_width(1000)
            .focusable(false)
            .hide_on_close(true)
            .resizable(true)
            .title("Subtitle-live Overlay")
            .build();
        window.add_css_class("subtitle-live-overlay");
        window.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                let empty_region = gtk::cairo::Region::create();
                surface.set_input_region(Some(&empty_region));
            }
        });
        window.maximize();

        let presenter = Self {
            applied_config: RefCell::new(None),
            css_provider,
            enabled: Cell::new(false),
            generation: Rc::new(Cell::new(0)),
            label,
            last_text: RefCell::new(String::new()),
            surface,
            window,
        };
        presenter.apply_config(config);
        presenter
    }

    pub fn show_text(&self, text: &str, is_final: bool) {
        if !self.enabled.get() {
            return;
        }
        let source_text = text.trim();
        if source_text.is_empty() {
            return;
        }
        let max_lines = self
            .applied_config
            .borrow()
            .as_ref()
            .map_or(2, |config| config.max_lines);
        let text = format_live_caption(source_text, max_lines);
        if text.is_empty() {
            return;
        }

        self.label.set_label(&text);
        self.last_text.replace(source_text.to_owned());
        // A normal GTK toplevel cannot request permanent always-on-top stacking
        // on GNOME Wayland. Re-present each update so an obscured overlay gets
        // another non-focusable raise request without intercepting input.
        self.window.present();

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

    pub fn apply_config(&self, config: &SubtitleConfig) {
        if self.applied_config.borrow().as_ref() == Some(config) {
            return;
        }

        self.set_enabled(config.visible);
        apply_position(&self.surface, &config.position);
        let source_text = self.last_text.borrow();
        if !source_text.is_empty() {
            self.label
                .set_label(&format_live_caption(source_text.as_str(), config.max_lines));
        }
        self.css_provider.load_from_data(&format!(
            ".subtitle-live-overlay, .subtitle-live-overlay-root {{ background: transparent; }}\n\
             .subtitle-live-surface {{ background: rgba(0, 0, 0, {:.3}); border-radius: 12px; padding: 12px 20px; }}\n\
             .subtitle-live-text {{ color: white; font-size: {}pt; font-weight: 600; }}",
            config.background_opacity, config.font_size
        ));
        self.applied_config.replace(Some(config.clone()));
    }
}

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
