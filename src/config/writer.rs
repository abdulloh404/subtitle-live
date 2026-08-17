use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread,
};

use super::{AppConfig, save_config};

pub struct ConfigWriter {
    sender: Sender<WriteCommand>,
    worker: Option<thread::JoinHandle<()>>,
}

enum WriteCommand {
    Save(AppConfig),
    Shutdown,
}

impl ConfigWriter {
    pub fn spawn(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("config-writer".to_owned())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        WriteCommand::Save(mut config) => {
                            let mut should_shutdown = false;
                            for pending in receiver.try_iter() {
                                match pending {
                                    WriteCommand::Save(newer) => config = newer,
                                    WriteCommand::Shutdown => should_shutdown = true,
                                }
                            }
                            if let Err(error) = save_config(&path, &config) {
                                tracing::error!(
                                    config_path = %path.display(),
                                    error = %error,
                                    "Failed to persist configuration"
                                );
                            }
                            if should_shutdown {
                                return;
                            }
                        }
                        WriteCommand::Shutdown => return,
                    }
                }
            })
            .expect("failed to spawn configuration writer");
        Self {
            sender,
            worker: Some(worker),
        }
    }

    pub fn save(&self, config: &AppConfig) {
        if self.sender.send(WriteCommand::Save(config.clone())).is_err() {
            tracing::error!("Configuration writer is unavailable");
        }
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(WriteCommand::Shutdown);
    }

    /// Flushes pending writes and joins the writer after the GTK event loop has stopped.
    pub fn finish(&mut self) {
        self.shutdown();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::error!("Configuration writer thread panicked");
        }
    }
}

impl Drop for ConfigWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}
