#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppCommand {
    StartSubtitles,
    StopSubtitles,
    SetKeepRunningWhenClosed(bool),
    ShowSettings,
    Quit,
}
