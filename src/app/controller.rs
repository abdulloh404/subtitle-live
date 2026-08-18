//! ตัวควบคุมสถานะแอปพลิเคชันและกฎการเลือกแหล่งเสียง

use crate::{
    config::{
        AppConfig, ApplicationRule, SUBTITLE_POSITIONS, SUBTITLE_TEXT_ALIGNMENTS, StreamRule,
    },
    pipewire::{
        ApplicationIdentity, ApplicationKey, CaptureTarget, StreamDiscriminator, StreamInfo,
    },
};

use super::{AppCommand, AppEvent, ApplicationState};

/// แหล่งข้อมูลจริงของ config สถานะ pipeline และกฎเลือก stream ในชั้นแอปพลิเคชัน
pub struct ApplicationController {
    /// การตั้งค่าที่ผ่านการปรับค่าให้อยู่ในช่วงที่ UI รองรับ
    config: AppConfig,
    /// สถานะวงจรชีวิตล่าสุดของ pipeline
    state: ApplicationState,
    /// snapshot ล่าสุดของ playback stream ที่ PipeWire ค้นพบ
    streams: Vec<StreamInfo>,
}

impl ApplicationController {
    /// สร้าง controller และกำหนดสถานะเริ่มต้นจากการตั้งค่าที่โหลดมา
    pub fn new(mut config: AppConfig) -> Self {
        normalize_subtitle_config(&mut config);
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

    /// คืนสถานะ pipeline ปัจจุบัน
    pub const fn state(&self) -> ApplicationState {
        self.state
    }

    /// คืนการตั้งค่าปัจจุบันแบบยืมค่า
    pub const fn config(&self) -> &AppConfig {
        &self.config
    }

    /// สร้าง snapshot สำหรับส่งให้ worker บันทึกการตั้งค่า
    pub fn config_snapshot(&self) -> AppConfig {
        self.config.clone()
    }

    /// อัปเดตสถานะที่ runtime รายงานกลับมา
    pub fn set_state(&mut self, state: ApplicationState) {
        self.state = state;
    }

    /// แทนที่รายการ stream ด้วย snapshot จาก PipeWire รอบล่าสุด
    pub fn replace_streams(&mut self, streams: Vec<StreamInfo>) {
        self.streams = streams;
    }

    /// ชื่อเรียกอีกแบบของ [`Self::replace_streams`] สำหรับชั้น UI
    pub fn set_streams(&mut self, streams: Vec<StreamInfo>) {
        self.replace_streams(streams);
    }

    /// คืนรายการ playback stream ล่าสุด
    pub fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    /// ตรวจว่า application ถูกเลือกทั้งแอปหรือครบทุก stream ที่ค้นพบหรือไม่
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

    /// ตรวจว่า stream ตรงกับกฎ application หรือกฎ stream ที่เปิดใช้งานหรือไม่
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

    /// เลือกหรือยกเลิกทั้ง application โดยบันทึกเฉพาะ stable identity
    pub fn set_application_selected(&mut self, application: &ApplicationIdentity, selected: bool) {
        self.remove_matching_rules(application);
        if selected && application.stable_key().is_some() {
            self.config
                .audio
                .rules
                .push(application_rule(application, true));
        }
    }

    /// เลือกหรือยกเลิก playback stream หนึ่งรายการภายใน application
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

    /// แปลงกฎถาวรเป็นเป้าหมายจับเสียงที่ไม่ผูกกับ node ID ชั่วคราว
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

    /// ชื่อเรียกอีกแบบของ [`Self::selected_capture_targets`] สำหรับ runtime
    pub fn selected_targets(&self) -> Vec<CaptureTarget> {
        self.selected_capture_targets()
    }

    /// ค้นหากฎแรกที่ตรงกับ stable identity ของ application
    fn matching_rule(&self, application: &ApplicationIdentity) -> Option<&ApplicationRule> {
        self.config
            .audio
            .rules
            .iter()
            .find(|rule| application_rule_matches(rule, application))
    }

    /// ลบกฎเดิมทั้งหมดก่อนแทนที่ด้วยการเลือกชุดใหม่
    fn remove_matching_rules(&mut self, application: &ApplicationIdentity) {
        self.config
            .audio
            .rules
            .retain(|rule| !application_rule_matches(rule, application));
    }

    /// ตรวจสอบและประมวลผลคำสั่งหนึ่งรายการโดยไม่เรียกบริการภายนอกโดยตรง
    pub fn handle_command(&mut self, command: AppCommand) -> AppEvent {
        let event = match command {
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
            AppCommand::RetryPipeline => {
                self.config.general.live_subtitles = true;
                self.state = ApplicationState::Starting;
                AppEvent::StateChanged(self.state)
            }
            AppCommand::SetKeepRunningWhenClosed(enabled) => {
                self.config.general.keep_running_when_closed = enabled;
                AppEvent::ConfigChanged("general.keep_running_when_closed")
            }
            AppCommand::SetSttStepMs(step_ms) => {
                if !(50..=1_000).contains(&step_ms) {
                    return AppEvent::Error(format!(
                        "STT audio step must be between 50 and 1000 ms: {step_ms}"
                    ));
                }
                self.config.stt.step_ms = step_ms;
                AppEvent::ConfigChanged("stt.step_ms")
            }
            AppCommand::SetSttWindowMs(window_ms) => {
                if !(1_000..=30_000).contains(&window_ms) {
                    return AppEvent::Error(format!(
                        "STT context window must be between 1000 and 30000 ms: {window_ms}"
                    ));
                }
                self.config.stt.window_ms = window_ms;
                AppEvent::ConfigChanged("stt.window_ms")
            }
            AppCommand::SetVadEnabled(enabled) => {
                self.config.stt.vad_enabled = enabled;
                AppEvent::ConfigChanged("stt.vad_enabled")
            }
            AppCommand::SetSubtitleVisible(visible) => {
                self.config.subtitle.visible = visible;
                AppEvent::ConfigChanged("subtitle.visible")
            }
            AppCommand::SetSubtitlePosition(position) => {
                if !SUBTITLE_POSITIONS.contains(&position.as_str()) {
                    return AppEvent::Error(format!(
                        "Unsupported subtitle position: {position}"
                    ));
                }
                self.config.subtitle.position = position;
                AppEvent::ConfigChanged("subtitle.position")
            }
            AppCommand::SetSubtitleTextAlignment(alignment) => {
                if !SUBTITLE_TEXT_ALIGNMENTS.contains(&alignment.as_str()) {
                    return AppEvent::Error(format!(
                        "Unsupported subtitle text alignment: {alignment}"
                    ));
                }
                self.config.subtitle.text_alignment = alignment;
                AppEvent::ConfigChanged("subtitle.text_alignment")
            }
            AppCommand::SetSubtitleFontSize(font_size) => {
                if !(16..=72).contains(&font_size) {
                    return AppEvent::Error(format!(
                        "Subtitle font size must be between 16 and 72: {font_size}"
                    ));
                }
                self.config.subtitle.font_size = font_size;
                AppEvent::ConfigChanged("subtitle.font_size")
            }
            AppCommand::SetSubtitleWidthPx(width_px) => {
                if !(320..=3_840).contains(&width_px) {
                    return AppEvent::Error(format!(
                        "Subtitle width must be between 320 and 3840 pixels: {width_px}"
                    ));
                }
                self.config.subtitle.width_px = width_px;
                AppEvent::ConfigChanged("subtitle.width_px")
            }
            AppCommand::SetSubtitleBackgroundOpacityPercent(opacity) => {
                if opacity > 100 {
                    return AppEvent::Error(format!(
                        "Subtitle background opacity must be between 0 and 100: {opacity}"
                    ));
                }
                self.config.subtitle.background_opacity = opacity as f32 / 100.0;
                AppEvent::ConfigChanged("subtitle.background_opacity")
            }
            AppCommand::SetSubtitleMaxLines(max_lines) => {
                if !(1..=5).contains(&max_lines) {
                    return AppEvent::Error(format!(
                        "Subtitle maximum lines must be between 1 and 5: {max_lines}"
                    ));
                }
                self.config.subtitle.max_lines = max_lines;
                AppEvent::ConfigChanged("subtitle.max_lines")
            }
            AppCommand::ShowSettings => AppEvent::SettingsRequested,
            AppCommand::Quit => AppEvent::QuitRequested,
        };
        event
    }
}

/// ปรับค่าคำบรรยายที่โหลดจากไฟล์ให้อยู่ในช่วงที่ UI รองรับ
fn normalize_subtitle_config(config: &mut AppConfig) {
    if !SUBTITLE_POSITIONS.contains(&config.subtitle.position.as_str()) {
        config.subtitle.position = "bottom-center".to_owned();
    }
    if !SUBTITLE_TEXT_ALIGNMENTS.contains(&config.subtitle.text_alignment.as_str()) {
        config.subtitle.text_alignment = "left".to_owned();
    }
    config.subtitle.font_size = config.subtitle.font_size.clamp(16, 72);
}

/// สร้างกฎ application จาก stable metadata ที่ PipeWire รายงาน
fn application_rule(application: &ApplicationIdentity, enabled: bool) -> ApplicationRule {
    ApplicationRule {
        enabled,
        application_id: application.application_id.clone(),
        process_binary: application.process_binary.clone(),
        application_name: application.application_name.clone(),
        streams: Vec::new(),
    }
}

/// สร้างกฎ stream โดยไม่บันทึก runtime node ID ซึ่งเปลี่ยนได้ทุกครั้ง
fn stream_rule(stream: &StreamInfo) -> StreamRule {
    StreamRule {
        enabled: true,
        media_name: stream.media_name.clone(),
        node_name: stream.node_name.clone(),
    }
}

/// แปลงกฎที่บันทึกไว้กลับเป็น identity สำหรับใช้จับคู่กับ graph ปัจจุบัน
fn rule_identity(rule: &ApplicationRule) -> ApplicationIdentity {
    ApplicationIdentity {
        application_id: rule.application_id.clone(),
        process_binary: rule.process_binary.clone(),
        application_name: rule.application_name.clone(),
    }
}

/// จับคู่กฎด้วย metadata ที่เสถียรที่สุดตามลำดับความสำคัญ
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

/// จับคู่ stream ด้วย node name ก่อน แล้วจึงใช้ media name เป็น fallback
fn stream_rule_matches(rule: &StreamRule, stream: &StreamInfo) -> bool {
    if let Some(expected) = non_empty(&rule.node_name) {
        return non_empty(&stream.node_name) == Some(expected);
    }
    non_empty(&rule.media_name)
        .is_some_and(|expected| non_empty(&stream.media_name) == Some(expected))
}

/// เปรียบเทียบ application ผ่าน stable key เท่านั้น
fn same_application(left: &ApplicationIdentity, right: &ApplicationIdentity) -> bool {
    match (left.stable_key(), right.stable_key()) {
        (Some(left), Some(right)) => application_keys_equal(&left, &right),
        _ => false,
    }
}

/// แยกฟังก์ชันเปรียบเทียบ key เพื่อให้จุดเรียกอ่านความหมายได้ชัดเจน
fn application_keys_equal(left: &ApplicationKey, right: &ApplicationKey) -> bool {
    left == right
}

/// คืนข้อความที่ไม่ว่างและตัดค่าที่มีแต่ช่องว่างออก
fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{AppCommand, AppEvent, ApplicationController, ApplicationState},
        config::{AppConfig, ApplicationRule},
    };

    #[test]
    fn retry_pipeline_keeps_audio_source_selection() {
        let mut config = AppConfig::default();
        config.audio.rules.push(ApplicationRule {
            enabled: true,
            application_id: Some("org.example.Player".to_owned()),
            process_binary: None,
            application_name: Some("Player".to_owned()),
            streams: Vec::new(),
        });
        let expected_rules = config.audio.rules.clone();
        let mut controller = ApplicationController::new(config);
        controller.set_state(ApplicationState::Error);

        let event = controller.handle_command(AppCommand::RetryPipeline);

        assert_eq!(
            event,
            AppEvent::StateChanged(ApplicationState::Starting)
        );
        assert!(controller.config().general.live_subtitles);
        assert_eq!(controller.config().audio.rules, expected_rules);
    }
}
