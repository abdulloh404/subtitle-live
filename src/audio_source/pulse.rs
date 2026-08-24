//! PulseAudio fallback แบบ native ที่ทำงานผ่าน Pulse utilities ที่ติดตั้งในระบบ
//!
//! backend นี้ตั้งใจให้ใช้เฉพาะ monitor ค่าเริ่มต้นของ server และทำงานได้ทั้งกับ native
//! PulseAudio server และ `pipewire-pulse` server ของ PipeWire แต่ไม่สามารถแยก application
//! stream แต่ละตัวได้

use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read},
    mem::size_of,
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::audio::{AudioBufferPool, LatestQueue, SAMPLE_RATE_HZ, SourceAudioChunk};

use super::{ApplicationIdentity, AudioSourceEvent, CaptureTarget, StreamInfo};

/// ID เฉพาะ session ที่สงวนไว้ให้ synthetic PulseAudio monitor ค่าเริ่มต้น
pub(crate) const DEFAULT_MONITOR_RUNTIME_ID: u32 = u32::MAX;
const CAPTURE_BUFFER_COUNT: usize = 8;
const CAPTURE_BUFFER_MAX_SAMPLES: usize = 4_096;
const CAPTURE_READ_BYTES: usize = CAPTURE_BUFFER_MAX_SAMPLES * size_of::<f32>();
/// เก็บ diagnostic tail ขนาดสูงสุดจาก process `parec` ที่ทำงานต่อเนื่อง
const CAPTURE_STDERR_MAX_BYTES: usize = 16 * 1_024;
/// ป้องกัน PulseAudio client connection ที่เสียหายไม่ให้ service shutdown ค้างตลอดไป
const PACTL_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const PACTL_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone)]
enum CommandMessage {
    SetSelected {
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    },
    StopCapture,
    Shutdown,
}

/// command endpoint ของ PulseAudio service thread ที่ clone ได้
#[derive(Clone)]
pub(crate) struct PulseAudioCommandSender {
    sender: mpsc::Sender<CommandMessage>,
}

impl PulseAudioCommandSender {
    pub(crate) fn set_selected(
        &self,
        targets: Vec<CaptureTarget>,
        audio_generation: u64,
    ) -> Result<(), PulseAudioServiceError> {
        self.sender
            .send(CommandMessage::SetSelected {
                targets,
                audio_generation,
            })
            .map_err(|_| PulseAudioServiceError)
    }

    pub(crate) fn stop_capture(&self) -> Result<(), PulseAudioServiceError> {
        self.sender
            .send(CommandMessage::StopCapture)
            .map_err(|_| PulseAudioServiceError)
    }

    pub(crate) fn shutdown(&self) -> Result<(), PulseAudioServiceError> {
        self.sender
            .send(CommandMessage::Shutdown)
            .map_err(|_| PulseAudioServiceError)
    }
}

pub(crate) struct PulseAudioService {
    commands: PulseAudioCommandSender,
    events: Arc<Mutex<mpsc::Receiver<AudioSourceEvent>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl PulseAudioService {
    pub(crate) fn command_sender(&self) -> PulseAudioCommandSender {
        self.commands.clone()
    }

    pub(crate) fn shutdown(&self) -> Result<(), PulseAudioServiceError> {
        self.commands.shutdown()
    }

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        let _ = self.shutdown();
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| "PulseAudio service thread panicked while shutting down".to_owned())
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    pub(crate) fn drain_events(&self) -> Vec<AudioSourceEvent> {
        let Ok(events) = self.events.try_lock() else {
            return Vec::new();
        };
        events.try_iter().collect()
    }
}

impl Drop for PulseAudioService {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PulseAudioServiceError;

impl fmt::Display for PulseAudioServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PulseAudio service is no longer running")
    }
}

impl std::error::Error for PulseAudioServiceError {}

