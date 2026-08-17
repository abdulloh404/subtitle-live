use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use subtitle_live::{app::ApplicationController, config, error::AppError, logging, ui};

fn main() -> Result<(), AppError> {
    logging::init()?;

    let config_path = config::default_path()?;
    let config = config::load_or_default(&config_path)?;
    let controller = Rc::new(RefCell::new(ApplicationController::new(config)));

    tracing::info!(
        state = ?controller.borrow().state(),
        config_path = %config_path.display(),
        "Subtitle-live initialized"
    );

    let application = adw::Application::builder()
        .application_id("io.github.subtitle_live")
        .build();

    application.connect_activate(move |application| {
        ui::present_settings(application, Rc::clone(&controller));
    });

    application.run();

    Ok(())
}
