//! การสร้างหน้า settings และเชื่อม widget event เข้ากับ application controller

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::Path,
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk::glib;

use crate::{
    app::{AppCommand, AppEvent, ApplicationController, ApplicationState},
    config::{AppConfig, SUBTITLE_POSITIONS, SUBTITLE_TEXT_ALIGNMENTS},
    debug_log::DebugLogStatus,
    metrics::MetricsSnapshot,
    overlay::{
        DesktopBackendInfo, DesktopSession, DisplayBackend, OverlayClientStatus,
        OverlayMonitorInfo, OverlayRuntimeBackend,
    },
    pipewire::{ApplicationIdentity, ApplicationKey, StreamInfo},
    runtime::DebugDropCounts,
    stt::{SttDebugRecord, compiled_compute_backend},
};

const SETTINGS_WINDOW_NAME: &str = "subtitle-live-settings";
const SUBTITLE_POSITION_LABELS: [&str; 9] = [
    "Top left",
    "Top center",
    "Top right",
    "Center left",
    "Center",
    "Center right",
    "Bottom left",
    "Bottom center",
    "Bottom right",
];
const SUBTITLE_TEXT_ALIGNMENT_LABELS: [&str; 3] = ["Left", "Center", "Right"];
const MAX_DEBUG_BLOCKS: usize = 200;

/// widget และ state ที่จำเป็นต่อการอัปเดตหน้าต่างจาก runtime
#[derive(Clone)]
pub struct SettingsPresenter {
    /// หน้าต่างตั้งค่าหลักที่ GTK main thread เป็นเจ้าของ
    window: adw::PreferencesWindow,
    /// controller ร่วมภายใน GTK main thread เท่านั้น จึงใช้ `Rc<RefCell<_>>`
    controller: Rc<RefCell<ApplicationController>>,
    /// รายการ application ที่สร้างใหม่เมื่อ PipeWire graph เปลี่ยน
    applications: DynamicApplications,
    /// switch ที่ต้อง sync เมื่อ runtime เปลี่ยนสถานะ pipeline
    live_switch: gtk::Switch,
    /// label สถานะและข้อมูล diagnostics ที่ runtime อัปเดตเป็นระยะ
    status_label: gtk::Label,
    error_label: gtk::Label,
    model_path_label: gtk::Label,
    /// สถานะการเชื่อมต่อ PipeWire ที่แสดงในหน้า About
    pipewire_status_label: gtk::Label,
    /// Backend ของ helper ที่อัปเดตตาม lifecycle จริง
    subtitle_backend_label: gtk::Label,
    /// สถานะ XWayland ที่ไม่อ้างเพียง environment
    xwayland_available_label: gtk::Label,
    /// สาเหตุที่ helper ยังไม่พร้อมหรือใช้งานไม่ได้
    overlay_status_label: gtk::Label,
    audio_buffer_label: gtk::Label,
    whisper_label: gtk::Label,
    end_to_end_label: gtk::Label,
    dropped_frames_label: gtk::Label,
    /// ตัวเลือกจอที่อัปเดตจากรายการของ overlay helper เท่านั้น
    monitor_selector: MonitorSelector,
    /// แผงข้อมูลดิบจาก Whisper ที่เปิดเฉพาะเมื่อผู้ใช้ร้องขอใน session ปัจจุบัน
    debug_panel: DebugPanel,
}

impl SettingsPresenter {
    /// สร้างทุกหน้าและเชื่อม signal handler โดยยังไม่แสดงหน้าต่าง
    pub fn new(
        application: &adw::Application,
        controller: Rc<RefCell<ApplicationController>>,
    ) -> Self {
        let session = DesktopSession::detect();
        Self::new_with_backend_info(
            application,
            controller,
            DesktopBackendInfo::new(
                session,
                DisplayBackend::detect(),
                OverlayRuntimeBackend::detect(session),
            ),
        )
    }

