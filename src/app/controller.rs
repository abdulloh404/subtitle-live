use crate::config::AppConfig;

use super::ApplicationState;

pub struct ApplicationController {
    config: AppConfig,
    state: ApplicationState,
}

impl ApplicationController {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            state: ApplicationState::Stopped,
        }
    }

    pub const fn state(&self) -> ApplicationState {
        self.state
    }

    pub const fn config(&self) -> &AppConfig {
        &self.config
    }
}
