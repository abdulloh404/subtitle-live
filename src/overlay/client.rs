//! Client ฝั่ง main process สำหรับเปิด helper และส่ง subtitle ผ่าน local stdio IPC

use std::{
    cell::{Cell, RefCell},
    env,
    io::{self, BufReader},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use crate::config::SubtitleConfig;

use super::{OverlayCommand, OverlayEvent, OverlayRuntimeBackend, read_message, write_message};

/// สถานะของ helper ที่ main process ใช้แสดงผลและตัดสินใจ recovery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayClientStatus {
    /// Process เริ่มแล้วแต่ยังไม่ได้รับ Ready
    Starting,
    /// Helper เปิด GTK/X11 display และพร้อมวาดแล้ว
    Ready,
    /// Helper หรือ IPC ล้มเหลว แต่ main pipeline ยังทำงานได้
    Error,
    /// Main ส่ง Shutdown แล้ว
    Stopped,
}

/// เจ้าของ child process และช่องทาง IPC ซึ่ง main GTK thread เรียกแบบไม่ทำ I/O โดยตรง
pub struct OverlayClient {
    /// Backend ที่เลือกจาก desktop session ก่อน spawn
    backend: OverlayRuntimeBackend,
    /// Child handle สำหรับตรวจ exit โดยไม่ block
    child: RefCell<Option<Child>>,
    /// ส่งคำสั่งให้ writer worker
    command_sender: Sender<OverlayCommand>,
    /// รับ event ที่ reader/writer worker ส่งกลับ
    event_receiver: Receiver<OverlayEvent>,
    /// ป้องกันการรายงาน EOF ปกติเป็น crash ระหว่าง shutdown
    shutdown_requested: Arc<AtomicBool>,
    /// สถานะล่าสุดของ helper
    status: Cell<OverlayClientStatus>,
    /// ข้อผิดพลาดล่าสุดที่ปลอดภัยต่อการแสดงใน UI
    last_error: RefCell<Option<String>>,
    /// ป้องกันการส่ง exit error ซ้ำทุก poll
    exit_reported: Cell<bool>,
    /// รหัส subtitle frame ถัดไป
    next_frame_id: Cell<u64>,
    /// config ล่าสุดสำหรับ coalesce การส่งซ้ำจาก GTK poll
    last_config: RefCell<Option<SubtitleConfig>>,
    /// เก็บ handle ไว้สำหรับ clean join ในขั้น lifecycle
    _reader_thread: thread::JoinHandle<()>,
    /// เก็บ handle ไว้สำหรับ clean join ในขั้น lifecycle
    _writer_thread: thread::JoinHandle<()>,
}

impl OverlayClient {
    /// เปิด helper ด้วย X11 backend และส่ง config เริ่มต้นทันที
    pub fn spawn(
        backend: OverlayRuntimeBackend,
        initial_config: &SubtitleConfig,
    ) -> io::Result<Self> {
        if backend == OverlayRuntimeBackend::Unavailable {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "desktop session นี้ไม่มี X11 display สำหรับ subtitle overlay",
            ));
        }

        let mut command = helper_command()?;
        command
            .env("GDK_BACKEND", "x11")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "helper ไม่มี stdin pipe"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "helper ไม่มี stdout pipe"))?;

        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let writer_thread = spawn_command_writer(
            child_stdin,
            command_receiver,
            event_sender.clone(),
            Arc::clone(&shutdown_requested),
        )?;
        let reader_thread =
            spawn_event_reader(child_stdout, event_sender, Arc::clone(&shutdown_requested))?;

        let client = Self {
            backend,
            child: RefCell::new(Some(child)),
            command_sender,
            event_receiver,
            shutdown_requested,
            status: Cell::new(OverlayClientStatus::Starting),
            last_error: RefCell::new(None),
            exit_reported: Cell::new(false),
            next_frame_id: Cell::new(1),
            last_config: RefCell::new(None),
            _reader_thread: reader_thread,
            _writer_thread: writer_thread,
        };
        client.apply_config(initial_config);
        Ok(client)
    }

    /// Backend ที่ helper ถูกเปิดให้ใช้งาน
    pub const fn backend(&self) -> OverlayRuntimeBackend {
        self.backend
    }

    /// สถานะล่าสุดหลังเรียก [`Self::drain_events`]
    pub fn status(&self) -> OverlayClientStatus {
        self.status.get()
    }

    /// ข้อผิดพลาดล่าสุดโดยไม่มีข้อความ subtitle
    pub fn last_error(&self) -> Option<String> {
        self.last_error.borrow().clone()
    }

    /// ส่ง presentation text และคืน frame id ที่ใช้จับคู่ Rendered event
    pub fn show_text(&self, text: &str, is_final: bool) -> u64 {
        let frame_id = self.next_frame_id.get();
        self.next_frame_id.set(frame_id.wrapping_add(1));
        self.send(OverlayCommand::ShowText {
            frame_id,
            text: text.to_owned(),
            is_final,
        });
        frame_id
    }

    /// ล้าง subtitle โดยไม่ปิด helper หรือ unmap X11 window
    pub fn hide(&self) {
        self.send(OverlayCommand::Hide);
    }

    /// ส่ง config เฉพาะเมื่อค่าต่างจากครั้งก่อน
    pub fn apply_config(&self, config: &SubtitleConfig) {
        if self.last_config.borrow().as_ref() == Some(config) {
            return;
        }
        self.send(OverlayCommand::ApplyConfig {
            config: config.clone(),
        });
        self.last_config.replace(Some(config.clone()));
    }

    /// อ่าน event ที่ค้างอยู่และตรวจ child exit โดยไม่ block GTK thread
    pub fn drain_events(&self) -> Vec<OverlayEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_receiver.try_recv() {
                Ok(event) => {
                    self.apply_event_status(&event);
                    events.push(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.record_error("ช่องรับ event จาก overlay helper ถูกปิด".to_owned());
                    break;
                }
            }
        }
        self.check_child_exit(&mut events);
        events
    }

    /// ขอให้ helper ออกจาก event loop โดย main process ไม่รอใน GTK callback
    pub fn shutdown(&self) {
        if self.shutdown_requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.command_sender.send(OverlayCommand::Shutdown);
        self.status.set(OverlayClientStatus::Stopped);
    }

    /// ส่งคำสั่งเข้าคิว writer; ไม่มี pipe I/O บน caller thread
    fn send(&self, command: OverlayCommand) {
        if self.status.get() == OverlayClientStatus::Stopped {
            return;
        }
        if self.command_sender.send(command).is_err() {
            self.record_error("ช่องส่งคำสั่งไป overlay helper ถูกปิด".to_owned());
        }
    }

    /// เปลี่ยนสถานะตาม event ที่ helper ยืนยันกลับมา
    fn apply_event_status(&self, event: &OverlayEvent) {
        match event {
            OverlayEvent::Ready => {
                self.status.set(OverlayClientStatus::Ready);
                self.last_error.borrow_mut().take();
            }
            OverlayEvent::Error { message } => self.record_error(message.clone()),
            OverlayEvent::Pong { .. } | OverlayEvent::Rendered { .. } => {}
        }
    }

    /// ตรวจ process exit ครั้งเดียวและแปลงเป็น event ที่ main runtime จัดการได้
    fn check_child_exit(&self, events: &mut Vec<OverlayEvent>) {
        if self.exit_reported.get() || self.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        let mut child_slot = self.child.borrow_mut();
        let Some(child) = child_slot.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.exit_reported.set(true);
                let message = format!("overlay helper ออกก่อนกำหนด: {status}");
                self.record_error(message.clone());
                events.push(OverlayEvent::Error { message });
            }
            Ok(None) => {}
            Err(error) => {
                self.exit_reported.set(true);
                let message = format!("ตรวจสถานะ overlay helper ไม่สำเร็จ: {error}");
                self.record_error(message.clone());
                events.push(OverlayEvent::Error { message });
            }
        }
    }

    /// เก็บ error ล่าสุดโดยไม่ทำให้ main application หยุด
    fn record_error(&self, message: String) {
        self.status.set(OverlayClientStatus::Error);
        self.last_error.replace(Some(message));
    }
}

