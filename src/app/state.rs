#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ApplicationState {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}
