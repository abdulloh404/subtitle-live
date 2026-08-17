//! status indicator ที่ทำงานบน thread แยกและส่งคำสั่งกลับ runtime ผ่าน channel

use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use ksni::blocking::TrayMethods;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// สถานะย่อที่ใช้เลือก label และ icon ของ tray
pub enum TrayState {
    /// pipeline กำลังจับและถอดเสียง
    Active,
    /// pipeline หยุดอยู่
    Paused,
    /// pipeline พบข้อผิดพลาดที่ต้องให้ผู้ใช้ตรวจสอบ
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// คำสั่งจาก tray ที่ runtime รับและแปลงเป็น [`crate::app::AppCommand`]
pub enum TrayCommand {
    /// เริ่ม pipeline คำบรรยาย
    StartSubtitles,
    /// หยุด pipeline คำบรรยาย
    StopSubtitles,
    /// แสดงหน้าต่างตั้งค่า
    ShowSettings,
    /// ออกจากแอปพลิเคชัน
    Quit,
}

/// handle ฝั่ง runtime สำหรับส่งสถานะไปยัง tray thread
pub struct TrayIndicator {
    updates: Sender<TrayUpdate>,
}

/// ข้อความภายในจาก runtime ไปยัง tray thread
enum TrayUpdate {
    /// เปลี่ยน icon, status และเมนูตามสถานะใหม่
    State(TrayState),
    /// ปิด status notifier และหยุด thread
    Shutdown,
}

/// model ที่ ksni เป็นเจ้าของและเรียกบน thread ของ tray
struct IndicatorModel {
    /// ช่องทางส่ง user action กลับไปยัง runtime
    commands: Sender<TrayCommand>,
    /// สถานะล่าสุดที่ใช้สร้าง icon และเมนู
    state: TrayState,
}

impl TrayIndicator {
    /// เริ่ม ksni model และ worker รับการอัปเดตสถานะนอก GTK main thread
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

    /// ส่งสถานะล่าสุดไปยัง tray thread แบบไม่บล็อก
    pub fn set_state(&self, state: TrayState) {
        let _ = self.updates.send(TrayUpdate::State(state));
    }

    /// ขอให้ tray thread ปิด status notifier
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
    /// คืน ID คงที่สำหรับลงทะเบียน status notifier
    fn id(&self) -> String {
        "subtitle-live".to_owned()
    }

    /// แสดงชื่อแอปพร้อมสถานะล่าสุด
    fn title(&self) -> String {
        format!("Subtitle-live — {}", self.state_label())
    }

    /// ขอ attention เฉพาะเมื่อ pipeline อยู่ในสถานะ error
    fn status(&self) -> ksni::Status {
        if self.state == TrayState::Error {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    /// เลือก icon ตามสถานะ pipeline
    fn icon_name(&self) -> String {
        match self.state {
            TrayState::Active => "audio-input-microphone".to_owned(),
            TrayState::Paused => "microphone-sensitivity-muted".to_owned(),
            TrayState::Error => "dialog-error".to_owned(),
        }
    }

    /// คืน icon ที่ desktop ใช้เมื่อ status ต้องการ attention
    fn attention_icon_name(&self) -> String {
        "dialog-error".to_owned()
    }

    /// การคลิก icon ขอให้ runtime แสดงหน้าต่างตั้งค่า
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.commands.send(TrayCommand::ShowSettings);
    }

    /// สร้างเมนูใหม่จากสถานะล่าสุดทุกครั้งที่ ksni ร้องขอ
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
    /// แปลงสถานะเป็นข้อความสั้นสำหรับ title และเมนู
    const fn state_label(&self) -> &'static str {
        match self.state {
            TrayState::Active => "Active",
            TrayState::Paused => "Paused",
            TrayState::Error => "Error",
        }
    }
}