    /// สร้างหน้าต่างด้วยข้อมูล backend ที่ main process ยืนยันจากผลเปิด helper แล้ว
    pub fn new_with_backend_info(
        application: &adw::Application,
        controller: Rc<RefCell<ApplicationController>>,
        backend_info: DesktopBackendInfo,
    ) -> Self {
        let snapshot = UiSnapshot::from_controller(&controller.borrow());
        let status_label = value_label(&snapshot.status);
        let error_label = value_label("None");
        let model_path_label = value_label("Not configured");
        let audio_buffer_label = value_label("Not available");
        let whisper_label = value_label("Not available");
        let end_to_end_label = value_label("Not available");
        let dropped_frames_label = value_label("Source 0 · Mixed 0");
        let pipewire_status_label = value_label("Starting");
        let overlay_starting = backend_info.overlay_backend != OverlayRuntimeBackend::Unavailable;
        let subtitle_backend_label = value_label(if overlay_starting {
            "Starting"
        } else {
            "Unavailable"
        });
        let xwayland_available_label = value_label(match backend_info.session {
            DesktopSession::Wayland if overlay_starting => "Starting",
            DesktopSession::Wayland => "No",
            DesktopSession::X11 | DesktopSession::Unknown => "Not applicable",
        });
        let overlay_status_label = value_label(if overlay_starting {
            "Starting overlay helper"
        } else if backend_info.session == DesktopSession::Wayland {
            "Subtitle overlay requires XWayland on this desktop session."
        } else {
            "Subtitle overlay requires an X11 display."
        });
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
        window.add(&speech_recognition_page(
            &snapshot,
            &model_path_label,
            Rc::clone(&controller),
        ));
        let (subtitle_page, monitor_selector) = subtitle_page(&snapshot, Rc::clone(&controller));
        window.add(&subtitle_page);
        window.add(&performance_page(
            &snapshot,
            &status_label,
            &error_label,
            &audio_buffer_label,
            &whisper_label,
            &end_to_end_label,
            &dropped_frames_label,
        ));
        let (debug_page, debug_panel) = debug_page(Rc::clone(&controller));
        window.add(&debug_page);
        window.add(&about_page(
            &snapshot,
            backend_info,
            &pipewire_status_label,
            &subtitle_backend_label,
            &xwayland_available_label,
            &overlay_status_label,
        ));

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
            pipewire_status_label,
            subtitle_backend_label,
            xwayland_available_label,
            overlay_status_label,
            audio_buffer_label,
            whisper_label,
            end_to_end_label,
            dropped_frames_label,
            monitor_selector,
            debug_panel,
        }
    }

    /// แสดงหรือยกหน้าต่างตั้งค่าขึ้นมาด้านหน้า
    pub fn present(&self) {
        self.window.present();
    }

    /// รับ snapshot stream จาก runtime แล้วสร้างรายการเลือกใหม่บน GTK thread
    pub fn update_streams(&self, streams: &[StreamInfo]) {
        self.controller.borrow_mut().set_streams(streams.to_vec());
        self.applications.rebuild();
    }

    /// sync สถานะ pipeline และ error ล่าสุดลงใน widget
    pub fn update_state(&self, state: ApplicationState, error: Option<&str>) {
        self.controller.borrow_mut().set_state(state);
        self.status_label.set_label(state_label(state));
        let live_subtitles = self.controller.borrow().config().general.live_subtitles;
        self.live_switch.set_active(live_subtitles);
        self.error_label.set_label(error.unwrap_or("None"));
    }

    /// แสดง snapshot latency ล่าสุดโดยไม่เก็บประวัติซ้ำในชั้น UI
    pub fn update_metrics(&self, metrics: MetricsSnapshot) {
        self.audio_buffer_label.set_label(&format_metric(
            metrics.audio_buffer_latest,
            metrics.audio_buffer_p50,
            metrics.audio_buffer_p95,
        ));
        self.whisper_label.set_label(&format_metric(
            metrics.whisper_latest,
            metrics.whisper_p50,
            metrics.whisper_p95,
        ));
        self.end_to_end_label.set_label(&format_metric(
            metrics.end_to_end_latest,
            metrics.end_to_end_p50,
            metrics.end_to_end_p95,
        ));
        self.dropped_frames_label.set_label(&format_dropped(
            metrics.source_queue_dropped,
            metrics.mixed_queue_dropped,
        ));
    }

    /// แสดง path โมเดลและผลการตรวจว่ามีไฟล์อยู่จริงหรือไม่
    pub fn update_model_status(&self, path: &Path, exists: bool) {
        let status = if exists {
            "Ready"
        } else {
            "Missing; run make model"
        };
        self.model_path_label
            .set_label(&format!("{} ({status})", path.display()));
    }

    /// แสดงสถานะ PipeWire ล่าสุดในหน้า About
    pub fn update_pipewire_status(&self, status: &str) {
        self.pipewire_status_label.set_label(status);
    }

    /// แสดง backend เฉพาะเมื่อ helper ยืนยัน Ready และแสดง Unavailable เมื่อ IPC ล้มเหลว
    pub fn update_overlay_status(
        &self,
        session: DesktopSession,
        backend: OverlayRuntimeBackend,
        status: OverlayClientStatus,
        error: Option<&str>,
    ) {
        let backend_text = match status {
            OverlayClientStatus::Starting => "Starting",
            OverlayClientStatus::Ready => backend.display_name(),
            OverlayClientStatus::Error | OverlayClientStatus::Stopped => "Unavailable",
        };
        self.subtitle_backend_label.set_label(backend_text);
        let xwayland_text = match session {
            DesktopSession::Wayland
                if status == OverlayClientStatus::Ready
                    && backend == OverlayRuntimeBackend::XWayland =>
            {
                "Yes"
            }
            DesktopSession::Wayland if status == OverlayClientStatus::Starting => "Starting",
            DesktopSession::Wayland => "No",
            DesktopSession::X11 | DesktopSession::Unknown => "Not applicable",
        };
        self.xwayland_available_label.set_label(xwayland_text);
        let status_text = match status {
            OverlayClientStatus::Starting => "Starting overlay helper",
            OverlayClientStatus::Ready => "Ready",
            OverlayClientStatus::Stopped => "Stopped",
            OverlayClientStatus::Error => error.unwrap_or(if session == DesktopSession::Wayland {
                "Subtitle overlay requires XWayland on this desktop session."
            } else {
                "Subtitle overlay requires an X11 display."
            }),
        };
        self.overlay_status_label.set_label(status_text);
    }

    /// แทนที่รายการจอด้วย snapshot จาก helper ซึ่งเป็น process ที่วาด overlay จริง
    pub fn update_overlay_monitors(&self, monitors: &[OverlayMonitorInfo]) {
        self.monitor_selector.update(monitors);
    }

    /// เพิ่มผล inference ใหม่ลงท้ายแผง Debug โดยจำกัดประวัติไม่ให้ UI โตไม่สิ้นสุด
    pub fn append_debug_records(&self, records: &[SttDebugRecord]) {
        self.debug_panel.append(records);
    }

    /// แสดงสถานะไฟล์ log ล่าสุด รวม path และข้อผิดพลาดจาก writer เบื้องหลัง
    pub fn update_debug_log_status(&self, status: &DebugLogStatus) {
        self.debug_panel.update_log_status(status);
    }

    /// แสดงจำนวน debug record สะสมที่ถูกทิ้งจากคิวใน session ปัจจุบัน
    pub fn update_debug_drop_counts(&self, counts: &DebugDropCounts) {
        self.debug_panel.update_drop_counts(counts);
    }
}

