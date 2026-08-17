use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::glib;

use crate::{
    app::{AppCommand, AppEvent, ApplicationController, ApplicationState},
    config::AppConfig,
    metrics::MetricsSnapshot,
    pipewire::{ApplicationIdentity, ApplicationKey, StreamInfo},
};

const SETTINGS_WINDOW_NAME: &str = "subtitle-live-settings";

#[derive(Clone)]
pub struct SettingsPresenter {
    window: adw::PreferencesWindow,
    controller: Rc<RefCell<ApplicationController>>,
    applications: DynamicApplications,
    live_switch: gtk::Switch,
    status_label: gtk::Label,
    error_label: gtk::Label,
    model_path_label: gtk::Label,
    audio_buffer_label: gtk::Label,
    inference_label: gtk::Label,
    total_label: gtk::Label,
}

impl SettingsPresenter {
    pub fn new(
        application: &adw::Application,
        controller: Rc<RefCell<ApplicationController>>,
    ) -> Self {
        let snapshot = UiSnapshot::from_controller(&controller.borrow());
        let status_label = value_label(&snapshot.status);
        let error_label = value_label("None");
        let model_path_label = value_label("Not configured");
        let audio_buffer_label = value_label("Not available");
        let inference_label = value_label("Not available");
        let total_label = value_label("Not available");
        let configured_label = value_label(&snapshot.configured_applications.to_string());
        let applications_group = adw::PreferencesGroup::builder()
            .title("Applications")
            .description(
                "Select an application or expand it to select individual playback streams.",
            )
            .build();
        let applications = DynamicApplications {
            group: applications_group.clone(),
            rows: Rc::new(RefCell::new(Vec::new())),
            configured_label: configured_label.clone(),
            controller: Rc::clone(&controller),
        };

        let window = adw::PreferencesWindow::builder()
            .application(application)
            .default_height(680)
            .default_width(760)
            .hide_on_close(true)
            .search_enabled(true)
            .title("Subtitle-live Settings")
            .build();
        window.set_widget_name(SETTINGS_WINDOW_NAME);

        let (general, live_switch) =
            general_page(&snapshot, Rc::clone(&controller), status_label.clone());
        window.add(&general);
        window.add(&audio_sources_page(
            &snapshot,
            &configured_label,
            &applications_group,
        ));
        window.add(&speech_recognition_page(&snapshot, &model_path_label));
        window.add(&subtitle_page(&snapshot));
        window.add(&performance_page(
            &snapshot,
            &status_label,
            &error_label,
            &audio_buffer_label,
            &inference_label,
            &total_label,
        ));
        window.add(&about_page());

        let close_controller = Rc::clone(&controller);
        window.connect_close_request(move |window| {
            if close_controller
                .borrow()
                .config()
                .general
                .keep_running_when_closed
            {
                window.hide();
                glib::Propagation::Stop
            } else {
                if let Some(application) = window.application() {
                    application.quit();
                }
                glib::Propagation::Proceed
            }
        });

        applications.rebuild();
        Self {
            window,
            controller,
            applications,
            live_switch,
            status_label,
            error_label,
            model_path_label,
            audio_buffer_label,
            inference_label,
            total_label,
        }
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn update_streams(&self, streams: &[StreamInfo]) {
        self.controller.borrow_mut().set_streams(streams.to_vec());
        self.applications.rebuild();
    }

    pub fn update_state(&self, state: ApplicationState, error: Option<&str>) {
        self.controller.borrow_mut().set_state(state);
        self.status_label.set_label(state_label(state));
        let live_subtitles = self.controller.borrow().config().general.live_subtitles;
        self.live_switch.set_active(live_subtitles);
        self.error_label.set_label(error.unwrap_or("None"));
    }

    pub fn update_metrics(&self, metrics: MetricsSnapshot) {
        self.audio_buffer_label.set_label(&format_dropped(
            metrics.source_queue_dropped,
            metrics.mixed_queue_dropped,
        ));
        self.inference_label.set_label(&format_metric(
            metrics.inference_latest,
            metrics.inference_p50,
            metrics.inference_p95,
        ));
        self.total_label.set_label(&format_metric(
            metrics.total_latest,
            metrics.total_p50,
            metrics.total_p95,
        ));
    }

    pub fn update_model_status(&self, path: &Path, exists: bool) {
        let status = if exists {
            "Ready"
        } else {
            "Missing; run make model"
        };
        self.model_path_label
            .set_label(&format!("{} ({status})", path.display()));
    }
}

pub fn present_settings(
    application: &adw::Application,
    controller: Rc<RefCell<ApplicationController>>,
) -> SettingsPresenter {
    let presenter = SettingsPresenter::new(application, controller);
    presenter.present();
    presenter
}

#[derive(Clone)]
struct DynamicApplications {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<adw::PreferencesRow>>>,
    configured_label: gtk::Label,
    controller: Rc<RefCell<ApplicationController>>,
}

impl DynamicApplications {
    fn rebuild(&self) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }

