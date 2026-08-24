//! Client ฝั่ง main process สำหรับเปิด helper และส่ง subtitle ผ่าน local stdio IPC

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    env,
    io::{self, BufReader},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::config::SubtitleConfig;

use super::{OverlayCommand, OverlayEvent, OverlayRuntimeBackend, read_message, write_message};

/// จำนวน control message สูงสุด; subtitle frame จะถูกแทนด้วย frame ล่าสุดเสมอ
const MAX_PENDING_COMMANDS: usize = 16;
/// เวลาที่ให้ helper ปิดตัวเองก่อนบังคับหยุดหลัง GTK event loop จบแล้ว
const HELPER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// ช่วงตรวจสถานะ child ระหว่างรอ shutdown โดยไม่วนใช้ CPU เต็ม
const CHILD_WAIT_INTERVAL: Duration = Duration::from_millis(10);

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
    /// Mailbox แบบ bounded ซึ่งเก็บ subtitle frame ล่าสุดแทนการสะสม frame เก่า
    command_mailbox: Arc<CommandMailbox>,
    /// รับ event ที่ reader/writer worker ส่งกลับ
    event_receiver: Receiver<OverlayEvent>,
    /// ป้องกันการรายงาน EOF ปกติเป็น crash ระหว่าง shutdown
    shutdown_requested: Arc<AtomicBool>,
    /// Error ของ transport เป็น terminal จนกว่าจะสร้าง client ใหม่
    transport_failed: Arc<AtomicBool>,
    /// สถานะล่าสุดของ helper
    status: Cell<OverlayClientStatus>,
    /// ข้อผิดพลาดล่าสุดที่ปลอดภัยต่อการแสดงใน UI
    last_error: RefCell<Option<String>>,
    /// ป้องกันการส่ง exit error ซ้ำทุก poll
    exit_reported: Cell<bool>,
    /// ป้องกันการรายงาน event channel ที่ปิดซ้ำทุก GTK poll
    event_disconnect_reported: Cell<bool>,
    /// รหัส subtitle frame ถัดไป
    next_frame_id: Cell<u64>,
    /// config ล่าสุดสำหรับ coalesce การส่งซ้ำจาก GTK poll
    last_config: RefCell<Option<SubtitleConfig>>,
    /// Reader worker ซึ่ง [`Self::finish`] จะ join หลัง event loop จบ
    reader_thread: RefCell<Option<thread::JoinHandle<()>>>,
    /// Writer worker ซึ่ง [`Self::finish`] จะ join หลัง event loop จบ
    writer_thread: RefCell<Option<thread::JoinHandle<()>>>,
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
                "this desktop session has no X11 display for the subtitle overlay",
            ));
        }

        let mut command = helper_command()?;
        command
            .env("GDK_BACKEND", "x11")
            // helper เป็น passive overlay จึงห้ามรับ activation token ของหน้าต่าง Settings
            .env_remove("DESKTOP_STARTUP_ID")
            .env_remove("XDG_ACTIVATION_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let Some(child_stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the overlay helper has no stdin pipe",
            ));
        };
        let Some(child_stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the overlay helper has no stdout pipe",
            ));
        };

        let (event_sender, event_receiver) = mpsc::channel();
        let command_mailbox = Arc::new(CommandMailbox::default());
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let transport_failed = Arc::new(AtomicBool::new(false));
        let writer_thread = match spawn_command_writer(
            child_stdin,
            Arc::clone(&command_mailbox),
            event_sender.clone(),
            Arc::clone(&shutdown_requested),
            Arc::clone(&transport_failed),
        ) {
            Ok(thread) => thread,
            Err(error) => {
                terminate_child(&mut child);
                return Err(error);
            }
        };
        let reader_thread = match spawn_event_reader(
            child_stdout,
            event_sender,
            Arc::clone(&command_mailbox),
            Arc::clone(&shutdown_requested),
            Arc::clone(&transport_failed),
        ) {
            Ok(thread) => thread,
            Err(error) => {
                command_mailbox.close();
                terminate_child(&mut child);
                let _ = writer_thread.join();
                return Err(error);
            }
        };

        let client = Self {
            backend,
            child: RefCell::new(Some(child)),
            command_mailbox,
            event_receiver,
            shutdown_requested,
            transport_failed,
            status: Cell::new(OverlayClientStatus::Starting),
            last_error: RefCell::new(None),
            exit_reported: Cell::new(false),
            event_disconnect_reported: Cell::new(false),
            next_frame_id: Cell::new(1),
            last_config: RefCell::new(None),
            reader_thread: RefCell::new(Some(reader_thread)),
            writer_thread: RefCell::new(Some(writer_thread)),
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

    /// ส่ง presentation text และคืน frame id เฉพาะเมื่อรับเข้าคิว IPC สำเร็จ
    pub fn show_text(&self, text: &str, is_final: bool) -> Option<u64> {
        let frame_id = self.next_frame_id.get();
        self.next_frame_id.set(frame_id.wrapping_add(1).max(1));
        self.send(OverlayCommand::ShowText {
            frame_id,
            text: text.to_owned(),
            is_final,
        })
        .then_some(frame_id)
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
        if self.send(OverlayCommand::ApplyConfig {
            config: config.clone(),
        }) {
            self.last_config.replace(Some(config.clone()));
        }
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
                    if !self.event_disconnect_reported.replace(true)
                        && !self.shutdown_requested.load(Ordering::Acquire)
                        && self.status.get() != OverlayClientStatus::Error
                    {
                        let message = "the overlay helper event channel was closed".to_owned();
                        self.record_error(message.clone());
                        events.push(OverlayEvent::Error { message });
                    }
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
        self.command_mailbox.enqueue(OverlayCommand::Shutdown);
        self.status.set(OverlayClientStatus::Stopped);
    }

    /// รอ helper และ worker หลัง GTK event loop จบ; ไม่ควรเรียกจาก GTK callback
    pub fn finish(&self) -> io::Result<()> {
        self.shutdown();
        let child_result = self.wait_for_child();
        self.command_mailbox.close();
        let writer_result = join_worker(&self.writer_thread, "overlay writer");
        let reader_result = join_worker(&self.reader_thread, "overlay reader");

        child_result.and(writer_result).and(reader_result)
    }

    /// ส่งคำสั่งเข้าคิว writer; ไม่มี pipe I/O บน caller thread
    fn send(&self, command: OverlayCommand) -> bool {
        if matches!(
            self.status.get(),
            OverlayClientStatus::Error | OverlayClientStatus::Stopped
        ) {
            return false;
        }
        if !self.command_mailbox.enqueue(command) {
            self.record_error("the overlay helper command channel was closed".to_owned());
            return false;
        }
        true
    }

    /// เปลี่ยนสถานะตาม event ที่ helper ยืนยันกลับมา
    fn apply_event_status(&self, event: &OverlayEvent) {
        match event {
            OverlayEvent::Ready => {
                // Ready ที่มาช้าห้ามล้าง transport error จาก writer/reader อีก thread
                if !self.transport_failed.load(Ordering::Acquire)
                    && self.status.get() == OverlayClientStatus::Starting
                {
                    self.status.set(OverlayClientStatus::Ready);
                    self.last_error.borrow_mut().take();
                }
            }
            OverlayEvent::Error { message } => self.record_error(message.clone()),
            OverlayEvent::MonitorsChanged { .. }
            | OverlayEvent::Pong { .. }
            | OverlayEvent::Rendered { .. } => {}
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
                let message = format!("the overlay helper exited unexpectedly: {status}");
                self.record_error(message.clone());
                events.push(OverlayEvent::Error { message });
            }
            Ok(None) => {}
            Err(error) => {
                self.exit_reported.set(true);
                let message = format!("failed to check the overlay helper status: {error}");
                self.record_error(message.clone());
                events.push(OverlayEvent::Error { message });
            }
        }
    }

    /// เก็บ error ล่าสุดโดยไม่ทำให้ main application หยุด
    fn record_error(&self, message: String) {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        self.transport_failed.store(true, Ordering::Release);
        self.command_mailbox.close();
        self.status.set(OverlayClientStatus::Error);
        self.last_error.replace(Some(message));
    }

    /// รอ child แบบมี timeout แล้ว kill เพื่อไม่ทิ้ง helper ค้างหลัง main process จบ
    fn wait_for_child(&self) -> io::Result<()> {
        let deadline = Instant::now() + HELPER_SHUTDOWN_TIMEOUT;
        let mut child_slot = self.child.borrow_mut();
        let Some(child) = child_slot.as_mut() else {
            return Ok(());
        };

        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    child_slot.take();
                    return Ok(());
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(CHILD_WAIT_INTERVAL),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    child_slot.take();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the overlay helper did not stop before the timeout and was terminated",
                    ));
                }
                Err(error) => {
                    terminate_child(child);
                    child_slot.take();
                    return Err(error);
                }
            }
        }
    }
}

