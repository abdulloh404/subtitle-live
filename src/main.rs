//! จุดเริ่มต้นของแอป GTK และสะพานส่ง event จาก runtime ไปยังหน้าต่าง Settings/Overlay

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use gtk::glib;
use subtitle_live::{
    app::ApplicationController,
    config,
    error::AppError,
    logging,
    overlay::{
        DesktopBackendInfo, DesktopSession, DisplayBackend, OverlayClient, OverlayClientStatus,
        OverlayEvent, OverlayRuntimeBackend, run_overlay_helper,
    },
    runtime::ApplicationRuntime,
    ui::SettingsPresenter,
};

/// รวม presenter ที่ต้องมีอายุเท่ากับ desktop application
struct DesktopUi {
    /// หน้าต่างตั้งค่าและสถานะ pipeline
    settings: SettingsPresenter,
    /// หน้าต่างโปร่งใสที่แสดง subtitle เหนือหน้าจอ
    overlay: DesktopOverlay,
}

/// Renderer ที่ main process เลือกตาม desktop session โดยไม่เปลี่ยน backend ของ Settings
enum DesktopOverlay {
    /// Main process ส่งข้อความให้ X11/XWayland helper
    Helper(Box<OverlayClient>),
    /// ไม่มี X11 display; pipeline อื่นยังทำงานต่อได้
    Unavailable,
}

/// ข้อมูลจับคู่ frame กับนาฬิกา monotonic ก่อนส่งเข้า IPC
struct OverlayFrameSubmission {
    frame_id: u64,
    audio_origin_micros: i64,
}

impl DesktopOverlay {
    /// นำค่ารูปลักษณ์ไปใช้กับ renderer ที่พร้อมใช้งาน
    fn apply_config(&self, config: &config::SubtitleConfig) {
        match self {
            Self::Helper(client) => client.apply_config(config),
            Self::Unavailable => {}
        }
    }

    /// แสดง subtitle โดย helper รับเฉพาะข้อความที่ runtime จัดการแล้ว
    fn show_text(
        &self,
        text: &str,
        is_final: bool,
        audio_origin_at: Instant,
    ) -> Option<OverlayFrameSubmission> {
        match self {
            Self::Helper(client) => {
                let now = Instant::now();
                let submitted_at_micros = glib::monotonic_time();
                let audio_age_micros =
                    i64::try_from(now.saturating_duration_since(audio_origin_at).as_micros())
                        .unwrap_or(i64::MAX);
                client
                    .show_text(text, is_final)
                    .map(|frame_id| OverlayFrameSubmission {
                        frame_id,
                        audio_origin_micros: submitted_at_micros.saturating_sub(audio_age_micros),
                    })
            }
            Self::Unavailable => None,
        }
    }

    /// ล้างข้อความปัจจุบันโดยไม่หยุด audio/STT pipeline
    fn hide(&self) {
        match self {
            Self::Helper(client) => client.hide(),
            Self::Unavailable => {}
        }
    }

    /// รับสถานะ helper แบบไม่ block และบันทึกเฉพาะ lifecycle/error
    fn poll_events(&self) -> Vec<OverlayEvent> {
        let Self::Helper(client) = self else {
            return Vec::new();
        };
        let events = client.drain_events();
        for event in &events {
            match event {
                OverlayEvent::Ready => tracing::info!(
                    overlay_backend = client.backend().as_str(),
                    "Subtitle overlay helper ready"
                ),
                OverlayEvent::Error { message } => tracing::warn!(
                    overlay_backend = client.backend().as_str(),
                    error = %message,
                    "Subtitle overlay helper unavailable"
                ),
                OverlayEvent::MonitorsChanged { .. }
                | OverlayEvent::Pong { .. }
                | OverlayEvent::Rendered { .. } => {}
            }
        }
        events
    }

    /// คืนสถานะ renderer ล่าสุดสำหรับหน้า About
    fn status(&self) -> OverlayClientStatus {
        match self {
            Self::Helper(client) => client.status(),
            Self::Unavailable => OverlayClientStatus::Error,
        }
    }

    /// คืน error ของ helper โดยไม่รวมข้อความ subtitle
    fn last_error(&self) -> Option<String> {
        match self {
            Self::Helper(client) => client.last_error(),
            Self::Unavailable => None,
        }
    }

    /// ขอให้ renderer หยุดโดยไม่ block GTK shutdown callback
    fn request_shutdown(&self) {
        match self {
            Self::Helper(client) => client.shutdown(),
            Self::Unavailable => {}
        }
    }

    /// รอ helper ปิดหลัง GTK event loop จบแล้ว เพื่อไม่ทิ้ง child หรือ IPC worker
    fn finish(&self) {
        let Self::Helper(client) = self else {
            return;
        };
        if let Err(error) = client.finish() {
            tracing::warn!(error = %error, "Subtitle overlay helper did not shut down cleanly");
        }
    }
}

