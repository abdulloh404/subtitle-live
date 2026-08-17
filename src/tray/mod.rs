use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use ksni::blocking::TrayMethods;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayState {
    Active,
    Paused,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    StartSubtitles,
    StopSubtitles,
    ShowSettings,
    Quit,
}

pub struct TrayIndicator {
    updates: Sender<TrayUpdate>,
}

enum TrayUpdate {
    State(TrayState),
    Shutdown,
}

struct IndicatorModel {
    commands: Sender<TrayCommand>,
    state: TrayState,
}

impl TrayIndicator {
    pub fn spawn(initial_state: TrayState) -> Result<(Self, Receiver<TrayCommand>), String> {
        let (command_sender, command_receiver) = mpsc::channel();
        let model = IndicatorModel {
            commands: command_sender,
            state: initial_state,
        };
        let handle = model.spawn().map_err(|error| error.to_string())?;
        let (updates, update_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("tray-indicator".to_owned())
            .spawn(move || {
                while let Ok(update) = update_receiver.recv() {
                    match update {
                        TrayUpdate::State(state) => {
                            let _ = handle.update(|model| model.state = state);
                        }
                        TrayUpdate::Shutdown => {
                            handle.shutdown().wait();
                            return;
                        }
                    }
                }
                handle.shutdown().wait();
            })
            .map_err(|error| error.to_string())?;

        Ok((Self { updates }, command_receiver))
    }

    pub fn set_state(&self, state: TrayState) {
        let _ = self.updates.send(TrayUpdate::State(state));
    }

    pub fn shutdown(&self) {
        let _ = self.updates.send(TrayUpdate::Shutdown);
    }
}

impl Drop for TrayIndicator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl ksni::Tray for IndicatorModel {
    fn id(&self) -> String {
        "subtitle-live".to_owned()
    }

    fn title(&self) -> String {
        format!("Subtitle-live — {}", self.state_label())
    }

    fn status(&self) -> ksni::Status {
        if self.state == TrayState::Error {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_name(&self) -> String {
        match self.state {
            TrayState::Active => "audio-input-microphone".to_owned(),
            TrayState::Paused => "microphone-sensitivity-muted".to_owned(),
            TrayState::Error => "dialog-error".to_owned(),
        }
    }

    fn attention_icon_name(&self) -> String {
        "dialog-error".to_owned()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.commands.send(TrayCommand::ShowSettings);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;

        let status = StandardItem {
            label: format!("Status: {}", self.state_label()),
            enabled: false,
            ..Default::default()
        };
        let toggle = if self.state == TrayState::Active {
            let commands = self.commands.clone();
            StandardItem {
                label: "Stop Subtitles".to_owned(),
                activate: Box::new(move |_| {
                    let _ = commands.send(TrayCommand::StopSubtitles);
                }),
                ..Default::default()
            }
        } else {
            let commands = self.commands.clone();
            StandardItem {
                label: "Start Subtitles".to_owned(),
                activate: Box::new(move |_| {
                    let _ = commands.send(TrayCommand::StartSubtitles);
                }),
                ..Default::default()
            }
        };
        let show_settings_commands = self.commands.clone();
        let show_settings = StandardItem {
            label: "Show Settings".to_owned(),
            activate: Box::new(move |_| {
                let _ = show_settings_commands.send(TrayCommand::ShowSettings);
            }),
            ..Default::default()
        };
        let quit_commands = self.commands.clone();
        let quit = StandardItem {
            label: "Quit".to_owned(),
            icon_name: "application-exit".to_owned(),
            activate: Box::new(move |_| {
                let _ = quit_commands.send(TrayCommand::Quit);
            }),
            ..Default::default()
        };

        vec![
            status.into(),
            ksni::MenuItem::Separator,
            toggle.into(),
            show_settings.into(),
            ksni::MenuItem::Separator,
            quit.into(),
        ]
    }
}

impl IndicatorModel {
    const fn state_label(&self) -> &'static str {
        match self.state {
            TrayState::Active => "Active",
            TrayState::Paused => "Paused",
            TrayState::Error => "Error",
        }
    }
}
