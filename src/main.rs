use subtitle_live::{app::ApplicationController, config, error::AppError, logging};

fn main() -> Result<(), AppError> {
    logging::init()?;

    let config_path = config::default_path()?;
    let config = config::load_or_default(&config_path)?;
    let controller = ApplicationController::new(config);

    tracing::info!(
        state = ?controller.state(),
        config_path = %config_path.display(),
        "Subtitle-live initialized"
    );

    Ok(())
}