pub(crate) fn spawn_service(audio: LatestQueue<SourceAudioChunk>) -> PulseAudioService {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let commands = PulseAudioCommandSender {
        sender: command_sender,
    };
    let failure_events = event_sender.clone();
    let worker = match thread::Builder::new()
        .name("pulseaudio-service".to_owned())
        .spawn(move || run_service(command_receiver, event_sender, audio))
    {
        Ok(worker) => Some(worker),
        Err(error) => {
            let _ = failure_events.send(AudioSourceEvent::Error(format!(
                "Failed to start PulseAudio service thread: {error}"
            )));
            None
        }
    };

    PulseAudioService {
        commands,
        events: Arc::new(Mutex::new(event_receiver)),
        worker,
    }
}

fn run_service(
    commands: mpsc::Receiver<CommandMessage>,
    events: mpsc::Sender<AudioSourceEvent>,
    audio: LatestQueue<SourceAudioChunk>,
) {
    if let Err(error) = resolve_default_monitor() {
        let _ = events.send(AudioSourceEvent::StreamsChanged(Vec::new()));
        let _ = events.send(AudioSourceEvent::Error(error));
        return;
    }

    let stream = default_monitor_stream();
    let _ = events.send(AudioSourceEvent::StreamsChanged(vec![stream.clone()]));

    let mut generation = 0;
    let mut capture: Option<CaptureProcess> = None;
    loop {
        if let Some(process) = capture.as_mut() {
            if let Some(error) = process.reader_failure() {
                let process = capture.take().expect("capture process exists");
                process.finish(&events, Some(error));
            } else if let Some(status) = process.exit_status() {
                let failure = if status.success() {
                    Some("PulseAudio capture process ended unexpectedly".to_owned())
                } else {
                    Some(format!(
                        "PulseAudio capture process exited with status {status}"
                    ))
                };
                let process = capture.take().expect("capture process exists");
                process.finish(&events, failure);
            }
        }

        match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(CommandMessage::SetSelected {
                targets,
                audio_generation,
            }) => {
                if audio_generation < generation {
                    continue;
                }

                let generation_changed = audio_generation != generation;
                generation = audio_generation;
                let selected = targets.iter().any(|target| target.matches(&stream));
                if capture.is_some() && (generation_changed || !selected) {
                    capture
                        .take()
                        .expect("capture process exists")
                        .finish(&events, None);
                }
                if selected && capture.is_none() {
                    let started = resolve_default_monitor().and_then(|monitor_name| {
                        CaptureProcess::start(generation, &monitor_name, audio.clone())
                    });
                    match started {
                        Ok(process) => {
                            let _ = events.send(AudioSourceEvent::CaptureStarted {
                                runtime_id: DEFAULT_MONITOR_RUNTIME_ID,
                                audio_generation: generation,
                            });
                            capture = Some(process);
                        }
                        Err(message) => {
                            let _ = events.send(AudioSourceEvent::CaptureError {
                                runtime_id: DEFAULT_MONITOR_RUNTIME_ID,
                                audio_generation: generation,
                                message,
                            });
                        }
                    }
                }
            }
            Ok(CommandMessage::StopCapture) => {
                if let Some(process) = capture.take() {
                    process.finish(&events, None);
                }
            }
            Ok(CommandMessage::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(process) = capture.take() {
                    process.finish(&events, None);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// สร้าง source identity ที่คงที่และไม่ขึ้นกับอุปกรณ์ output ปัจจุบัน
///
/// จะ resolve monitor ก่อนเริ่ม `parec` ทุกครั้ง เพื่อให้ Retry ตาม sink ค่าเริ่มต้นที่เปลี่ยนไป
/// แทนการเปิด device name ที่ล้าสมัย
fn default_monitor_stream() -> StreamInfo {
    StreamInfo {
        runtime_id: DEFAULT_MONITOR_RUNTIME_ID,
        object_serial: None,
        node_name: Some("pulse-default-monitor".to_owned()),
        media_name: Some("Default system output (PulseAudio monitor)".to_owned()),
        application: ApplicationIdentity {
            application_id: Some("subtitle-live.pulse-default-monitor".to_owned()),
            process_binary: None,
            application_name: Some("PulseAudio system output".to_owned()),
        },
    }
}

fn resolve_default_monitor() -> Result<String, String> {
    let output = run_pactl("info").map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            "PulseAudio backend requires the `pactl` utility, but it was not found".to_owned()
        }
        _ => format!("Failed to run `pactl info`: {error}"),
    })?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return if detail.is_empty() {
            Err(format!(
                "PulseAudio server is unavailable (`pactl info` exited with {})",
                output.status
            ))
        } else {
            Err(format!("PulseAudio server is unavailable: {detail}"))
        };
    }

    let info = String::from_utf8_lossy(&output.stdout);
    let default_sink = query_default_sink().or_else(|| {
        info.lines()
            .find_map(|line| line.trim_start().strip_prefix("Default Sink:"))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    });
    let Some(default_sink) = default_sink else {
        return Err("PulseAudio server did not report a default output sink".to_owned());
    };
    Ok(format!("{default_sink}.monitor"))
}