/// โหลด config, สร้าง GTK application และปิด worker อย่างเป็นระเบียบเมื่อ event loop จบ
fn main() -> Result<(), AppError> {
    // โหมดนี้เป็น child process เท่านั้น จึงห้ามติดตั้ง logger ที่เขียน JSON ลง stdout ของ IPC
    if std::env::args_os().any(|argument| argument == "--overlay-helper") {
        return run_overlay_helper().map_err(|error| AppError::Overlay(error.to_string()));
    }

    // เริ่ม tracing ก่อนทำ I/O เพื่อให้ข้อผิดพลาดช่วง startup ปรากฏใน terminal
    logging::init()?;

    // ถ้า config เสีย แอปยังเปิดได้ด้วยค่า default แต่จะไม่เขียนทับไฟล์เดิมโดยไม่ตั้งใจ
    let config_path = config::default_path()?;
    let loaded = config::load_config(&config_path)?;
    let persistence_enabled = loaded.warning.is_none();
    let startup_warning = loaded.warning.map(|warning| {
        tracing::warn!(
            config_path = %config_path.display(),
            warning = %warning,
            "Configuration could not be loaded"
        );
        warning.to_string()
    });
    let default_model_path = config::default_model_path()?;
    let controller = Rc::new(RefCell::new(ApplicationController::new(loaded.config)));
    let model_path = controller
        .borrow()
        .config()
        .stt
        .model_path
        .clone()
        .unwrap_or_else(|| default_model_path.clone());
    let model_ready = model_path.is_file();
    let runtime = Rc::new(RefCell::new(None::<ApplicationRuntime>));
    let desktop_session = DesktopSession::detect();
    let overlay_backend = OverlayRuntimeBackend::detect(desktop_session);
    let initial_subtitle_config = controller.borrow().config().subtitle.clone();
    let overlay_client = Rc::new(RefCell::new(match overlay_backend {
        OverlayRuntimeBackend::XWayland | OverlayRuntimeBackend::X11 => {
            match OverlayClient::spawn(overlay_backend, &initial_subtitle_config) {
                Ok(client) => Some(client),
                Err(error) => {
                    tracing::warn!(
                        overlay_backend = overlay_backend.as_str(),
                        error = %error,
                        "Could not start subtitle overlay helper; the main pipeline remains available"
                    );
                    None
                }
            }
        }
        OverlayRuntimeBackend::Unavailable => None,
    }));
    let effective_overlay_backend = if overlay_client.borrow().is_some() {
        overlay_backend
    } else {
        OverlayRuntimeBackend::Unavailable
    };

    tracing::info!(
        state = ?controller.borrow().state(),
        config_path = %config_path.display(),
        desktop_session = desktop_session.as_str(),
        settings_backend = desktop_session.as_str(),
        overlay_backend = effective_overlay_backend.as_str(),
        xwayland_available = effective_overlay_backend == OverlayRuntimeBackend::XWayland,
        "Subtitle-live initialized"
    );

    let application = adw::Application::builder()
        .application_id("io.github.subtitle_live")
        .build();
    let desktop = Rc::new(RefCell::new(None::<DesktopUi>));

    let startup_controller = Rc::clone(&controller);
    let startup_runtime = Rc::clone(&runtime);
    let startup_config_path = config_path.clone();
    application.connect_startup(move |_| {
        // สร้าง worker เพียงครั้งเดียว แม้ desktop environment จะ activate แอปซ้ำ
        if startup_runtime.borrow().is_none() {
            *startup_runtime.borrow_mut() = Some(ApplicationRuntime::new(
                Rc::clone(&startup_controller),
                startup_config_path.clone(),
                default_model_path.clone(),
                model_ready,
                persistence_enabled,
                startup_warning.clone(),
            ));
        }
    });

    let activate_controller = Rc::clone(&controller);
    let activate_runtime = Rc::clone(&runtime);
    let activate_desktop = Rc::clone(&desktop);
    let activate_overlay_client = Rc::clone(&overlay_client);
    application.connect_activate(move |application| {
        // Presenter เป็น GTK object จึงต้องสร้างและใช้งานบน main thread เท่านั้น
        if activate_desktop.borrow().is_none() {
            let settings = SettingsPresenter::new_with_backend_info(
                application,
                Rc::clone(&activate_controller),
                DesktopBackendInfo::new(
                    desktop_session,
                    DisplayBackend::detect(),
                    effective_overlay_backend,
                ),
            );
            let overlay = match overlay_backend {
                OverlayRuntimeBackend::XWayland | OverlayRuntimeBackend::X11 => {
                    activate_overlay_client
                        .borrow_mut()
                        .take()
                        .map_or(DesktopOverlay::Unavailable, |client| {
                            DesktopOverlay::Helper(Box::new(client))
                        })
                }
                OverlayRuntimeBackend::Unavailable => DesktopOverlay::Unavailable,
            };

            let (model_path, model_exists) = {
                let runtime = activate_runtime.borrow();
                let runtime = runtime
                    .as_ref()
                    .expect("primary application runtime was not initialized");
                (runtime.model_path(), runtime.model_exists())
            };
            settings.update_model_status(&model_path, model_exists);
            *activate_desktop.borrow_mut() = Some(DesktopUi { settings, overlay });

            install_runtime_poll(
                application,
                Rc::clone(&activate_controller),
                Rc::clone(&activate_runtime),
                Rc::clone(&activate_desktop),
                desktop_session,
                effective_overlay_backend,
            );
        }

        if let Some(desktop) = activate_desktop.borrow().as_ref() {
            desktop.settings.present();
        }
    });

    let shutdown_runtime = Rc::clone(&runtime);
    let shutdown_desktop = Rc::clone(&desktop);
    application.connect_shutdown(move |_| {
        if let Some(desktop) = shutdown_desktop.borrow().as_ref() {
            desktop.overlay.request_shutdown();
        }
        if let Some(runtime) = shutdown_runtime.borrow_mut().as_mut() {
            runtime.request_shutdown();
        }
    });

    application.run();
    if let Some(desktop) = desktop.borrow().as_ref() {
        desktop.overlay.finish();
    }
    if let Some(runtime) = runtime.borrow_mut().as_mut() {
        runtime.finish();
    }
    Ok(())
}

