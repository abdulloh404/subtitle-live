use tracing_subscriber::EnvFilter;

use crate::error::AppError;

pub fn init() -> Result<(), AppError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .try_init()
        .map_err(|error| AppError::Logging(error.to_string()))
}
