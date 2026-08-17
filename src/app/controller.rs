use crate::config::AppConfig;

use super::{AppCommand, AppEvent, ApplicationState};

pub struct ApplicationController {
    config: AppConfig,
    state: ApplicationState,
}

impl ApplicationController {
    pub fn new(config: AppConfig) -> Self {
        let state = if config.general.live_subtitles {
            ApplicationState::Starting
        } else {
            ApplicationState::Stopped
        };

        Self { config, state }
    }

    pub const fn state(&self) -> ApplicationState {
        self.state
    }

    pub const fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn handle_command(&mut self, command: AppCommand) -> AppEvent {
        match command {
            AppCommand::StartSubtitles => {
                self.config.general.live_subtitles = true;
                self.state = ApplicationState::Starting;
                AppEvent::StateChanged(self.state)
            }
            AppCommand::StopSubtitles => {
                self.config.general.live_subtitles = false;
                self.state = ApplicationState::Stopped;
                AppEvent::StateChanged(self.state)
            }
            AppCommand::SetKeepRunningWhenClosed(enabled) => {
                self.config.general.keep_running_when_closed = enabled;
                AppEvent::ConfigChanged("general.keep_running_when_closed")
            }
            AppCommand::ShowSettings => AppEvent::SettingsRequested,
            AppCommand::Quit => AppEvent::QuitRequested,
        }
    }
}