/// สร้างและแสดง settings presenter ในขั้นตอนเดียว
pub fn present_settings(
    application: &adw::Application,
    controller: Rc<RefCell<ApplicationController>>,
) -> SettingsPresenter {
    let presenter = SettingsPresenter::new(application, controller);
    presenter.present();
    presenter
}

#[derive(Clone)]
/// state สำหรับสร้างกลุ่ม application/stream ใหม่เมื่อ graph เปลี่ยน
struct DynamicApplications {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<adw::PreferencesRow>>>,
    configured_label: gtk::Label,
    controller: Rc<RefCell<ApplicationController>>,
}

impl DynamicApplications {
    /// ลบ row เก่าและสร้าง row จาก snapshot ปัจจุบันทั้งหมดบน GTK thread
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

    /// สร้าง expander หนึ่งชุดพร้อม switch ระดับ application และราย stream
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

/// playback stream ที่จัดกลุ่มตาม stable application key สำหรับแสดงใน UI
struct ApplicationStreams {
    key: ApplicationKey,
    identity: ApplicationIdentity,
    streams: Vec<StreamInfo>,
}

/// รวม stream ตาม application และเรียงชื่อเพื่อให้ลำดับ UI คงที่
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

/// อธิบายชนิด stable key ที่นำมาใช้จับคู่
fn key_label(key: &ApplicationKey) -> &'static str {
    match key {
        ApplicationKey::ApplicationId(_) => "application ID",
        ApplicationKey::ProcessBinary(_) => "process",
        ApplicationKey::ApplicationName(_) => "application name",
    }
}

/// สร้างหน้า General และเชื่อม switch เข้ากับคำสั่งเริ่ม/หยุด pipeline
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

/// สร้างหน้ารายการแหล่งเสียงที่ค้นพบและกฎที่เลือกไว้
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

/// สร้างหน้าข้อมูลโมเดลและตัวควบคุมการส่งเสียงแบบ streaming
fn speech_recognition_page(
    snapshot: &UiSnapshot,
    model_path_label: &gtk::Label,
    controller: Rc<RefCell<ApplicationController>>,
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
    let (step_row, step) = spin_row(
        "Audio Step",
        snapshot.audio_step_ms,
        50,
        1_000,
        25,
        Some("ms"),
    );
    let step_controller = Rc::clone(&controller);
    step.connect_value_changed(move |spin| {
        let _ = step_controller
            .borrow_mut()
            .handle_command(AppCommand::SetSttStepMs(spin.value_as_int() as u32));
    });
    streaming_group.add(&step_row);

    let (window_row, window) = spin_row(
        "Context Window",
        snapshot.context_window_ms,
        1_000,
        30_000,
        250,
        Some("ms"),
    );
    let window_controller = Rc::clone(&controller);
    window.connect_value_changed(move |spin| {
        let _ = window_controller
            .borrow_mut()
            .handle_command(AppCommand::SetSttWindowMs(spin.value_as_int() as u32));
    });
    streaming_group.add(&window_row);

    let (vad_row, vad_switch) = switch_row(
        "Audio Activity Filter",
        "Skip low-energy audio before recognition; this does not identify speech",
        snapshot.vad_enabled,
        true,
    );
    vad_switch.connect_state_set(move |_, enabled| {
        let _ = controller
            .borrow_mut()
            .handle_command(AppCommand::SetVadEnabled(enabled));
        glib::Propagation::Proceed
    });
    streaming_group.add(&vad_row);
    page.add(&streaming_group);
    page
}

