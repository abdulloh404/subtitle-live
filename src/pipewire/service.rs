//! service thread สำหรับค้นหา PipeWire playback stream และควบคุม capture
//!
//! GTK/application controller ส่งคำสั่งผ่าน channel ส่วน PipeWire main loop
//! ทำงานใน thread ของตัวเอง แล้วส่ง event ที่ normalize แล้วกลับไปยัง runtime

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use pipewire as pw;
use pw::spa::prelude::*;

use crate::audio::{LatestQueue, SourceAudioChunk};

use super::{
    ApplicationIdentity, CaptureTarget, PipeWireEvent, StreamInfo,
    capture::{CaptureSession, create_capture},
};

/// คำสั่งข้าม thread ที่เปลี่ยนสถานะ capture ภายใน PipeWire main loop
#[derive(Clone)]
enum Command {
    SetSelected {
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    },
    StopCapture,
    Shutdown,
}

/// ฝั่งส่งคำสั่งที่ clone ได้โดยไม่เปิดเผย PipeWire main loop ให้ UI เข้าถึง
#[derive(Clone)]
pub struct PipeWireCommandSender {
    sender: pw::channel::Sender<Command>,
}

impl PipeWireCommandSender {
    /// แทนที่กฎการเลือกทั้งหมดและเปิด capture สำหรับ stream ที่ตรงกัน
    pub fn set_selected(
        &self,
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    ) -> Result<(), PipeWireServiceError> {
        self.sender
            .send(Command::SetSelected {
                targets,
                audio_generation,
            })
            .map_err(|_| PipeWireServiceError)
    }

    /// ปิด capture ปัจจุบันโดยยังคง service และการค้นหา stream ไว้
    pub fn stop_capture(&self) -> Result<(), PipeWireServiceError> {
        self.sender
            .send(Command::StopCapture)
            .map_err(|_| PipeWireServiceError)
    }

    /// ปิด capture และออกจาก PipeWire main loop
    pub fn shutdown(&self) -> Result<(), PipeWireServiceError> {
        self.sender
            .send(Command::Shutdown)
            .map_err(|_| PipeWireServiceError)
    }
}

/// handle ฝั่ง application สำหรับส่งคำสั่งและรับ event แบบไม่บล็อก
#[derive(Clone)]
pub struct PipeWireService {
    commands: PipeWireCommandSender,
    events: Arc<Mutex<mpsc::Receiver<PipeWireEvent>>>,
}

impl PipeWireService {
    /// คืน command sender ที่ส่งต่อให้ component อื่นได้
    pub fn command_sender(&self) -> PipeWireCommandSender {
        self.commands.clone()
    }

    pub fn set_selected(
        &self,
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    ) -> Result<(), PipeWireServiceError> {
        self.commands.set_selected(targets, audio_generation)
    }

    pub fn stop_capture(&self) -> Result<(), PipeWireServiceError> {
        self.commands.stop_capture()
    }

    pub fn shutdown(&self) -> Result<(), PipeWireServiceError> {
        self.commands.shutdown()
    }

    /// ดึง event ที่รออยู่ทั้งหมดโดยไม่รอ PipeWire สร้าง event ใหม่
    pub fn drain_events(&self) -> Vec<PipeWireEvent> {
        let Ok(events) = self.events.try_lock() else {
            return Vec::new();
        };
        events.try_iter().collect()
    }
}

/// ข้อผิดพลาดที่ระบุว่า command channel ใช้งานไม่ได้แล้ว
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipeWireServiceError;

impl fmt::Display for PipeWireServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PipeWire service is no longer running")
    }
}

impl std::error::Error for PipeWireServiceError {}

/// เริ่ม PipeWire service thread และคืน handle ทันทีโดยไม่รอการเชื่อมต่อ
pub fn spawn_service(audio: LatestQueue<SourceAudioChunk>) -> PipeWireService {
    let (command_sender, command_receiver) = pw::channel::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let commands = PipeWireCommandSender {
        sender: command_sender,
    };
    let service = PipeWireService {
        commands,
        events: Arc::new(Mutex::new(event_receiver)),
    };

    let failure_events = event_sender.clone();
    if let Err(error) = thread::Builder::new()
        .name("pipewire-service".to_owned())
        .spawn(move || run_service(command_receiver, event_sender, audio))
    {
        let _ = failure_events.send(PipeWireEvent::Error(format!(
            "failed to start PipeWire service thread: {error}"
        )));
    }

    service
}

