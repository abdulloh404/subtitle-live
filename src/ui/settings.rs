use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::glib;

use crate::{
    app::{AppCommand, ApplicationController, ApplicationState},
    config::AppConfig,
};

const SETTINGS_WINDOW_NAME: &str = "subtitle-live-settings";

pub fn present_settings(
    application: &adw::Application,
    controller: Rc<RefCell<ApplicationController>>,
) {
    if let Some(window) = existing_settings_window(application) {
        window.present();
        return;
    }

    let snapshot = UiSnapshot::from_controller(&controller.borrow());
    let window = adw::PreferencesWindow::builder()
        .application(application)
        .default_height(680)
        .default_width(760)
        .hide_on_close(true)
        .search_enabled(true)
        .title("Subtitle-live Settings")
        .build();
    window.set_widget_name(SETTINGS_WINDOW_NAME);

    window.add(&general_page(&snapshot, controller));
    window.add(&audio_sources_page(&snapshot));
    window.add(&speech_recognition_page(&snapshot));
    window.add(&subtitle_page(&snapshot));
    window.add(&performance_page(&snapshot));
    window.add(&about_page());

    window.connect_close_request(|window| {
        window.hide();
        glib::Propagation::Stop
    });
    window.present();
}

fn existing_settings_window(application: &adw::Application) -> Option<adw::PreferencesWindow> {
    application
        .windows()
        .into_iter()
        .find(|window| window.widget_name() == SETTINGS_WINDOW_NAME)
        .and_then(|window| window.downcast::<adw::PreferencesWindow>().ok())
}

fn general_page(
    snapshot: &UiSnapshot,
    controller: Rc<RefCell<ApplicationController>>,
) -> adw::PreferencesPage {
    let page = preferences_page("General", "preferences-system-symbolic");
    let group = adw::PreferencesGroup::builder().title("General").build();

    let (live_row, live_switch) = switch_row(
        "Live Subtitles",
        "Capture selected audio and show English subtitles",
        snapshot.live_subtitles,
        true,
    );
    live_switch.connect_state_set(move |_, active| {
        let command = if active {
            AppCommand::StartSubtitles
        } else {
            AppCommand::StopSubtitles
        };
        let _ = controller.borrow_mut().handle_command(command);
        glib::Propagation::Proceed
    });
    group.add(&live_row);

    let (keep_running_row, _) = switch_row(
        "Keep Running When Closed",
        "Closing this window hides it without stopping subtitles",
        snapshot.keep_running_when_closed,
        false,
    );
    group.add(&keep_running_row);
    page.add(&group);
    page
}

fn audio_sources_page(snapshot: &UiSnapshot) -> adw::PreferencesPage {
    let page = preferences_page("Audio Sources", "audio-speakers-symbolic");

    let capture_group = adw::PreferencesGroup::builder().title("Capture").build();
    capture_group.add(&value_row("Capture Mode", &snapshot.capture_mode));
    capture_group.add(&value_row(
        "Configured Applications",
        &snapshot.configured_applications.to_string(),
    ));
    page.add(&capture_group);

    let applications_group = adw::PreferencesGroup::builder()
        .title("Applications")
        .description("Available PipeWire applications will appear here.")
        .build();
    applications_group.add(&message_row(
        "No audio sources available",
        "Source discovery will be added in the PipeWire milestone.",
    ));
    page.add(&applications_group);
    page
}

fn speech_recognition_page(snapshot: &UiSnapshot) -> adw::PreferencesPage {
    let page = preferences_page("Speech Recognition", "audio-input-microphone-symbolic");

    let recognition_group = adw::PreferencesGroup::builder()
        .title("Recognition")
        .build();
    recognition_group.add(&value_row("Language", &snapshot.language));
    recognition_group.add(&value_row("Model", &snapshot.model));
    recognition_group.add(&value_row("Compute Backend", &snapshot.backend));
    page.add(&recognition_group);

    let streaming_group = adw::PreferencesGroup::builder().title("Streaming").build();
    streaming_group.add(&value_row(
        "Audio Step",
        &format!("{} ms", snapshot.audio_step_ms),
    ));
    streaming_group.add(&value_row(
        "Context Window",
        &format_duration(snapshot.context_window_ms),
    ));
    let (vad_row, _) = switch_row(
        "Voice Activity Detection",
        "Detect speech before recognition",
        snapshot.vad_enabled,
        false,
    );
    streaming_group.add(&vad_row);
    page.add(&streaming_group);
    page
}

fn subtitle_page(snapshot: &UiSnapshot) -> adw::PreferencesPage {
    let page = preferences_page("Subtitle", "insert-text-symbolic");
    let appearance_group = adw::PreferencesGroup::builder()
        .title("Appearance")
        .build();

    let (visible_row, _) = switch_row(
        "Show Subtitle",
        "Display recognized English speech",
        snapshot.subtitle_visible,
        false,
    );
    appearance_group.add(&visible_row);
    appearance_group.add(&value_row("Position", &snapshot.subtitle_position));
    appearance_group.add(&value_row(
        "Font Size",
        &format!("{} pt", snapshot.font_size),
    ));
    appearance_group.add(&value_row(
        "Background Opacity",
        &format!("{}%", snapshot.background_opacity_percent),
    ));
    appearance_group.add(&value_row(
        "Maximum Lines",
        &snapshot.maximum_lines.to_string(),
    ));
    page.add(&appearance_group);
    page
}

