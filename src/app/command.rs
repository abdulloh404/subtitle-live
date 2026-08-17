#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppCommand {
    StartSubtitles,
    StopSubtitles,
    ShowSettings,
    Quit,
}