/// สร้างหน้าปรับ overlay และส่งทุกการเปลี่ยนผ่าน controller
fn subtitle_page(
    snapshot: &UiSnapshot,
    controller: Rc<RefCell<ApplicationController>>,
) -> (adw::PreferencesPage, MonitorSelector) {
    let page = preferences_page("Subtitle", "insert-text-symbolic");
    let group = adw::PreferencesGroup::builder().title("Appearance").build();
    let (visible_row, visible_switch) = switch_row(
        "Show Subtitle",
        "Display recognized English speech",
        snapshot.subtitle_visible,
        true,
    );
    let visible_controller = Rc::clone(&controller);
    visible_switch.connect_state_set(move |_, visible| {
        let _ = visible_controller
            .borrow_mut()
            .handle_command(AppCommand::SetSubtitleVisible(visible));
        glib::Propagation::Proceed
    });
    group.add(&visible_row);

    let (monitor_row, monitor_selector) = MonitorSelector::new(Rc::clone(&controller));
    group.add(&monitor_row);

    let selected_position = SUBTITLE_POSITIONS
        .iter()
        .position(|position| *position == snapshot.subtitle_position)
        .unwrap_or(7) as u32;
    let position_dropdown = gtk::DropDown::from_strings(&SUBTITLE_POSITION_LABELS);
    position_dropdown.set_selected(selected_position);
    position_dropdown.set_valign(gtk::Align::Center);
    let position_row = adw::ActionRow::builder()
        .activatable_widget(&position_dropdown)
        .title("Position")
        .build();
    position_row.add_suffix(&position_dropdown);
    let position_controller = Rc::clone(&controller);
    position_dropdown.connect_selected_notify(move |dropdown| {
        let Some(position) = SUBTITLE_POSITIONS.get(dropdown.selected() as usize) else {
            return;
        };
        let _ = position_controller
            .borrow_mut()
            .handle_command(AppCommand::SetSubtitlePosition((*position).to_owned()));
    });
    group.add(&position_row);

    let selected_alignment = SUBTITLE_TEXT_ALIGNMENTS
        .iter()
        .position(|alignment| *alignment == snapshot.subtitle_text_alignment)
        .unwrap_or(0) as u32;
    let alignment_dropdown = gtk::DropDown::from_strings(&SUBTITLE_TEXT_ALIGNMENT_LABELS);
    alignment_dropdown.set_selected(selected_alignment);
    alignment_dropdown.set_valign(gtk::Align::Center);
    let alignment_row = adw::ActionRow::builder()
        .activatable_widget(&alignment_dropdown)
        .title("Text Alignment")
        .build();
    alignment_row.add_suffix(&alignment_dropdown);
    let alignment_controller = Rc::clone(&controller);
    alignment_dropdown.connect_selected_notify(move |dropdown| {
        let Some(alignment) = SUBTITLE_TEXT_ALIGNMENTS.get(dropdown.selected() as usize) else {
            return;
        };
        let _ =
            alignment_controller
                .borrow_mut()
                .handle_command(AppCommand::SetSubtitleTextAlignment(
                    (*alignment).to_owned(),
                ));
    });
    group.add(&alignment_row);

    let (font_size_row, font_size) =
        spin_row("Font Size", snapshot.font_size, 16, 72, 1, Some("pt"));
    let font_size_controller = Rc::clone(&controller);
    font_size.connect_value_changed(move |spin| {
        let _ = font_size_controller
            .borrow_mut()
            .handle_command(AppCommand::SetSubtitleFontSize(spin.value_as_int() as u32));
    });
    group.add(&font_size_row);

    let (width_row, width) = spin_row(
        "Maximum Width",
        snapshot.subtitle_width_px,
        320,
        3_840,
        20,
        Some("px"),
    );
    let width_controller = Rc::clone(&controller);
    width.connect_value_changed(move |spin| {
        let _ = width_controller
            .borrow_mut()
            .handle_command(AppCommand::SetSubtitleWidthPx(spin.value_as_int() as u32));
    });
    group.add(&width_row);

    let (opacity_row, opacity) = spin_row(
        "Background Opacity",
        snapshot.background_opacity_percent,
        0,
        100,
        1,
        Some("%"),
    );
    let opacity_controller = Rc::clone(&controller);
    opacity.connect_value_changed(move |spin| {
        let _ = opacity_controller.borrow_mut().handle_command(
            AppCommand::SetSubtitleBackgroundOpacityPercent(spin.value_as_int() as u32),
        );
    });
    group.add(&opacity_row);

    let (max_lines_row, max_lines) =
        spin_row("Maximum Lines", snapshot.maximum_lines, 1, 5, 1, None);
    max_lines.connect_value_changed(move |spin| {
        let _ = controller
            .borrow_mut()
            .handle_command(AppCommand::SetSubtitleMaxLines(spin.value_as_int() as u32));
    });
    group.add(&max_lines_row);
    page.add(&group);
    (page, monitor_selector)
}