/// Poll ช่องทาง background ทุก 20 ms แล้วนำเฉพาะ diff ไปใช้กับ GTK widgets
fn install_runtime_poll(
    application: &adw::Application,
    controller: Rc<RefCell<ApplicationController>>,
    runtime: Rc<RefCell<Option<ApplicationRuntime>>>,
    desktop: Rc<RefCell<Option<DesktopUi>>>,
    desktop_session: DesktopSession,
    overlay_backend: OverlayRuntimeBackend,
) {
    let application = application.clone();
    glib::timeout_add_local(Duration::from_millis(20), move || {
        let update = {
            let mut runtime = runtime.borrow_mut();
            let Some(runtime) = runtime.as_mut() else {
                return glib::ControlFlow::Break;
            };
            runtime.poll()
        };

        if update.streams_changed {
            let streams = controller.borrow().streams().to_vec();
            if let Some(desktop) = desktop.borrow().as_ref() {
                desktop.settings.update_streams(&streams);
            }
        }

        let (state, last_error, metrics, pipewire_status) = {
            let runtime = runtime.borrow();
            let runtime = runtime
                .as_ref()
                .expect("primary application runtime was not initialized");
            (
                runtime.state(),
                runtime.last_error().map(str::to_owned),
                runtime.metrics_snapshot(),
                runtime.pipewire_status(),
            )
        };
        let subtitle_config = controller.borrow().config().subtitle.clone();
        if let Some(desktop) = desktop.borrow().as_ref() {
            // งานส่วนนี้มีเฉพาะการอัปเดต widget; model load และ inference อยู่บน worker ทั้งหมด
            desktop.settings.update_state(state, last_error.as_deref());
            desktop.settings.update_metrics(metrics);
            desktop.settings.update_pipewire_status(pipewire_status);
            desktop
                .settings
                .append_debug_records(&update.debug_records);
            if let Some(status) = update.debug_log_status.as_ref() {
                desktop.settings.update_debug_log_status(status);
            }
            let overlay_events = desktop.overlay.poll_events();
            for event in &overlay_events {
                if let OverlayEvent::MonitorsChanged { monitors } = event {
                    desktop.settings.update_overlay_monitors(monitors);
                }
            }
            {
                let mut runtime = runtime.borrow_mut();
                let runtime = runtime
                    .as_mut()
                    .expect("primary application runtime was not initialized");
                for event in &overlay_events {
                    match event {
                        OverlayEvent::Rendered {
                            frame_id,
                            rendered_at_micros,
                        } => {
                            runtime.record_overlay_rendered(*frame_id, *rendered_at_micros);
                        }
                        OverlayEvent::Error { .. } => runtime.clear_pending_overlay_frames(),
                        OverlayEvent::Ready
                        | OverlayEvent::MonitorsChanged { .. }
                        | OverlayEvent::Pong { .. } => {}
                    }
                }
            }
            let overlay_error = desktop.overlay.last_error();
            desktop.settings.update_overlay_status(
                desktop_session,
                overlay_backend,
                desktop.overlay.status(),
                overlay_error.as_deref(),
            );
            desktop.overlay.apply_config(&subtitle_config);
            if update.hide_overlay {
                desktop.overlay.hide();
                if let Some(runtime) = runtime.borrow_mut().as_mut() {
                    runtime.clear_pending_overlay_frames();
                }
            }
            if let Some(subtitle) = update.subtitle
                && let Some(submission) = desktop.overlay.show_text(
                    &subtitle.text,
                    subtitle.is_final,
                    subtitle.audio_origin_at,
                )
                && let Some(runtime) = runtime.borrow_mut().as_mut()
            {
                runtime.track_overlay_frame(submission.frame_id, submission.audio_origin_micros);
            }
            if update.show_settings {
                desktop.settings.present();
            }
        }

        if update.quit_requested {
            application.quit();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}
