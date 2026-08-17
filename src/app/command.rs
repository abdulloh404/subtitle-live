#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommand {
    StartSubtitles,
    StopSubtitles,
    SetKeepRunningWhenClosed(bool),
    SetSubtitleVisible(bool),
    SetSubtitlePosition(String),
    SetSubtitleFontSize(u32),
    SetSubtitleBackgroundOpacityPercent(u32),
    SetSubtitleMaxLines(u32),
    ShowSettings,
    Quit,
}