#[derive(Clone)]
/// ตัวเลือกจอที่เก็บ ID แยกจากข้อความ เพื่อส่ง identity เดิมกลับ helper โดยไม่ตีความ label
struct MonitorSelector {
    dropdown: gtk::DropDown,
    monitor_ids: Rc<RefCell<Vec<String>>>,
    updating: Rc<Cell<bool>>,
    controller: Rc<RefCell<ApplicationController>>,
}

impl MonitorSelector {
    /// สร้าง dropdown สถานะเริ่มต้นระหว่างรอ helper รายงานจอ
    fn new(controller: Rc<RefCell<ApplicationController>>) -> (adw::ActionRow, Self) {
        let dropdown = gtk::DropDown::from_strings(&["Detecting displays…"]);
        dropdown.set_sensitive(false);
        dropdown.set_valign(gtk::Align::Center);
        let row = adw::ActionRow::builder()
            .activatable_widget(&dropdown)
            .title("Display")
            .subtitle("Screen used for the subtitle overlay")
            .build();
        row.add_suffix(&dropdown);
        let selector = Self {
            dropdown,
            monitor_ids: Rc::new(RefCell::new(Vec::new())),
            updating: Rc::new(Cell::new(false)),
            controller,
        };
        let signal_selector = selector.clone();
        selector.dropdown.connect_selected_notify(move |dropdown| {
            if signal_selector.updating.get() {
                return;
            }
            let Some(monitor_id) = signal_selector
                .monitor_ids
                .borrow()
                .get(dropdown.selected() as usize)
                .cloned()
            else {
                return;
            };
            let _ = signal_selector
                .controller
                .borrow_mut()
                .handle_command(AppCommand::SetSubtitleMonitor(monitor_id));
        });
        (row, selector)
    }

    /// เปลี่ยน model และ selection พร้อม guard ไม่ให้การ sync UI เขียน config กลับเอง
    fn update(&self, monitors: &[OverlayMonitorInfo]) {
        self.updating.set(true);
        let labels = if monitors.is_empty() {
            vec!["No displays detected"]
        } else {
            monitors
                .iter()
                .map(|monitor| monitor.label.as_str())
                .collect::<Vec<_>>()
        };
        self.dropdown
            .set_model(Some(&gtk::StringList::new(&labels)));
        self.monitor_ids
            .replace(monitors.iter().map(|monitor| monitor.id.clone()).collect());
        let configured_id = self
            .controller
            .borrow()
            .config()
            .subtitle
            .monitor_id
            .clone();
        let configured_position = monitors
            .iter()
            .position(|monitor| !configured_id.is_empty() && monitor.id == configured_id);
        let selected = configured_position.unwrap_or(0) as u32;
        self.dropdown.set_selected(selected);
        self.dropdown.set_sensitive(!monitors.is_empty());
        self.updating.set(false);
        if configured_position.is_none()
            && let Some(first_monitor) = monitors.first()
        {
            // เมื่อจอเดิมหายหรือยังเป็นค่า auto ให้ค่าที่ persist ตรงกับ fallback ที่ helper ใช้จริง
            let _ = self
                .controller
                .borrow_mut()
                .handle_command(AppCommand::SetSubtitleMonitor(first_monitor.id.clone()));
        }
    }
}

/// สร้างหน้าสถานะ pipeline และ latency ที่ runtime วัดได้
fn performance_page(
    snapshot: &UiSnapshot,
    status_label: &gtk::Label,
    error_label: &gtk::Label,
    audio_buffer_label: &gtk::Label,
    whisper_label: &gtk::Label,
    end_to_end_label: &gtk::Label,
    dropped_frames_label: &gtk::Label,
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
    metrics_group.add(&value_row_with_label("Audio Buffer", audio_buffer_label));
    metrics_group.add(&value_row_with_label("Whisper Inference", whisper_label));
    metrics_group.add(&value_row_with_label("End-to-End", end_to_end_label));
    metrics_group.add(&value_row_with_label(
        "Dropped Frames",
        dropped_frames_label,
    ));
    page.add(&metrics_group);
    page
}

#[derive(Clone)]
/// widget และประวัติแบบจำกัดขนาดของหน้า Debug ซึ่งมีอายุเท่ากับหน้าต่าง Settings
struct DebugPanel {
    buffer: gtk::TextBuffer,
    view: gtk::TextView,
    blocks: Rc<RefCell<VecDeque<String>>>,
    file_status_label: gtk::Label,
    file_path_label: gtk::Label,
    dropped_records_label: gtk::Label,
}

