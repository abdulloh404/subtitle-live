use super::ApplicationState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppEvent {
    StateChanged(ApplicationState),
    Error(String),
}