        let groups = grouped_streams(self.controller.borrow().streams());
        if groups.is_empty() {
            let row: adw::PreferencesRow = message_row(
                "No playback streams found",
                "Start audio in an application and it will appear here.",
            )
            .upcast();
            self.group.add(&row);
            self.rows.borrow_mut().push(row);
        } else {
            for group in groups {
                let row = self.application_row(group);
                self.group.add(&row);
                self.rows.borrow_mut().push(row);
            }
        }

        let configured = self
            .controller
            .borrow()
            .config()
            .audio
            .rules
            .iter()
            .filter(|rule| rule.enabled || rule.streams.iter().any(|stream| stream.enabled))
            .count();
        self.configured_label.set_label(&configured.to_string());
    }

    fn application_row(&self, group: ApplicationStreams) -> adw::PreferencesRow {
        let app_name = group
            .identity
            .display_name()
            .unwrap_or("Unknown application");
        let subtitle = format!(
            "{} playback stream(s) · {}",
            group.streams.len(),
            key_label(&group.key)
        );
        let row = adw::ExpanderRow::builder()
            .title(app_name)
            .subtitle(&subtitle)
            .build();
        let app_switch = gtk::Switch::builder()
            .active(
                self.controller
                    .borrow()
                    .application_selected(&group.identity),
            )
            .valign(gtk::Align::Center)
            .build();
        row.add_action(&app_switch);

        let identity = group.identity.clone();
        let dynamic = self.clone();
        app_switch.connect_state_set(move |_, active| {
            dynamic
                .controller
                .borrow_mut()
                .set_application_selected(&identity, active);
            let refresh = dynamic.clone();
            glib::idle_add_local_once(move || refresh.rebuild());
            glib::Propagation::Proceed
        });

        for stream in group.streams {
            let stream_row = adw::ActionRow::builder()
                .title(stream.display_name())
                .subtitle(stream.node_name.as_deref().unwrap_or("Playback stream"))
                .build();
            let stream_switch = gtk::Switch::builder()
                .active(self.controller.borrow().stream_selected(&stream))
                .sensitive(
                    stream
                        .node_name
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                        || stream
                            .media_name
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty()),
                )
                .valign(gtk::Align::Center)
                .build();
            stream_row.add_suffix(&stream_switch);
            stream_row.set_activatable_widget(Some(&stream_switch));

            let dynamic = self.clone();
            stream_switch.connect_state_set(move |_, active| {
                dynamic
                    .controller
                    .borrow_mut()
                    .set_stream_selected(&stream, active);
                let refresh = dynamic.clone();
                glib::idle_add_local_once(move || refresh.rebuild());
                glib::Propagation::Proceed
            });
            row.add_row(&stream_row);
        }
        row.upcast()
    }
}

struct ApplicationStreams {
    key: ApplicationKey,
    identity: ApplicationIdentity,
    streams: Vec<StreamInfo>,
}

fn grouped_streams(streams: &[StreamInfo]) -> Vec<ApplicationStreams> {
    let mut groups: Vec<ApplicationStreams> = Vec::new();
    for stream in streams {
        let Some(key) = stream.application_key() else {
            continue;
        };
        if let Some(group) = groups.iter_mut().find(|group| group.key == key) {
            group.streams.push(stream.clone());
        } else {
            groups.push(ApplicationStreams {
                key,
                identity: stream.application.clone(),
                streams: vec![stream.clone()],
            });
        }
    }
    groups.sort_by_key(|group| {
        group
            .identity
            .display_name()
            .unwrap_or("Unknown application")
            .to_lowercase()
    });
    for group in &mut groups {
        group
            .streams
            .sort_by_key(|stream| stream.display_name().to_lowercase());
    }
    groups
}

fn key_label(key: &ApplicationKey) -> &'static str {
    match key {
        ApplicationKey::ApplicationId(_) => "application ID",
        ApplicationKey::ProcessBinary(_) => "process",
        ApplicationKey::ApplicationName(_) => "application name",
    }
}

fn general_page(
    snapshot: &UiSnapshot,
    controller: Rc<RefCell<ApplicationController>>,
    status_label: gtk::Label,
) -> (adw::PreferencesPage, gtk::Switch) {
    let page = preferences_page("General", "preferences-system-symbolic");
    let group = adw::PreferencesGroup::builder().title("General").build();

    let (live_row, live_switch) = switch_row(
        "Live Subtitles",
        "Capture selected audio and show English subtitles",
        snapshot.live_subtitles,
        true,
    );
    let live_controller = Rc::clone(&controller);
    live_switch.connect_state_set(move |_, active| {
        let command = if active {
            AppCommand::StartSubtitles
        } else {
            AppCommand::StopSubtitles
        };
        if let AppEvent::StateChanged(state) = live_controller.borrow_mut().handle_command(command)
        {
            status_label.set_label(state_label(state));
        }
        glib::Propagation::Proceed
    });
    group.add(&live_row);

    let (keep_running_row, keep_running_switch) = switch_row(
        "Keep Running When Closed",
        "Closing this window hides it without stopping subtitles",
        snapshot.keep_running_when_closed,
        true,
    );
    keep_running_switch.connect_state_set(move |_, active| {
        let _ = controller
            .borrow_mut()
            .handle_command(AppCommand::SetKeepRunningWhenClosed(active));
        glib::Propagation::Proceed
    });
    group.add(&keep_running_row);
    page.add(&group);
    (page, live_switch)
}

