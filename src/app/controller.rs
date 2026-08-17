use crate::{
    config::{AppConfig, ApplicationRule, StreamRule},
    pipewire::{
        ApplicationIdentity, ApplicationKey, CaptureTarget, StreamDiscriminator, StreamInfo,
    },
};

use super::{AppCommand, AppEvent, ApplicationState};

pub struct ApplicationController {
    config: AppConfig,
    state: ApplicationState,
    streams: Vec<StreamInfo>,
}

impl ApplicationController {
    pub fn new(config: AppConfig) -> Self {
        let state = if config.general.live_subtitles {
            ApplicationState::Starting
        } else {
            ApplicationState::Stopped
        };

        Self {
            config,
            state,
            streams: Vec::new(),
        }
    }

    pub const fn state(&self) -> ApplicationState {
        self.state
    }

    pub const fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn config_snapshot(&self) -> AppConfig {
        self.config.clone()
    }

    pub fn set_state(&mut self, state: ApplicationState) {
        self.state = state;
    }

    pub fn replace_streams(&mut self, streams: Vec<StreamInfo>) {
        self.streams = streams;
    }

    pub fn set_streams(&mut self, streams: Vec<StreamInfo>) {
        self.replace_streams(streams);
    }

    pub fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    pub fn application_selected(&self, application: &ApplicationIdentity) -> bool {
        let application_streams: Vec<_> = self
            .streams
            .iter()
            .filter(|stream| same_application(&stream.application, application))
            .collect();
        if application_streams.is_empty() {
            return self
                .matching_rule(application)
                .is_some_and(|rule| rule.enabled);
        }
        application_streams
            .iter()
            .all(|stream| self.stream_selected(stream))
    }

    pub fn stream_selected(&self, stream: &StreamInfo) -> bool {
        let Some(rule) = self.matching_rule(&stream.application) else {
            return false;
        };
        rule.enabled
            || rule
                .streams
                .iter()
                .any(|candidate| candidate.enabled && stream_rule_matches(candidate, stream))
    }

    pub fn set_application_selected(&mut self, application: &ApplicationIdentity, selected: bool) {
        self.remove_matching_rules(application);
        if selected && application.stable_key().is_some() {
            self.config
                .audio
                .rules
                .push(application_rule(application, true));
        }
    }

    pub fn set_stream_selected(&mut self, stream: &StreamInfo, selected: bool) {
        if stream.application.stable_key().is_none()
            || (non_empty(&stream.node_name).is_none() && non_empty(&stream.media_name).is_none())
        {
            return;
        }

        let was_application_selected = self
            .matching_rule(&stream.application)
            .is_some_and(|rule| rule.enabled);
        if was_application_selected && selected {
            return;
        }

        if was_application_selected {
            let selected_streams: Vec<_> = self
                .streams
                .iter()
                .filter(|candidate| {
                    same_application(&candidate.application, &stream.application)
                        && candidate.runtime_id != stream.runtime_id
                        && (non_empty(&candidate.node_name).is_some()
                            || non_empty(&candidate.media_name).is_some())
                })
                .cloned()
                .collect();
            self.remove_matching_rules(&stream.application);
            if !selected_streams.is_empty() {
                let mut rule = application_rule(&stream.application, false);
                rule.streams = selected_streams.iter().map(stream_rule).collect();
                self.config.audio.rules.push(rule);
            }
            return;
        }

        let rule_index = self
            .config
            .audio
            .rules
            .iter()
            .position(|rule| application_rule_matches(rule, &stream.application));
        if selected {
            let index = rule_index.unwrap_or_else(|| {
                self.config
                    .audio
                    .rules
                    .push(application_rule(&stream.application, false));
                self.config.audio.rules.len() - 1
            });
            let rule = &mut self.config.audio.rules[index];
            if !rule
                .streams
                .iter()
                .any(|candidate| stream_rule_matches(candidate, stream))
            {
                rule.streams.push(stream_rule(stream));
            }
        } else if let Some(index) = rule_index {
            self.config.audio.rules[index]
                .streams
                .retain(|candidate| !stream_rule_matches(candidate, stream));
            if self.config.audio.rules[index].streams.is_empty() {
                self.config.audio.rules.remove(index);
            }
        }
    }

    pub fn selected_capture_targets(&self) -> Vec<CaptureTarget> {
        let mut targets = Vec::new();
        for rule in &self.config.audio.rules {
            let application = rule_identity(rule);
            if rule.enabled {
                targets.push(CaptureTarget::application(application));
                continue;
            }
            for stream in rule.streams.iter().filter(|stream| stream.enabled) {
                let Some(stream_match) = non_empty(&stream.node_name)
                    .map(|value| StreamDiscriminator::NodeName(value.to_owned()))
                    .or_else(|| {
                        non_empty(&stream.media_name)
                            .map(|value| StreamDiscriminator::MediaName(value.to_owned()))
                    })
                else {
                    continue;
                };
                let target = CaptureTarget {
                    application: application.clone(),
                    stream_match: Some(stream_match),
                    runtime_id: None,
                };
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
        targets
    }

    pub fn selected_targets(&self) -> Vec<CaptureTarget> {
        self.selected_capture_targets()
    }

    fn matching_rule(&self, application: &ApplicationIdentity) -> Option<&ApplicationRule> {
        self.config
            .audio
            .rules
            .iter()
            .find(|rule| application_rule_matches(rule, application))
    }

    fn remove_matching_rules(&mut self, application: &ApplicationIdentity) {
        self.config
            .audio
            .rules
            .retain(|rule| !application_rule_matches(rule, application));
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

fn application_rule(application: &ApplicationIdentity, enabled: bool) -> ApplicationRule {
    ApplicationRule {
        enabled,
        application_id: application.application_id.clone(),
        process_binary: application.process_binary.clone(),
        application_name: application.application_name.clone(),
        streams: Vec::new(),
    }
}

fn stream_rule(stream: &StreamInfo) -> StreamRule {
    StreamRule {
        enabled: true,
        media_name: stream.media_name.clone(),
        node_name: stream.node_name.clone(),
    }
}

fn rule_identity(rule: &ApplicationRule) -> ApplicationIdentity {
    ApplicationIdentity {
        application_id: rule.application_id.clone(),
        process_binary: rule.process_binary.clone(),
        application_name: rule.application_name.clone(),
    }
}

fn application_rule_matches(rule: &ApplicationRule, application: &ApplicationIdentity) -> bool {
    if let (Some(expected), Some(actual)) = (
        non_empty(&rule.application_id),
        non_empty(&application.application_id),
    ) {
        return expected == actual;
    }
    if let (Some(expected), Some(actual)) = (
        non_empty(&rule.process_binary),
        non_empty(&application.process_binary),
    ) {
        return expected == actual;
    }
    if let (Some(expected), Some(actual)) = (
        non_empty(&rule.application_name),
        non_empty(&application.application_name),
    ) {
        return expected == actual;
    }
    false
}

fn stream_rule_matches(rule: &StreamRule, stream: &StreamInfo) -> bool {
    if let Some(expected) = non_empty(&rule.node_name) {
        return non_empty(&stream.node_name) == Some(expected);
    }
    non_empty(&rule.media_name)
        .is_some_and(|expected| non_empty(&stream.media_name) == Some(expected))
}

fn same_application(left: &ApplicationIdentity, right: &ApplicationIdentity) -> bool {
    match (left.stable_key(), right.stable_key()) {
        (Some(left), Some(right)) => application_keys_equal(&left, &right),
        _ => false,
    }
}

fn application_keys_equal(left: &ApplicationKey, right: &ApplicationKey) -> bool {
    left == right
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}
