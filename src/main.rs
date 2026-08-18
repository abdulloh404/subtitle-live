//! จุดเริ่มต้นของแอป GTK และสะพานส่ง event จาก runtime ไปยังหน้าต่าง Settings/Overlay

use std::{cell::RefCell, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::glib;
use subtitle_live::{
    app::ApplicationController,
    config,
    error::AppError,
    logging,
    overlay::{
        DesktopSession, OverlayClient, OverlayEvent, OverlayPresenter, OverlayRuntimeBackend,
        run_overlay_helper,
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
    /// Wayland main process ส่งข้อความให้ XWayland helper
    Helper(OverlayClient),
    /// X11 main process ยังใช้ presenter เดิมจนกว่าจะย้ายไป helper ในขั้นถัดไป
    Local(OverlayPresenter),
    /// ไม่มี X11 display; pipeline อื่นยังทำงานต่อได้
    Unavailable,
}

impl DesktopOverlay {
    /// นำค่ารูปลักษณ์ไปใช้กับ renderer ที่พร้อมใช้งาน
    fn apply_config(&self, config: &config::SubtitleConfig) {
        match self {
            Self::Helper(client) => client.apply_config(config),
            Self::Local(presenter) => presenter.apply_config(config),
            Self::Unavailable => {}
        }
    }

    /// แสดง subtitle โดย helper รับเฉพาะข้อความที่ runtime จัดการแล้ว
    fn show_text(&self, text: &str, is_final: bool) {
        match self {
            Self::Helper(client) => {
                client.show_text(text, is_final);
            }
            Self::Local(presenter) => presenter.show_text(text, is_final),
            Self::Unavailable => {}
        }
    }

    /// ล้างข้อความปัจจุบันโดยไม่หยุด audio/STT pipeline
    fn hide(&self) {
        match self {
            Self::Helper(client) => client.hide(),
            Self::Local(presenter) => presenter.hide(),
            Self::Unavailable => {}
        }
    }

    /// รับสถานะ helper แบบไม่ block และบันทึกเฉพาะ lifecycle/error
    fn poll_events(&self) {
        let Self::Helper(client) = self else {
            return;
        };
        for event in client.drain_events() {
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
                OverlayEvent::Pong { .. } | OverlayEvent::Rendered { .. } => {}
            }
        }
    }

    /// ขอให้ renderer หยุดโดยไม่ block GTK shutdown callback
    fn request_shutdown(&self) {
        match self {
            Self::Helper(client) => client.shutdown(),
            Self::Local(presenter) => presenter.hide(),
            Self::Unavailable => {}
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
        OverlayRuntimeBackend::XWayland => {
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
        OverlayRuntimeBackend::X11 | OverlayRuntimeBackend::Unavailable => None,
    }));

    tracing::info!(
        state = ?controller.borrow().state(),
        config_path = %config_path.display(),
        desktop_session = desktop_session.as_str(),
        settings_backend = desktop_session.as_str(),
        overlay_backend = overlay_backend.as_str(),
        xwayland_available = overlay_backend == OverlayRuntimeBackend::XWayland,
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
            let settings = SettingsPresenter::new(application, Rc::clone(&activate_controller));
            let subtitle_config = activate_controller.borrow().config().subtitle.clone();
            let overlay = match overlay_backend {
                OverlayRuntimeBackend::XWayland => activate_overlay_client
                    .borrow_mut()
                    .take()
                    .map_or(DesktopOverlay::Unavailable, DesktopOverlay::Helper),
                OverlayRuntimeBackend::X11 => {
                    DesktopOverlay::Local(OverlayPresenter::new(application, &subtitle_config))
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

        let (state, last_error, metrics) = {
            let runtime = runtime.borrow();
            let runtime = runtime
                .as_ref()
                .expect("primary application runtime was not initialized");
            (
                runtime.state(),
                runtime.last_error().map(str::to_owned),
                runtime.metrics_snapshot(),
            )
        };
        let subtitle_config = controller.borrow().config().subtitle.clone();
        if let Some(desktop) = desktop.borrow().as_ref() {
            // งานส่วนนี้มีเฉพาะการอัปเดต widget; model load และ inference อยู่บน worker ทั้งหมด
            desktop.settings.update_state(state, last_error.as_deref());
            desktop.settings.update_metrics(metrics);
            desktop.overlay.poll_events();
            desktop.overlay.apply_config(&subtitle_config);
            if update.hide_overlay {
                desktop.overlay.hide();
            }
            if let Some(subtitle) = update.subtitle {
                desktop.overlay.show_text(&subtitle.text, subtitle.is_final);
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
