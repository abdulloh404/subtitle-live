//! Event loop ของ overlay helper ซึ่งทำหน้าที่รับ IPC และวาดข้อความเท่านั้น

use std::{
    cell::RefCell,
    io::{self, Cursor},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::Duration,
};

use adw::prelude::*;
use gtk::gio::prelude::{FileExt, InputStreamExt};
use gtk::{gio, glib};

use super::{
    OverlayCommand, OverlayEvent, OverlayPresenter, read_message,
    protocol::MAX_MESSAGE_BYTES, write_message,
};
use crate::config::SubtitleConfig;

/// เปิด GTK renderer และรับคำสั่งจาก stdin จน main process ขอ shutdown หรือปิด pipe
pub fn run_overlay_helper() -> io::Result<()> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let event_writer = spawn_event_writer(event_receiver)?;
    let stdin = match gio::File::for_path("/dev/stdin").read(None::<&gio::Cancellable>) {
        Ok(stdin) => stdin,
        Err(error) => {
            drop(event_sender);
            let _ = join_helper_worker(event_writer, "overlay IPC writer");
            return Err(io::Error::other(format!(
                "ไม่สามารถเปิด stdin ของ overlay helper ได้: {error}"
            )));
        }
    };
    let input_cancellable = gio::Cancellable::new();
    let cleanup_cancellable = input_cancellable.clone();
    let cleanup_event_sender = event_sender.clone();

    let application = adw::Application::builder()
        .application_id("io.github.subtitle_live.overlay")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let command_receiver = Rc::new(RefCell::new(Some(command_receiver)));
    let receiver_for_activate = Rc::clone(&command_receiver);
    application.connect_activate(move |application| {
        let Some(command_receiver) = receiver_for_activate.borrow_mut().take() else {
            return;
        };
        schedule_command_read(
            stdin.clone(),
            input_cancellable.clone(),
            Rc::new(RefCell::new(Vec::new())),
            command_sender.clone(),
            event_sender.clone(),
        );
        let presenter = OverlayPresenter::new(application, &SubtitleConfig::default());
        install_command_poll(
            application,
            presenter,
            command_receiver,
            event_sender.clone(),
        );
        let _ = event_sender.send(WriterMessage::Event(OverlayEvent::Ready));
    });

    application.run();
    // ยกเลิก async read ก่อนปล่อย GTK จึงไม่มี reader thread ค้างรอ stdin
    cleanup_cancellable.cancel();
    drop(application);
    let _ = cleanup_event_sender.send(WriterMessage::Shutdown);
    join_helper_worker(event_writer, "overlay IPC writer")
}

/// ข้อความภายในสำหรับแยก event จริงออกจากคำสั่งปิด writer
enum WriterMessage {
    /// event ที่ต้อง serialize กลับ main process
    Event(OverlayEvent),
    /// จบ writer แม้ reader ยังถือ channel clone อยู่
    Shutdown,
}

/// ขนาด read แต่ละรอบเล็กกว่าขอบเขต protocol เพื่อจำกัด allocation ก่อนพบ newline
const INPUT_CHUNK_BYTES: usize = 8 * 1024;

/// อ่าน stdin เป็นก้อนบน GIO main context แล้วประกอบ frame ตาม LF ของ protocol เดิม
fn schedule_command_read(
    input: gio::FileInputStream,
    cancellable: gio::Cancellable,
    pending: Rc<RefCell<Vec<u8>>>,
    command_sender: Sender<OverlayCommand>,
    event_sender: Sender<WriterMessage>,
) {
    let next_input = input.clone();
    let next_cancellable = cancellable.clone();
    let next_pending = Rc::clone(&pending);
    input.read_bytes_async(
        INPUT_CHUNK_BYTES,
        glib::Priority::DEFAULT,
        Some(&cancellable),
        move |result| match result {
            Ok(bytes) if bytes.is_empty() => {
                let mut pending = pending.borrow_mut();
                if pending.is_empty() {
                    let _ = command_sender.send(OverlayCommand::Shutdown);
                    return;
                }
                // Decoder กลางต้องเห็น EOF ที่ไม่มี LF เพื่อคืน MessageTooLarge เหมือนเดิม
                let mut reader = Cursor::new(std::mem::take(&mut *pending));
                report_command_result(
                    read_message::<_, OverlayCommand>(&mut reader),
                    &command_sender,
                    &event_sender,
                );
            }
            Ok(bytes) => {
                let continue_reading = process_command_bytes(
                    &mut pending.borrow_mut(),
                    bytes.as_ref(),
                    &command_sender,
                    &event_sender,
                );
                if continue_reading {
                    schedule_command_read(
                        next_input,
                        next_cancellable,
                        next_pending,
                        command_sender,
                        event_sender,
                    );
                }
            }
            Err(error) if error.matches::<gio::IOErrorEnum>(gio::IOErrorEnum::Cancelled) => {}
            Err(error) => {
                let _ = event_sender.send(WriterMessage::Event(OverlayEvent::Error {
                    message: error.to_string(),
                }));
                let _ = command_sender.send(OverlayCommand::Shutdown);
            }
        },
    );
}