impl Drop for OverlayClient {
    fn drop(&mut self) {
        self.shutdown_requested.store(true, Ordering::Release);
        self.command_mailbox.enqueue(OverlayCommand::Shutdown);
        self.command_mailbox.close();
        if let Some(child) = self.child.get_mut().as_mut() {
            terminate_child(child);
        }
        self.child.get_mut().take();
        join_worker_mut(self.writer_thread.get_mut());
        join_worker_mut(self.reader_thread.get_mut());
    }
}

/// เลือก binary แยกก่อน และ fallback เป็น main binary internal mode สำหรับ `cargo run`
fn helper_command() -> io::Result<Command> {
    let current_executable = env::current_exe()?;
    // `cargo run` ไม่ rebuild binary อื่นเสมอ จึงใช้โค้ดจาก main binary ปัจจุบันใน debug
    if cfg!(debug_assertions) {
        let mut command = Command::new(current_executable);
        command.arg("--overlay-helper");
        return Ok(command);
    }

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

/// Queue ภายในที่ใช้ทดสอบกฎ bounded/coalescing โดยไม่แตะ thread หรือ child process
struct PendingCommandQueue {
    commands: VecDeque<OverlayCommand>,
    accepting: bool,
}

impl PendingCommandQueue {
    /// เริ่ม queue ในสถานะรับคำสั่ง
    fn new() -> Self {
        Self {
            commands: VecDeque::new(),
            accepting: true,
        }
    }

    /// เก็บ control ordering แต่แทน subtitle/config/hide ที่ยังไม่ถูกส่งด้วยค่าล่าสุด
    fn push(&mut self, command: OverlayCommand) -> bool {
        if !self.accepting {
            return false;
        }

        if matches!(&command, OverlayCommand::ApplyConfig { .. })
            && let Some(queued) = self
                .commands
                .iter_mut()
                .find(|queued| matches!(queued, OverlayCommand::ApplyConfig { .. }))
        {
            // แทน config ณ ตำแหน่งเดิมเพื่อไม่สลับลำดับกับ subtitle/control ที่อยู่รอบข้าง
            *queued = command;
            return true;
        }

        match &command {
            OverlayCommand::ShowText { .. } => self
                .commands
                .retain(|queued| !matches!(queued, OverlayCommand::ShowText { .. })),
            OverlayCommand::Hide => self.commands.retain(|queued| {
                !matches!(
                    queued,
                    OverlayCommand::ShowText { .. } | OverlayCommand::Hide
                )
            }),
            OverlayCommand::ApplyConfig { .. } => {}
            OverlayCommand::Shutdown => {
                self.commands.clear();
                self.accepting = false;
            }
            OverlayCommand::Ping { .. } => {}
        }

        if self.commands.len() >= MAX_PENDING_COMMANDS {
            if let Some(index) = self
                .commands
                .iter()
                .position(|queued| matches!(queued, OverlayCommand::Ping { .. }))
            {
                self.commands.remove(index);
            } else {
                return false;
            }
        }
        self.commands.push_back(command);
        true
    }

    /// คืนคำสั่งเก่าสุดหลังผ่านกฎ coalescing
    fn pop(&mut self) -> Option<OverlayCommand> {
        self.commands.pop_front()
    }
}

/// Mailbox ระหว่าง GTK caller กับ writer worker; enqueue ไม่ทำ pipe I/O
struct CommandMailbox {
    state: Mutex<PendingCommandQueue>,
    available: Condvar,
}

impl Default for CommandMailbox {
    fn default() -> Self {
        Self {
            state: Mutex::new(PendingCommandQueue::new()),
            available: Condvar::new(),
        }
    }
}

impl CommandMailbox {
    /// เพิ่มคำสั่งโดยไม่ block นอกจากช่วง lock สั้น ๆ ในหน่วยความจำ
    fn enqueue(&self, command: OverlayCommand) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.push(command) {
            return false;
        }
        self.available.notify_one();
        true
    }

    /// รอคำสั่งถัดไปบน writer thread เท่านั้น
    fn receive(&self) -> Option<OverlayCommand> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(command) = state.pop() {
                return Some(command);
            }
            if !state.accepting {
                return None;
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// ปิด mailbox และปลุก writer เพื่อให้ join ได้แม้ไม่มีคำสั่งค้าง
    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.accepting = false;
        state.commands.clear();
        self.available.notify_all();
    }
}