impl DebugPanel {
    /// เพิ่มหนึ่ง block ต่อ Whisper inference ไม่ใช่หนึ่ง block ต่อคำ
    fn append(&self, records: &[SttDebugRecord]) {
        if records.is_empty() {
            return;
        }
        let mut blocks = self.blocks.borrow_mut();
        for record in records {
            blocks.push_back(format_debug_record(record));
            if blocks.len() > MAX_DEBUG_BLOCKS {
                blocks.pop_front();
            }
        }
        self.buffer.set_text(
            &blocks
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
        drop(blocks);

        // รอให้ GTK คำนวณ layout ของข้อความใหม่ก่อนเลื่อนไปยังผลล่าสุด
        let buffer = self.buffer.clone();
        let view = self.view.clone();
        glib::idle_add_local_once(move || {
            let mut end = buffer.end_iter();
            view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
        });
    }

    /// ล้างเฉพาะข้อความบนหน้าจอ โดยไม่กระทบไฟล์ log หรือ pipeline
    fn clear(&self) {
        self.blocks.borrow_mut().clear();
        self.buffer.set_text("");
    }

    /// แปลงสถานะ writer เป็นข้อความที่ผู้ใช้ตรวจ path และ error ได้โดยตรง
    fn update_log_status(&self, status: &DebugLogStatus) {
        match status {
            DebugLogStatus::Disabled { path } => {
                self.file_status_label.set_label("Disabled");
                self.file_path_label.set_label(&path.display().to_string());
            }
            DebugLogStatus::Writing { path } => {
                self.file_status_label.set_label("Writing");
                self.file_path_label.set_label(&path.display().to_string());
            }
            DebugLogStatus::Error { path, message } => {
                self.file_status_label.set_label(&format!("Error: {message}"));
                self.file_path_label.set_label(&path.display().to_string());
            }
        }
    }

    /// แสดงตัวนับสะสมแยกคิว STT และคิว writer เพื่อบอกเมื่อ debug รับข้อมูลไม่ทัน
    fn update_drop_counts(&self, counts: &DebugDropCounts) {
        self.dropped_records_label.set_label(&format!(
            "STT {} · File {}",
            counts.stt_queue, counts.file_queue
        ));
    }
}

/// สร้างหน้า Debug ซึ่งปิดทั้ง live view และ file logging ทุกครั้งที่เปิดโปรแกรม
fn debug_page(
    controller: Rc<RefCell<ApplicationController>>,
) -> (adw::PreferencesPage, DebugPanel) {
    let page = preferences_page("Debug", "utilities-terminal-symbolic");

    let controls_group = adw::PreferencesGroup::builder()
        .title("Debug Controls")
        .description("Debug collection is disabled when Subtitle-live launches.")
        .build();
    let (live_row, live_switch) = switch_row(
        "Live Debug View",
        "Show each Whisper inference as it reaches the subtitle pipeline",
        false,
        true,
    );
    let (file_row, file_switch) = switch_row(
        "Write Debug Log File",
        "Save the same inference records to a local log file",
        false,
        true,
    );
    controls_group.add(&live_row);
    controls_group.add(&file_row);
    page.add(&controls_group);

    let file_status_label = value_label("Disabled");
    let file_path_label = value_label("Waiting for runtime");
    let dropped_records_label = value_label("STT 0 · File 0");
    let file_group = adw::PreferencesGroup::builder()
        .title("Log File")
        .build();
    file_group.add(&value_row_with_label("Status", &file_status_label));
    file_group.add(&value_row_with_label("Path", &file_path_label));
    file_group.add(&value_row_with_label(
        "Dropped Debug Records",
        &dropped_records_label,
    ));
    page.add(&file_group);

    let explanation_group = adw::PreferencesGroup::builder()
        .title("How Streaming Recognition Works")
        .build();
    explanation_group.add(&message_row(
        "One rolling chunk per inference",
        "Whisper processes a rolling audio window and returns a complete hypothesis each time. It does not send one word at a time, so later hypotheses may revise earlier words before the segment becomes final.",
    ));
    explanation_group.add(&message_row(
        "Backend timing",
        "full() wall is the elapsed wall time around WhisperState::full, not GPU kernel time. The current API reports the requested backend but cannot confirm which backend actually ran.",
    ));
    page.add(&explanation_group);

    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .cursor_visible(false)
        .editable(false)
        .left_margin(12)
        .monospace(true)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .has_frame(true)
        .height_request(360)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let clear_button = gtk::Button::builder()
        .label("Clear")
        .valign(gtk::Align::Center)
        .build();
    let output_header = adw::ActionRow::builder()
        .activatable(false)
        .title("Latest Inferences")
        .subtitle("Up to 200 inference blocks")
        .build();
    output_header.add_suffix(&clear_button);
    let output_group = adw::PreferencesGroup::builder()
        .title("Inference Stream")
        .build();
    output_group.add(&output_header);
    output_group.add(&scrolled);
    page.add(&output_group);

    let panel = DebugPanel {
        buffer,
        view,
        blocks: Rc::new(RefCell::new(VecDeque::new())),
        file_status_label,
        file_path_label,
        dropped_records_label,
    };
    let clear_panel = panel.clone();
    clear_button.connect_clicked(move |_| clear_panel.clear());

    let live_controller = Rc::clone(&controller);
    let live_panel = panel.clone();
    live_switch.connect_state_set(move |_, enabled| {
        let _ = live_controller
            .borrow_mut()
            .handle_command(AppCommand::SetDebugLiveEnabled(enabled));
        if !enabled {
            live_panel.clear();
        }
        glib::Propagation::Proceed
    });
    file_switch.connect_state_set(move |_, enabled| {
        let _ = controller
            .borrow_mut()
            .handle_command(AppCommand::SetDebugFileLoggingEnabled(enabled));
        glib::Propagation::Proceed
    });

    (page, panel)
}

/// จัดรูปแบบ record เป็นบรรทัดคงที่ เพื่อแยกข้อความดิบและข้อความที่เข้า subtitle ชัดเจน
fn format_debug_record(record: &SttDebugRecord) -> String {
    let backend_request = if record.gpu_backend_requested {
        format!("GPU device {} (actual backend unknown)", record.gpu_device)
    } else {
        "CPU requested".to_owned()
    };
    format!(
        "[{}] segment={} kind={} generation={} | whisper={} | full() wall={} | request={} | step={} ms window={} ms\nRAW: {}\nNORMALIZED: {}\nSUBTITLE INPUT: {}",
        local_timestamp(record.timestamp_unix_ms),
        record.segment_id,
        record.kind.as_str(),
        record.audio_generation,
        precise_duration_ms(record.whisper_model_duration),
        precise_duration_ms(record.backend_full_duration),
        backend_request,
        record.step_ms,
        record.window_ms,
        escape_debug_text(&record.raw_text),
        escape_debug_text(&record.processed_text),
        escape_debug_text(&record.emitted_text),
    )
}

/// แสดง Unix timestamp เป็นเวลาท้องถิ่นพร้อม millisecond โดยไม่เพิ่ม dependency ใหม่
fn local_timestamp(timestamp_unix_ms: u64) -> String {
    let seconds = i64::try_from(timestamp_unix_ms / 1_000).unwrap_or(i64::MAX);
    let milliseconds = timestamp_unix_ms % 1_000;
    glib::DateTime::from_unix_local(seconds)
        .and_then(|timestamp| timestamp.format("%Y-%m-%d %H:%M:%S"))
        .map_or_else(
            |_| timestamp_unix_ms.to_string(),
            |timestamp| format!("{timestamp}.{milliseconds:03}"),
        )
}

/// วัด latency แบบทศนิยมเพื่อให้รอบ GPU ที่ต่ำกว่า 1 ms ไม่ถูกปัดเป็นศูนย์
fn precise_duration_ms(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
}

/// ทำให้ newline ภายในข้อความหนึ่งค่าไม่ทำลายโครงบรรทัดของ debug block
fn escape_debug_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

/// สร้างหน้าข้อมูลรุ่นและขอบเขตความเป็นส่วนตัว
fn about_page(
    snapshot: &UiSnapshot,
    backend_info: DesktopBackendInfo,
    pipewire_status_label: &gtk::Label,
    subtitle_backend_label: &gtk::Label,
    xwayland_available_label: &gtk::Label,
    overlay_status_label: &gtk::Label,
) -> adw::PreferencesPage {
    let page = preferences_page("About", "help-about-symbolic");
    let application_group = adw::PreferencesGroup::builder()
        .title("Subtitle-live")
        .build();
    application_group.add(&message_row(
        "Local English Live Subtitles",
        "Captures selected application audio and processes it locally.",
    ));
    application_group.add(&value_row("Version", env!("CARGO_PKG_VERSION")));
    application_group.add(&value_row("Privacy", "Local-first"));
    page.add(&application_group);

    let desktop_group = adw::PreferencesGroup::builder().title("Desktop").build();
    desktop_group.add(&value_row("Session", backend_info.session.display_name()));
    desktop_group.add(&value_row(
        "Settings Backend",
        backend_info.settings_backend.display_name(),
    ));
    desktop_group.add(&value_row_with_label(
        "Subtitle Backend",
        subtitle_backend_label,
    ));
    desktop_group.add(&value_row_with_label(
        "XWayland Available",
        xwayland_available_label,
    ));
    desktop_group.add(&value_row_with_label(
        "Overlay Status",
        overlay_status_label,
    ));
    page.add(&desktop_group);

    let recognition_group = adw::PreferencesGroup::builder()
        .title("Speech Recognition")
        .build();
    recognition_group.add(&value_row("Model", &snapshot.model));
    recognition_group.add(&value_row("Compute Backend", &snapshot.backend));
    page.add(&recognition_group);

    let audio_group = adw::PreferencesGroup::builder().title("Audio").build();
    audio_group.add(&value_row_with_label("PipeWire", pipewire_status_label));
    page.add(&audio_group);
    page
}

/// สร้างโครงหน้า preferences ที่ใช้รูปแบบเดียวกันทุกหน้า
fn preferences_page(title: &str, icon_name: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder()
        .icon_name(icon_name)
        .title(title)
        .build()
}

/// สร้าง action row พร้อม switch และคืนทั้งคู่เพื่อเชื่อม signal ภายหลัง
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

/// สร้าง row สำหรับค่าข้อความแบบอ่านอย่างเดียว
fn value_row(title: &str, value: &str) -> adw::ActionRow {
    value_row_with_label(title, &value_label(value))
}

/// สร้าง label ค่าที่เลือกและคัดลอกได้
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

/// ผูก label ที่มีอยู่เข้ากับ action row
fn value_row_with_label(title: &str, value_label: &gtk::Label) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(value_label);
    row
}