fn performance_page(snapshot: &UiSnapshot) -> adw::PreferencesPage {
    let page = preferences_page("Performance", "utilities-system-monitor-symbolic");

    let status_group = adw::PreferencesGroup::builder().title("Pipeline").build();
    status_group.add(&value_row("Status", &snapshot.status));
    status_group.add(&value_row("Model", &snapshot.model));
    status_group.add(&value_row("Backend", &snapshot.backend));
    page.add(&status_group);

    let metrics_group = adw::PreferencesGroup::builder()
        .title("Latency")
        .description("Live pipeline measurements will appear here when processing is available.")
        .build();
    let (metrics_row, _) = switch_row(
        "Show Metrics",
        "Expose live performance measurements",
        snapshot.show_metrics,
        false,
    );
    metrics_group.add(&metrics_row);
    metrics_group.add(&value_row("Audio Buffer", "Not available"));
    metrics_group.add(&value_row("STT Inference", "Not available"));
    metrics_group.add(&value_row("Approximate Total", "Not available"));
    page.add(&metrics_group);
    page
}

fn about_page() -> adw::PreferencesPage {
    let page = preferences_page("About", "help-about-symbolic");
    let group = adw::PreferencesGroup::builder().title("Subtitle-live").build();
    group.add(&message_row(
        "Local English Live Subtitles",
        "Captures selected application audio and processes it locally.",
    ));
    group.add(&value_row("Version", env!("CARGO_PKG_VERSION")));
    group.add(&value_row("Privacy", "Local-first"));
    page.add(&group);
    page
}

fn preferences_page(title: &str, icon_name: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder()
        .icon_name(icon_name)
        .title(title)
        .build()
}

fn switch_row(
    title: &str,
    subtitle: &str,
    active: bool,
    editable: bool,
) -> (adw::ActionRow, gtk::Switch) {
    let toggle = gtk::Switch::builder()
        .active(active)
        .sensitive(editable)
        .valign(gtk::Align::Center)
        .build();
    let row = adw::ActionRow::builder()
        .activatable_widget(&toggle)
        .subtitle(subtitle)
        .title(title)
        .build();
    row.add_suffix(&toggle);
    (row, toggle)
}

fn value_row(title: &str, value: &str) -> adw::ActionRow {
    let value_label = gtk::Label::builder()
        .label(value)
        .selectable(true)
        .valign(gtk::Align::Center)
        .build();
    value_label.add_css_class("dim-label");

    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(&value_label);
    row
}

fn message_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .activatable(false)
        .subtitle(subtitle)
        .title(title)
        .build()
}

fn format_duration(milliseconds: u32) -> String {
    if milliseconds % 1_000 == 0 {
        format!("{} s", milliseconds / 1_000)
    } else {
        format!("{milliseconds} ms")
    }
}

struct UiSnapshot {
    live_subtitles: bool,
    keep_running_when_closed: bool,
    capture_mode: String,
    configured_applications: usize,
    language: String,
    model: String,
    backend: String,
    audio_step_ms: u32,
    context_window_ms: u32,
    vad_enabled: bool,
    subtitle_visible: bool,
    subtitle_position: String,
    font_size: u32,
    background_opacity_percent: u32,
    maximum_lines: u32,
    show_metrics: bool,
    status: String,
}

impl UiSnapshot {
    fn from_controller(controller: &ApplicationController) -> Self {
        let config = controller.config();
        Self {
            live_subtitles: config.general.live_subtitles,
            keep_running_when_closed: config.general.keep_running_when_closed,
            capture_mode: capture_mode_label(config),
            configured_applications: config.audio.rules.len(),
            language: language_label(config),
            model: config.stt.model.clone(),
            backend: display_identifier(&config.stt.backend),
            audio_step_ms: config.stt.step_ms,
            context_window_ms: config.stt.window_ms,
            vad_enabled: config.stt.vad_enabled,
            subtitle_visible: config.subtitle.visible,
            subtitle_position: display_identifier(&config.subtitle.position),
            font_size: config.subtitle.font_size,
            background_opacity_percent: (config.subtitle.background_opacity * 100.0).round()
                as u32,
            maximum_lines: config.subtitle.max_lines,
            show_metrics: config.performance.show_metrics,
            status: state_label(controller.state()).to_owned(),
        }
    }
}

fn capture_mode_label(config: &AppConfig) -> String {
    match config.audio.capture_mode.as_str() {
        "selected" => "Selected Applications".to_owned(),
        other => display_identifier(other),
    }
}

fn language_label(config: &AppConfig) -> String {
    match config.stt.language.as_str() {
        "en" => "English".to_owned(),
        other => display_identifier(other),
    }
}

fn display_identifier(value: &str) -> String {
    let words = value.replace(['-', '_'], " ");
    let mut characters = words.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

const fn state_label(state: ApplicationState) -> &'static str {
    match state {
        ApplicationState::Stopped => "Stopped",
        ApplicationState::Starting => "Starting",
        ApplicationState::Running => "Running",
        ApplicationState::Stopping => "Stopping",
        ApplicationState::Error => "Error",
    }
}
