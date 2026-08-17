//! จุดเริ่มต้นของแอป GTK และสะพานส่ง event จาก runtime ไปยังหน้าต่าง Settings/Overlay

use std::{
    cell::RefCell,
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk::glib;
use subtitle_live::{
    app::ApplicationController,
    config,
    error::AppError,
    logging,
    overlay::OverlayPresenter,
    runtime::ApplicationRuntime,
    ui::SettingsPresenter,
};

/// รวม presenter ที่ต้องมีอายุเท่ากับ desktop application
struct DesktopUi {
    /// หน้าต่างตั้งค่าและสถานะ pipeline
    settings: SettingsPresenter,
    /// หน้าต่างโปร่งใสที่แสดง subtitle เหนือหน้าจอ
    overlay: OverlayPresenter,
}

/// โหลด config, สร้าง GTK application และปิด worker อย่างเป็นระเบียบเมื่อ event loop จบ
fn main() -> Result<(), AppError> {
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

    tracing::info!(
        state = ?controller.borrow().state(),
        config_path = %config_path.display(),
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
    application.connect_activate(move |application| {
        // Presenter เป็น GTK object จึงต้องสร้างและใช้งานบน main thread เท่านั้น
        if activate_desktop.borrow().is_none() {
            let settings = SettingsPresenter::new(application, Rc::clone(&activate_controller));
            let subtitle_config = activate_controller.borrow().config().subtitle.clone();
            let overlay = OverlayPresenter::new(application, &subtitle_config);

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
    application.connect_shutdown(move |_| {
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

/// Poll ช่องทาง background ทุก 50 ms แล้วนำเฉพาะ diff ไปใช้กับ GTK widgets
fn install_runtime_poll(
    application: &adw::Application,
    controller: Rc<RefCell<ApplicationController>>,
    runtime: Rc<RefCell<Option<ApplicationRuntime>>>,
    desktop: Rc<RefCell<Option<DesktopUi>>>,
) {
    let application = application.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
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
            desktop.overlay.apply_config(&subtitle_config);
            if update.hide_overlay {
                desktop.overlay.hide();
            }
            if let Some(subtitle) = update.subtitle {
                desktop
                    .overlay
                    .show_text(&subtitle.text, subtitle.is_final);
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