fn query_default_sink() -> Option<String> {
    let output = run_pactl("get-default-sink").ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// รัน `pactl` query แบบสั้นพร้อม hard deadline เพื่อให้ Stop/Shutdown จบในเวลาที่จำกัด
fn run_pactl(argument: &str) -> io::Result<Output> {
    let mut child = Command::new("pactl")
        .arg(argument)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + PACTL_COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("`pactl {argument}` timed out"),
                ));
            }
            Ok(None) => thread::sleep(PACTL_POLL_INTERVAL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Err(error);
            }
        }
    }
}

enum ReaderMessage {
    Finished,
    Error(String),
}

struct CaptureProcess {
    child: Child,
    audio_reader: Option<thread::JoinHandle<()>>,
    stderr_reader: Option<thread::JoinHandle<String>>,
    reader_messages: mpsc::Receiver<ReaderMessage>,
    generation: u64,
}

impl CaptureProcess {
    fn start(
        generation: u64,
        monitor_name: &str,
        audio: LatestQueue<SourceAudioChunk>,
    ) -> Result<Self, String> {
        let mut child = Command::new("parec")
            .arg(format!("--device={monitor_name}"))
            .args([
                "--format=float32le",
                "--rate=16000",
                "--channels=1",
                "--latency-msec=20",
                "--raw",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    "PulseAudio backend requires the `parec` utility, but it was not found"
                        .to_owned()
                }
                _ => format!("Failed to start PulseAudio capture with `parec`: {error}"),
            })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            "Failed to open the PulseAudio capture output pipe".to_owned()
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            "Failed to open the PulseAudio capture error pipe".to_owned()
        })?;
        let (reader_sender, reader_messages) = mpsc::channel();
        let audio_reader = thread::Builder::new()
            .name("pulseaudio-capture-reader".to_owned())
            .spawn(move || read_capture(stdout, generation, audio, reader_sender))
            .map_err(|error| {
                let _ = child.kill();
                let _ = child.wait();
                format!("Failed to start PulseAudio capture reader thread: {error}")
            })?;
        let stderr_reader = match thread::Builder::new()
            .name("pulseaudio-error-reader".to_owned())
            .spawn(move || read_error_output(stderr))
        {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = audio_reader.join();
                return Err(format!(
                    "Failed to start PulseAudio error reader thread: {error}"
                ));
            }
        };

        Ok(Self {
            child,
            audio_reader: Some(audio_reader),
            stderr_reader: Some(stderr_reader),
            reader_messages,
            generation,
        })
    }

    fn reader_failure(&self) -> Option<String> {
        self.reader_messages
            .try_iter()
            .find_map(|message| match message {
                ReaderMessage::Finished => {
                    Some("PulseAudio capture stream ended unexpectedly".to_owned())
                }
                ReaderMessage::Error(error) => Some(error),
            })
    }

    fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    fn finish(mut self, events: &mpsc::Sender<AudioSourceEvent>, mut failure: Option<String>) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        if let Err(error) = self.child.wait()
            && failure.is_none()
        {
            failure = Some(format!(
                "Failed to wait for PulseAudio capture process: {error}"
            ));
        }

        if let Some(reader) = self.audio_reader.take()
            && reader.join().is_err()
            && failure.is_none()
        {
            failure = Some("PulseAudio capture reader thread panicked".to_owned());
        }
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        if let Some(message) = failure {
            let detail = stderr.trim();
            let message = if detail.is_empty() {
                message
            } else {
                format!("{message}: {detail}")
            };
            let _ = events.send(AudioSourceEvent::CaptureError {
                runtime_id: DEFAULT_MONITOR_RUNTIME_ID,
                audio_generation: self.generation,
                message,
            });
        } else {
            let _ = events.send(AudioSourceEvent::CaptureStopped {
                runtime_id: DEFAULT_MONITOR_RUNTIME_ID,
                audio_generation: self.generation,
            });
        }
    }
}