/// Worker ที่ serialize command ลง child stdin
fn spawn_command_writer(
    mut child_stdin: ChildStdin,
    command_mailbox: Arc<CommandMailbox>,
    event_sender: Sender<OverlayEvent>,
    shutdown_requested: Arc<AtomicBool>,
    transport_failed: Arc<AtomicBool>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("overlay-client-writer".to_owned())
        .spawn(move || {
            while let Some(command) = command_mailbox.receive() {
                let shutting_down = matches!(command, OverlayCommand::Shutdown);
                if let Err(error) = write_message(&mut child_stdin, &command) {
                    if !shutdown_requested.load(Ordering::Acquire)
                        && !transport_failed.swap(true, Ordering::AcqRel)
                    {
                        let _ = event_sender.send(OverlayEvent::Error {
                            message: format!(
                                "failed to send a command to the overlay helper: {error}"
                            ),
                        });
                    }
                    command_mailbox.close();
                    break;
                }
                if shutting_down {
                    command_mailbox.close();
                    break;
                }
            }
        })
}

/// Worker ที่ deserialize event จาก child stdout
fn spawn_event_reader(
    child_stdout: ChildStdout,
    event_sender: Sender<OverlayEvent>,
    command_mailbox: Arc<CommandMailbox>,
    shutdown_requested: Arc<AtomicBool>,
    transport_failed: Arc<AtomicBool>,
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
                        if !shutdown_requested.load(Ordering::Acquire)
                            && !transport_failed.swap(true, Ordering::AcqRel)
                        {
                            command_mailbox.close();
                            let _ = event_sender.send(OverlayEvent::Error {
                                message: "the overlay helper closed its output pipe".to_owned(),
                            });
                        }
                        break;
                    }
                    Err(error) => {
                        if !shutdown_requested.load(Ordering::Acquire)
                            && !transport_failed.swap(true, Ordering::AcqRel)
                        {
                            command_mailbox.close();
                            let _ = event_sender.send(OverlayEvent::Error {
                                message: format!(
                                    "failed to read an event from the overlay helper: {error}"
                                ),
                            });
                        }
                        break;
                    }
                }
            }
        })
}

