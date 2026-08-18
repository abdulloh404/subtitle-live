//! Event loop ของ overlay helper ซึ่งทำหน้าที่รับ IPC และวาดข้อความเท่านั้น

use std::{
    cell::RefCell,
    io::{self, BufReader},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::Duration,
};

use adw::prelude::*;
use gtk::{gio, glib};

use super::{OverlayCommand, OverlayEvent, OverlayPresenter, read_message, write_message};
use crate::config::SubtitleConfig;

/// เปิด GTK renderer และรับคำสั่งจาก stdin จน main process ขอ shutdown หรือปิด pipe
pub fn run_overlay_helper() -> io::Result<()> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let _event_writer = spawn_event_writer(event_receiver)?;
    let _command_reader = spawn_command_reader(command_sender, event_sender.clone())?;

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
        let presenter = OverlayPresenter::new(application, &SubtitleConfig::default());
        install_command_poll(
            application,
            presenter,
            command_receiver,
            event_sender.clone(),
        );
        let _ = event_sender.send(OverlayEvent::Ready);
    });

    application.run();
    Ok(())
}

/// อ่านคำสั่งใน worker เพื่อไม่ให้ blocking I/O หยุด GTK main loop
fn spawn_command_reader(
    command_sender: Sender<OverlayCommand>,
    event_sender: Sender<OverlayEvent>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("overlay-ipc-reader".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            loop {
                match read_message(&mut reader) {
                    Ok(Some(command)) => {
                        if command_sender.send(command).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = command_sender.send(OverlayCommand::Shutdown);
                        break;
                    }
                    Err(error) => {
                        let _ = event_sender.send(OverlayEvent::Error {
                            message: error.to_string(),
                        });
                        let _ = command_sender.send(OverlayCommand::Shutdown);
                        break;
                    }
                }
            }
        })
}

/// เขียน event ใน worker เพื่อไม่ให้ stdout backpressure หยุดการวาด subtitle
fn spawn_event_writer(
    event_receiver: Receiver<OverlayEvent>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("overlay-ipc-writer".to_owned())
        .spawn(move || {
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            while let Ok(event) = event_receiver.recv() {
                if let Err(error) = write_message(&mut writer, &event) {
                    eprintln!("ไม่สามารถส่ง overlay event กลับ main process ได้: {error}");
                    break;
                }
            }
        })
}

/// Poll command queue บน GTK thread และแตะ widget เฉพาะใน callback นี้
fn install_command_poll(
    application: &adw::Application,
    presenter: OverlayPresenter,
    command_receiver: Receiver<OverlayCommand>,
    event_sender: Sender<OverlayEvent>,
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
                            let _ = event_sender.send(OverlayEvent::Rendered {
                                frame_id,
                                rendered_at_micros: glib::monotonic_time(),
                            });
                        });
                    }
                }
                Ok(OverlayCommand::Hide) => presenter.hide(),
                Ok(OverlayCommand::ApplyConfig { config }) => presenter.apply_config(&config),
                Ok(OverlayCommand::Ping { nonce }) => {
                    let _ = event_sender.send(OverlayEvent::Pong { nonce });
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
