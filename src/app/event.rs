use super::ApplicationState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppEvent {
    StateChanged(ApplicationState),
    ConfigChanged(&'static str),
    SettingsRequested,
    QuitRequested,
    Error(String),
}