/// สร้างตัวเลขปรับค่าได้พร้อมช่วงและหน่วยที่กำหนด
fn spin_row(
    title: &str,
    value: u32,
    minimum: u32,
    maximum: u32,
    increment: u32,
    unit: Option<&str>,
) -> (adw::ActionRow, gtk::SpinButton) {
    let spin = gtk::SpinButton::with_range(minimum as f64, maximum as f64, increment as f64);
    spin.set_value(value as f64);
    spin.set_valign(gtk::Align::Center);
    spin.set_width_chars(maximum.to_string().len() as i32);
    let row = adw::ActionRow::builder()
        .activatable_widget(&spin)
        .title(title)
        .build();
    row.add_suffix(&spin);
    if let Some(unit) = unit {
        row.add_suffix(&gtk::Label::new(Some(unit)));
    }
    (row, spin)
}

/// สร้าง row ข้อความที่ไม่ตอบสนองต่อการคลิก
fn message_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .activatable(false)
        .subtitle(subtitle)
        .title(title)
        .build()
}

/// จัดรูปแบบ latency ล่าสุด ค่า p50 และ p95 สำหรับหน้า Performance
fn format_metric(latest: Option<Duration>, p50: Option<Duration>, p95: Option<Duration>) -> String {
    match (latest, p50, p95) {
        (Some(latest), Some(p50), Some(p95)) => format!(
            "Latest {} · P50 {} · P95 {}",
            duration_ms(latest),
            duration_ms(p50),
            duration_ms(p95)
        ),
        _ => "Not available".to_owned(),
    }
}