fn audio_sources_page(
    snapshot: &UiSnapshot,
    configured_label: &gtk::Label,
    applications_group: &adw::PreferencesGroup,
) -> adw::PreferencesPage {
    let page = preferences_page("Audio Sources", "audio-speakers-symbolic");
    let capture_group = adw::PreferencesGroup::builder().title("Capture").build();
    capture_group.add(&value_row("Capture Mode", &snapshot.capture_mode));
    capture_group.add(&value_row_with_label(
        "Configured Applications",
        configured_label,
    ));
    page.add(&capture_group);
    page.add(applications_group);
    page
}

fn speech_recognition_page(
    snapshot: &UiSnapshot,
    model_path_label: &gtk::Label,
) -> adw::PreferencesPage {
    let page = preferences_page("Speech Recognition", "audio-input-microphone-symbolic");
    let recognition_group = adw::PreferencesGroup::builder()
        .title("Recognition")
        .build();
    recognition_group.add(&value_row("Language", &snapshot.language));
    recognition_group.add(&value_row("Model", &snapshot.model));
    recognition_group.add(&value_row_with_label("Model File", model_path_label));
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
    let group = adw::PreferencesGroup::builder().title("Appearance").build();
    let (visible_row, _) = switch_row(
        "Show Subtitle",
        "Display recognized English speech",
        snapshot.subtitle_visible,
        false,
    );
    group.add(&visible_row);
    group.add(&value_row("Position", &snapshot.subtitle_position));
    group.add(&value_row(
        "Font Size",
        &format!("{} pt", snapshot.font_size),
    ));
    group.add(&value_row(
        "Background Opacity",
        &format!("{}%", snapshot.background_opacity_percent),
    ));
    group.add(&value_row(
        "Maximum Lines",
        &snapshot.maximum_lines.to_string(),
    ));
    page.add(&group);
    page
}

fn performance_page(
    snapshot: &UiSnapshot,
    status_label: &gtk::Label,
    error_label: &gtk::Label,
    audio_buffer_label: &gtk::Label,
    inference_label: &gtk::Label,
    total_label: &gtk::Label,
) -> adw::PreferencesPage {
    let page = preferences_page("Performance", "utilities-system-monitor-symbolic");
    let status_group = adw::PreferencesGroup::builder().title("Pipeline").build();
    status_group.add(&value_row_with_label("Status", status_label));
    status_group.add(&value_row_with_label("Last Error", error_label));
    status_group.add(&value_row("Model", &snapshot.model));
    status_group.add(&value_row("Backend", &snapshot.backend));
    page.add(&status_group);

    let metrics_group = adw::PreferencesGroup::builder().title("Latency").build();
    let (metrics_row, _) = switch_row(
        "Show Metrics",
        "Expose live performance measurements",
        snapshot.show_metrics,
        false,
    );
    metrics_group.add(&metrics_row);
    metrics_group.add(&value_row_with_label("Audio Queues", audio_buffer_label));
    metrics_group.add(&value_row_with_label("STT Inference", inference_label));
    metrics_group.add(&value_row_with_label("Approximate Total", total_label));
    page.add(&metrics_group);
    page
}

fn about_page() -> adw::PreferencesPage {
    let page = preferences_page("About", "help-about-symbolic");
    let group = adw::PreferencesGroup::builder()
        .title("Subtitle-live")
        .build();
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
    value_row_with_label(title, &value_label(value))
}

fn value_label(value: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .label(value)
        .selectable(true)
        .valign(gtk::Align::Center)
        .build();
    label.add_css_class("dim-label");
    label
}

fn value_row_with_label(title: &str, value_label: &gtk::Label) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(value_label);
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

fn format_metric(latest: Option<Duration>, p50: Option<Duration>, p95: Option<Duration>) -> String {
    match (latest, p50, p95) {
        (Some(latest), Some(p50), Some(p95)) => format!(
            "latest {} · p50 {} · p95 {}",
            duration_ms(latest),
            duration_ms(p50),
            duration_ms(p95)
        ),
        _ => "Not available".to_owned(),
    }
}

fn format_dropped(source: u64, mixed: u64) -> String {
    format!("dropped source {source} · mixed {mixed}")
}

fn duration_ms(duration: Duration) -> String {
    format!("{} ms", duration.as_millis())
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
            background_opacity_percent: (config.subtitle.background_opacity * 100.0).round() as u32,
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