fn run_service(
    command_receiver: pw::channel::Receiver<Command>,
    events: mpsc::Sender<PipeWireEvent>,
    audio: LatestQueue<SourceAudioChunk>,
) {
    pw::init();

    let result = (|| -> Result<(), pw::Error> {
        let mainloop = pw::MainLoop::new()?;
        let context = pw::Context::new(&mainloop)?;
        let core = context.connect(None)?;
        let registry = core.get_registry()?;
        let state = Rc::new(RefCell::new(ServiceState::new(
            mainloop.clone(),
            events.clone(),
            audio,
        )));

        let core_events = events.clone();
        let _core_listener = core
            .add_listener_local()
            .error(move |id, _, result, message| {
                let _ = core_events.send(PipeWireEvent::Error(format!(
                    "PipeWire core error on object {id} ({result}): {message}"
                )));
            })
            .register();

        let added_state = Rc::clone(&state);
        let removed_state = Rc::clone(&state);
        let _registry_listener = registry
            .add_listener_local()
            .global(move |object| {
                let Some(stream) = normalize_stream(object) else {
                    return;
                };
                added_state.borrow_mut().add_stream(stream);
            })
            .global_remove(move |runtime_id| {
                removed_state.borrow_mut().remove_stream(runtime_id);
            })
            .register();

        let command_state = Rc::clone(&state);
        let command_loop = mainloop.clone();
        let _commands = command_receiver.attach(&mainloop, move |command| {
            let mut state = command_state.borrow_mut();
            match command {
                Command::SetSelected {
                    targets,
                    audio_generation,
                } => state.set_selected(targets, audio_generation),
                Command::StopCapture => state.stop_capture(),
                Command::Shutdown => {
                    state.stop_capture();
                    command_loop.quit();
                }
            }
        });

        state.borrow().emit_snapshot();
        mainloop.run();
        Ok(())
    })();

    if let Err(error) = result {
        let _ = events.send(PipeWireEvent::Error(format!(
            "PipeWire service unavailable: {error}"
        )));
    }
}

/// สถานะทั้งหมดที่ต้องแก้บน PipeWire service thread เท่านั้น
struct ServiceState {
    /// main loop ที่ใช้สร้างและควบคุม capture stream
    mainloop: pw::MainLoop,
    /// channel ส่ง graph/capture event กลับไปยัง application runtime
    events: mpsc::Sender<PipeWireEvent>,
    /// คิวเสียงร่วมที่ callback ใช้ส่งข้อมูลไป mixer โดยไม่รอ
    audio: LatestQueue<SourceAudioChunk>,
    /// playback stream ปัจจุบัน indexed ด้วย runtime node ID
    streams: HashMap<u32, StreamInfo>,
    /// capture session ที่ต่ออยู่ indexed ด้วย runtime node ID
    captures: HashMap<u32, CaptureSession>,
    /// กฎ application/stream แบบคงทนที่ผู้ใช้เลือก
    selected: Vec<CaptureTarget>,
    /// ป้องกัน `reconcile` ต่อ capture ใหม่หลังได้รับคำสั่งหยุด
    capture_enabled: bool,
    /// รุ่น pipeline ล่าสุด ป้องกัน command เก่ากลับมาเปิด capture ซ้ำ
    audio_generation: u64,
}

impl ServiceState {
    fn new(
        mainloop: pw::MainLoop,
        events: mpsc::Sender<PipeWireEvent>,
        audio: LatestQueue<SourceAudioChunk>,
    ) -> Self {
        Self {
            mainloop,
            events,
            audio,
            streams: HashMap::new(),
            captures: HashMap::new(),
            selected: Vec::new(),
            capture_enabled: true,
            audio_generation: 0,
        }
    }

    fn add_stream(&mut self, stream: StreamInfo) {
        self.streams.insert(stream.runtime_id, stream);
        self.emit_snapshot();
        self.reconcile();
    }

    fn remove_stream(&mut self, runtime_id: u32) {
        if self.streams.remove(&runtime_id).is_none() {
            return;
        }
        self.detach(runtime_id);
        self.emit_snapshot();
    }