/// สรุปจำนวน audio frame ที่ถูกทิ้งจากคิวแต่ละช่วง
fn format_dropped(source: u64, mixed: u64) -> String {
    format!("Source {source} · Mixed {mixed}")
}

/// แปลง duration เป็นข้อความ millisecond
fn duration_ms(duration: Duration) -> String {
    format!("{} ms", duration.as_millis())
}

/// snapshot แบบพร้อมแสดงผลที่แยก GTK widget ออกจาก schema การตั้งค่า
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
    subtitle_text_alignment: String,
    font_size: u32,
    subtitle_width_px: u32,
    background_opacity_percent: u32,
    maximum_lines: u32,
    show_metrics: bool,
    status: String,
}

impl UiSnapshot {
    /// อ่าน controller ครั้งเดียวแล้วแปลงค่าทั้งหมดเป็นข้อมูลสำหรับสร้าง UI
    fn from_controller(controller: &ApplicationController) -> Self {
        let config = controller.config();
        Self {
            live_subtitles: config.general.live_subtitles,
            keep_running_when_closed: config.general.keep_running_when_closed,
            capture_mode: capture_mode_label(config),
            configured_applications: config.audio.rules.len(),
            language: language_label(config),
            model: config.stt.model.clone(),
            // แสดง backend ที่ไบนารีใช้จริง ไม่ใช่ค่าความต้องการ `auto` ใน config
            backend: compiled_compute_backend().display_name().to_owned(),
            audio_step_ms: config.stt.step_ms,
            context_window_ms: config.stt.window_ms,
            vad_enabled: config.stt.vad_enabled,
            subtitle_visible: config.subtitle.visible,
            subtitle_position: config.subtitle.position.clone(),
            subtitle_text_alignment: config.subtitle.text_alignment.clone(),
            font_size: config.subtitle.font_size,
            subtitle_width_px: config.subtitle.width_px,
            background_opacity_percent: (config.subtitle.background_opacity * 100.0).round() as u32,
            maximum_lines: config.subtitle.max_lines,
            show_metrics: config.performance.show_metrics,
            status: state_label(controller.state()).to_owned(),
        }
    }
}

/// แปลงรหัส capture mode เป็นชื่อสำหรับผู้ใช้
fn capture_mode_label(config: &AppConfig) -> String {
    match config.audio.capture_mode.as_str() {
        "selected" => "Selected Applications".to_owned(),
        other => display_identifier(other),
    }
}

/// แปลงรหัสภาษาเป็นชื่อสำหรับผู้ใช้
fn language_label(config: &AppConfig) -> String {
    match config.stt.language.as_str() {
        "en" => "English".to_owned(),
        other => display_identifier(other),
    }
}

/// เปลี่ยน identifier แบบ kebab/snake case ให้เป็นข้อความอ่านง่าย
fn display_identifier(value: &str) -> String {
    let words = value.replace(['-', '_'], " ");
    let mut characters = words.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// แปลง enum สถานะเป็นข้อความสั้นสำหรับ UI
const fn state_label(state: ApplicationState) -> &'static str {
    match state {
        ApplicationState::Stopped => "Stopped",
        ApplicationState::Starting => "Starting",
        ApplicationState::Running => "Running",
        ApplicationState::Stopping => "Stopping",
        ApplicationState::Error => "Error",
    }
}