/// Kill และ reap child ในเส้นทาง error/drop เพื่อไม่ทิ้ง process หรือ zombie
fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Join worker หลัง event loop พร้อมแปลง panic เป็น I/O error ที่รายงานได้
fn join_worker(
    worker: &RefCell<Option<thread::JoinHandle<()>>>,
    worker_name: &str,
) -> io::Result<()> {
    let Some(worker) = worker.borrow_mut().take() else {
        return Ok(());
    };
    worker.join().map_err(|_| {
        io::Error::other(format!(
            "{worker_name} panicked while stopping the overlay helper"
        ))
    })
}

/// Join ใน Drop โดยห้าม panic ซ้ำระหว่าง unwinding
fn join_worker_mut(worker: &mut Option<thread::JoinHandle<()>>) {
    if let Some(worker) = worker.take() {
        let _ = worker.join();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{OverlayCommand, PendingCommandQueue, sibling_helper_path};
    use crate::config::SubtitleConfig;

    #[test]
    fn helper_binary_is_resolved_next_to_main_executable() {
        assert_eq!(
            sibling_helper_path(Path::new("/opt/subtitle-live/bin/subtitle-live")),
            Path::new("/opt/subtitle-live/bin/subtitle-live-overlay")
        );
    }

    #[test]
    fn pending_subtitle_is_replaced_by_latest_frame() {
        let mut queue = PendingCommandQueue::new();
        assert!(queue.push(OverlayCommand::ShowText {
            frame_id: 1,
            text: "old".to_owned(),
            is_final: false,
        }));
        assert!(queue.push(OverlayCommand::ShowText {
            frame_id: 2,
            text: "latest".to_owned(),
            is_final: true,
        }));

        assert_eq!(
            queue.pop(),
            Some(OverlayCommand::ShowText {
                frame_id: 2,
                text: "latest".to_owned(),
                is_final: true,
            })
        );
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn controls_keep_order_while_stale_subtitle_is_removed() {
        let mut queue = PendingCommandQueue::new();
        assert!(queue.push(OverlayCommand::ShowText {
            frame_id: 1,
            text: "old".to_owned(),
            is_final: false,
        }));
        assert!(queue.push(OverlayCommand::Hide));
        assert!(queue.push(OverlayCommand::ApplyConfig {
            config: SubtitleConfig::default(),
        }));
        assert!(queue.push(OverlayCommand::ShowText {
            frame_id: 2,
            text: "latest".to_owned(),
            is_final: false,
        }));

        assert_eq!(queue.pop(), Some(OverlayCommand::Hide));
        assert!(matches!(
            queue.pop(),
            Some(OverlayCommand::ApplyConfig { .. })
        ));
        assert!(matches!(
            queue.pop(),
            Some(OverlayCommand::ShowText { frame_id: 2, .. })
        ));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn config_coalescing_does_not_discard_pending_final_subtitle() {
        let mut queue = PendingCommandQueue::new();
        assert!(queue.push(OverlayCommand::ShowText {
            frame_id: 7,
            text: "final sentence".to_owned(),
            is_final: true,
        }));
        assert!(queue.push(OverlayCommand::ApplyConfig {
            config: SubtitleConfig::default(),
        }));

        assert!(matches!(
            queue.pop(),
            Some(OverlayCommand::ShowText {
                frame_id: 7,
                is_final: true,
                ..
            })
        ));
        assert!(matches!(
            queue.pop(),
            Some(OverlayCommand::ApplyConfig { .. })
        ));
    }

    #[test]
    fn shutdown_discards_backlog_and_closes_queue() {
        let mut queue = PendingCommandQueue::new();
        assert!(queue.push(OverlayCommand::Ping { nonce: 1 }));
        assert!(queue.push(OverlayCommand::Shutdown));

        assert_eq!(queue.pop(), Some(OverlayCommand::Shutdown));
        assert_eq!(queue.pop(), None);
        assert!(!queue.push(OverlayCommand::Hide));
    }
}