impl Drop for OverlayClient {
    fn drop(&mut self) {
        self.shutdown_requested.store(true, Ordering::Release);
        let _ = self.command_sender.send(OverlayCommand::Shutdown);
    }
}

/// เลือก binary แยกก่อน และ fallback เป็น main binary internal mode สำหรับ `cargo run`
fn helper_command() -> io::Result<Command> {
    let current_executable = env::current_exe()?;
    let helper_executable = sibling_helper_path(&current_executable);
    if helper_executable.is_file() {
        Ok(Command::new(helper_executable))
    } else {
        let mut command = Command::new(current_executable);
        command.arg("--overlay-helper");
        Ok(command)
    }
}

/// สร้าง path ของ helper ที่ติดตั้งอยู่ข้าง main executable
fn sibling_helper_path(current_executable: &std::path::Path) -> PathBuf {
    current_executable.with_file_name("subtitle-live-overlay")
}

/// Worker ที่ serialize command ลง child stdin
fn spawn_command_writer(
    mut child_stdin: ChildStdin,
    command_receiver: Receiver<OverlayCommand>,
    event_sender: Sender<OverlayEvent>,
    shutdown_requested: Arc<AtomicBool>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("overlay-client-writer".to_owned())
        .spawn(move || {
            while let Ok(command) = command_receiver.recv() {
                let shutting_down = matches!(command, OverlayCommand::Shutdown);
                if let Err(error) = write_message(&mut child_stdin, &command) {
                    if !shutdown_requested.load(Ordering::Acquire) {
                        let _ = event_sender.send(OverlayEvent::Error {
                            message: format!("ส่งคำสั่งไป overlay helper ไม่สำเร็จ: {error}"),
                        });
                    }
                    break;
                }
                if shutting_down {
                    break;
                }
            }
        })
}

/// Worker ที่ deserialize event จาก child stdout
fn spawn_event_reader(
    child_stdout: ChildStdout,
    event_sender: Sender<OverlayEvent>,
    shutdown_requested: Arc<AtomicBool>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("overlay-client-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(child_stdout);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(event)) => {
                        if event_sender.send(event).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        if !shutdown_requested.load(Ordering::Acquire) {
                            let _ = event_sender.send(OverlayEvent::Error {
                                message: "overlay helper ปิด output pipe".to_owned(),
                            });
                        }
                        break;
                    }
                    Err(error) => {
                        if !shutdown_requested.load(Ordering::Acquire) {
                            let _ = event_sender.send(OverlayEvent::Error {
                                message: format!("อ่าน event จาก overlay helper ไม่สำเร็จ: {error}"),
                            });
                        }
                        break;
                    }
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::sibling_helper_path;

    #[test]
    fn helper_binary_is_resolved_next_to_main_executable() {
        assert_eq!(
            sibling_helper_path(Path::new("/opt/subtitle-live/bin/subtitle-live")),
            Path::new("/opt/subtitle-live/bin/subtitle-live-overlay")
        );
    }
}