/// แยกทุก frame ที่ครบ LF และเก็บท้าย frame ที่ยังมาไม่ครบไว้สำหรับ read รอบถัดไป
fn process_command_bytes(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    command_sender: &Sender<OverlayCommand>,
    event_sender: &Sender<WriterMessage>,
) -> bool {
    pending.extend_from_slice(bytes);
    while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
        let encoded: Vec<u8> = pending.drain(..=newline).collect();
        let mut reader = Cursor::new(encoded);
        match read_message::<_, OverlayCommand>(&mut reader) {
            Ok(Some(command)) => {
                let shutdown = matches!(command, OverlayCommand::Shutdown);
                if command_sender.send(command).is_err() || shutdown {
                    return false;
                }
            }
            result => {
                report_command_result(result, command_sender, event_sender);
                return false;
            }
        }
    }
    if pending.len() > MAX_MESSAGE_BYTES {
        let mut reader = Cursor::new(std::mem::take(pending));
        report_command_result(
            read_message::<_, OverlayCommand>(&mut reader),
            command_sender,
            event_sender,
        );
        return false;
    }
    true
}

/// ส่ง protocol error แล้วขอให้ GTK ออกผ่าน command queue เดียวกับ Shutdown ปกติ
fn report_command_result(
    result: Result<Option<OverlayCommand>, super::OverlayProtocolError>,
    command_sender: &Sender<OverlayCommand>,
    event_sender: &Sender<WriterMessage>,
) {
    if let Err(error) = result {
        let _ = event_sender.send(WriterMessage::Event(OverlayEvent::Error {
            message: error.to_string(),
        }));
    }
    let _ = command_sender.send(OverlayCommand::Shutdown);
}

/// แปลง panic ของ worker เป็น I/O error แทนการปล่อย thread แบบ detached
fn join_helper_worker(worker: thread::JoinHandle<()>, name: &str) -> io::Result<()> {
    worker
        .join()
        .map_err(|_| io::Error::other(format!("{name} หยุดทำงานด้วย panic")))
}

/// เขียน event ใน worker เพื่อไม่ให้ stdout backpressure หยุดการวาด subtitle
fn spawn_event_writer(
    event_receiver: Receiver<WriterMessage>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("overlay-ipc-writer".to_owned())
        .spawn(move || {
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            while let Ok(message) = event_receiver.recv() {
                match message {
                    WriterMessage::Event(event) => {
                        if let Err(error) = write_message(&mut writer, &event) {
                            eprintln!("ไม่สามารถส่ง overlay event กลับ main process ได้: {error}");
                            break;
                        }
                    }
                    WriterMessage::Shutdown => break,
                }
            }
        })
}

/// Poll command queue บน GTK thread และแตะ widget เฉพาะใน callback นี้
fn install_command_poll(
    application: &adw::Application,
    presenter: OverlayPresenter,
    command_receiver: Receiver<OverlayCommand>,
    event_sender: Sender<WriterMessage>,
) {
    let application = application.clone();
    glib::timeout_add_local(Duration::from_millis(10), move || {
        loop {
            match command_receiver.try_recv() {
                Ok(OverlayCommand::ShowText {
                    frame_id,
                    text,
                    is_final,
                }) => {
                    if presenter.show_text(&text, is_final) {
                        let event_sender = event_sender.clone();
                        presenter.after_next_paint(move || {
                            let _ = event_sender.send(WriterMessage::Event(
                                OverlayEvent::Rendered {
                                    frame_id,
                                    rendered_at_micros: glib::monotonic_time(),
                                },
                            ));
                        });
                    }
                }
                Ok(OverlayCommand::Hide) => presenter.hide(),
                Ok(OverlayCommand::ApplyConfig { config }) => presenter.apply_config(&config),
                Ok(OverlayCommand::Ping { nonce }) => {
                    let _ = event_sender.send(WriterMessage::Event(OverlayEvent::Pong { nonce }));
                }
                Ok(OverlayCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    presenter.hide();
                    application.quit();
                    return glib::ControlFlow::Break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        glib::ControlFlow::Continue
    });
}