fn read_capture(
    mut stdout: impl Read,
    generation: u64,
    audio: LatestQueue<SourceAudioChunk>,
    messages: mpsc::Sender<ReaderMessage>,
) {
    let buffers = AudioBufferPool::new(CAPTURE_BUFFER_COUNT, CAPTURE_BUFFER_MAX_SAMPLES);
    let mut bytes = [0_u8; CAPTURE_READ_BYTES + 3];
    let mut pending = 0;

    loop {
        let read = match stdout.read(&mut bytes[pending..]) {
            Ok(0) => {
                let _ = messages.send(ReaderMessage::Finished);
                return;
            }
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = messages.send(ReaderMessage::Error(format!(
                    "Failed to read PulseAudio capture data: {error}"
                )));
                return;
            }
        };

        let available = pending + read;
        let complete = available - available % size_of::<f32>();
        let sample_count = complete / size_of::<f32>();
        if sample_count > 0
            && let Some(mut samples) = buffers.try_acquire(sample_count)
        {
            for sample in bytes[..complete].chunks_exact(size_of::<f32>()) {
                samples.push(f32::from_le_bytes([
                    sample[0], sample[1], sample[2], sample[3],
                ]));
            }
            let captured_end = Instant::now();
            let sample_duration =
                Duration::from_secs_f64(samples.len() as f64 / SAMPLE_RATE_HZ as f64);
            audio.push_latest(SourceAudioChunk {
                generation,
                source_id: DEFAULT_MONITOR_RUNTIME_ID,
                samples,
                captured_at: captured_end
                    .checked_sub(sample_duration)
                    .unwrap_or(captured_end),
            });
        }

        pending = available - complete;
        bytes.copy_within(complete..available, 0);
    }
}

fn read_error_output(mut stderr: impl Read) -> String {
    let mut retained = VecDeque::with_capacity(CAPTURE_STDERR_MAX_BYTES);
    let mut buffer = [0_u8; 1_024];
    loop {
        let read = match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let overflow = retained
            .len()
            .saturating_add(read)
            .saturating_sub(CAPTURE_STDERR_MAX_BYTES);
        retained.drain(..overflow.min(retained.len()));
        retained.extend(&buffer[..read]);
    }
    String::from_utf8_lossy(&retained.into_iter().collect::<Vec<_>>()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MONITOR_RUNTIME_ID, default_monitor_stream};
    use crate::audio_source::CaptureTarget;

    #[test]
    fn synthetic_monitor_can_be_selected_as_one_stream() {
        let stream = default_monitor_stream();
        let target = CaptureTarget::stream(&stream);

        assert_eq!(stream.runtime_id, DEFAULT_MONITOR_RUNTIME_ID);
        assert_eq!(stream.node_name.as_deref(), Some("pulse-default-monitor"));
        assert!(target.matches(&stream));
        assert!(stream.display_name().contains("PulseAudio monitor"));
    }
}
