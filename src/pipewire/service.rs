use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use pipewire as pw;

use crate::audio::{LatestQueue, SourceAudioChunk};

use super::{
    ApplicationIdentity, CaptureTarget, PipeWireEvent, StreamInfo,
    capture::{CaptureSession, create_capture},
};

enum Command {
    SetSelected(Vec<CaptureTarget>),
    StopCapture,
    Shutdown,
}

#[derive(Clone)]
pub struct PipeWireCommandSender {
    sender: pw::channel::Sender<Command>,
}

impl PipeWireCommandSender {
    pub fn set_selected(&self, targets: Vec<CaptureTarget>) -> Result<(), PipeWireServiceError> {
        self.sender
            .send(Command::SetSelected(targets))
            .map_err(|_| PipeWireServiceError)
    }

    pub fn stop_capture(&self) -> Result<(), PipeWireServiceError> {
        self.sender
            .send(Command::StopCapture)
            .map_err(|_| PipeWireServiceError)
    }

    pub fn shutdown(&self) -> Result<(), PipeWireServiceError> {
        self.sender
            .send(Command::Shutdown)
            .map_err(|_| PipeWireServiceError)
    }
}

#[derive(Clone)]
pub struct PipeWireService {
    commands: PipeWireCommandSender,
    events: Arc<Mutex<mpsc::Receiver<PipeWireEvent>>>,
}

impl PipeWireService {
    pub fn command_sender(&self) -> PipeWireCommandSender {
        self.commands.clone()
    }

    pub fn set_selected(&self, targets: Vec<CaptureTarget>) -> Result<(), PipeWireServiceError> {
        self.commands.set_selected(targets)
    }

    pub fn stop_capture(&self) -> Result<(), PipeWireServiceError> {
        self.commands.stop_capture()
    }

    pub fn shutdown(&self) -> Result<(), PipeWireServiceError> {
        self.commands.shutdown()
    }

    /// Drains all currently queued events without waiting for PipeWire.
    pub fn drain_events(&self) -> Vec<PipeWireEvent> {
        let Ok(events) = self.events.try_lock() else {
            return Vec::new();
        };
        events.try_iter().collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipeWireServiceError;

impl fmt::Display for PipeWireServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PipeWire service is no longer running")
    }
}

impl std::error::Error for PipeWireServiceError {}

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
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;
        let state = Rc::new(RefCell::new(ServiceState::new(
            core.clone(),
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
        let _commands = command_receiver.attach(mainloop.loop_(), move |command| {
            let mut state = command_state.borrow_mut();
            match command {
                Command::SetSelected(targets) => state.set_selected(targets),
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

struct ServiceState {
    core: pw::core::CoreRc,
    events: mpsc::Sender<PipeWireEvent>,
    audio: LatestQueue<SourceAudioChunk>,
    streams: HashMap<u32, StreamInfo>,
    captures: HashMap<u32, CaptureSession>,
    selected: Vec<CaptureTarget>,
    capture_enabled: bool,
}

impl ServiceState {
    fn new(
        core: pw::core::CoreRc,
        events: mpsc::Sender<PipeWireEvent>,
        audio: LatestQueue<SourceAudioChunk>,
    ) -> Self {
        Self {
            core,
            events,
            audio,
            streams: HashMap::new(),
            captures: HashMap::new(),
            selected: Vec::new(),
            capture_enabled: true,
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

    fn set_selected(&mut self, targets: Vec<CaptureTarget>) {
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
            match create_capture(&self.core, &stream, self.audio.clone(), self.events.clone()) {
                Ok(capture) => {
                    self.captures.insert(stream.runtime_id, capture);
                }
                Err(message) => {
                    let _ = self.events.send(PipeWireEvent::CaptureError {
                        runtime_id: stream.runtime_id,
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
        capture.disconnect();
        let _ = self.events.send(PipeWireEvent::CaptureStopped(runtime_id));
    }

    fn emit_snapshot(&self) {
        let mut streams: Vec<_> = self.streams.values().cloned().collect();
        streams.sort_by_key(|stream| stream.runtime_id);
        let _ = self.events.send(PipeWireEvent::StreamsChanged(streams));
    }
}

fn normalize_stream(
    object: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) -> Option<StreamInfo> {
    if object.type_ != pw::types::ObjectType::Node {
        return None;
    }
    let properties = object.props?;
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

fn owned_property(properties: &pw::spa::utils::dict::DictRef, key: &str) -> Option<String> {
    properties
        .get(key)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