    fn set_selected(&mut self, targets: Vec<CaptureTarget>, audio_generation: u64) {
        if audio_generation < self.audio_generation {
            return;
        }
        if audio_generation > self.audio_generation {
            let runtime_ids: Vec<_> = self.captures.keys().copied().collect();
            for runtime_id in runtime_ids {
                self.detach(runtime_id);
            }
        }
        self.audio_generation = audio_generation;
        self.selected = targets;
        self.capture_enabled = true;
        self.reconcile();
    }

    fn stop_capture(&mut self) {
        self.capture_enabled = false;
        let runtime_ids: Vec<_> = self.captures.keys().copied().collect();
        for runtime_id in runtime_ids {
            self.detach(runtime_id);
        }
    }

    fn reconcile(&mut self) {
        // คำนวณจาก metadata ทุกครั้งเพื่อให้ application ที่เปิดใหม่และได้ node ID ใหม่
        // เชื่อมกลับอัตโนมัติ โดยไม่ใช้ runtime ID เป็นตัวตนถาวร
        let desired: HashSet<u32> = if self.capture_enabled {
            self.streams
                .values()
                .filter(|stream| self.selected.iter().any(|target| target.matches(stream)))
                .map(|stream| stream.runtime_id)
                .collect()
        } else {
            HashSet::new()
        };

        let stale: Vec<_> = self
            .captures
            .keys()
            .filter(|runtime_id| !desired.contains(runtime_id))
            .copied()
            .collect();
        for runtime_id in stale {
            self.detach(runtime_id);
        }

        let missing: Vec<_> = desired
            .into_iter()
            .filter(|runtime_id| !self.captures.contains_key(runtime_id))
            .filter_map(|runtime_id| self.streams.get(&runtime_id).cloned())
            .collect();
        for stream in missing {
            match create_capture(
                &self.mainloop,
                &stream,
                self.audio_generation,
                self.audio.clone(),
                self.events.clone(),
            ) {
                Ok(capture) => {
                    self.captures.insert(stream.runtime_id, capture);
                }
                Err(message) => {
                    let _ = self.events.send(PipeWireEvent::CaptureError {
                        runtime_id: stream.runtime_id,
                        audio_generation: self.audio_generation,
                        message,
                    });
                }
            }
        }
    }

    fn detach(&mut self, runtime_id: u32) {
        let Some(capture) = self.captures.remove(&runtime_id) else {
            return;
        };
        let audio_generation = capture.generation();
        capture.disconnect();
        let _ = self.events.send(PipeWireEvent::CaptureStopped {
            runtime_id,
            audio_generation,
        });
    }

    fn emit_snapshot(&self) {
        let mut streams: Vec<_> = self.streams.values().cloned().collect();
        streams.sort_by_key(|stream| stream.runtime_id);
        let _ = self.events.send(PipeWireEvent::StreamsChanged(streams));
    }
}

/// แปลง PipeWire registry object เฉพาะ playback audio node เป็นชนิดภายในระบบ
fn normalize_stream(
    object: &pw::registry::GlobalObject<pw::spa::dict::ForeignDict>,
) -> Option<StreamInfo> {
    if object.type_ != pw::types::ObjectType::Node {
        return None;
    }
    let properties = object.props.as_ref()?;
    if properties.get(*pw::keys::MEDIA_CLASS) != Some("Stream/Output/Audio") {
        return None;
    }

    Some(StreamInfo {
        runtime_id: object.id,
        object_serial: owned_property(properties, "object.serial"),
        node_name: owned_property(properties, *pw::keys::NODE_NAME),
        media_name: owned_property(properties, *pw::keys::MEDIA_NAME),
        application: ApplicationIdentity {
            application_id: owned_property(properties, *pw::keys::APP_ID),
            process_binary: owned_property(properties, *pw::keys::APP_PROCESS_BINARY),
            application_name: owned_property(properties, *pw::keys::APP_NAME),
        },
    })
}

/// คัดลอก property ที่มีค่าออกจาก dictionary ซึ่งมีอายุผูกกับ PipeWire object
fn owned_property(properties: &pw::spa::dict::ForeignDict, key: &str) -> Option<String> {
    properties
        .get(key)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
