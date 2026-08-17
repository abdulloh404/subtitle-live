//! worker สำหรับรวมและบันทึก snapshot การตั้งค่านอก GTK main thread

use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread,
};

use super::{AppConfig, save_config};

/// ช่องทางส่ง snapshot ไปยัง worker บันทึกการตั้งค่า
pub struct ConfigWriter {
    sender: Sender<WriteCommand>,
    worker: Option<thread::JoinHandle<()>>,
}

/// คำสั่งภายในที่มีเพียง worker thread เป็นผู้รับ
enum WriteCommand {
    /// บันทึก snapshot ล่าสุด โดย snapshot ที่ใหม่กว่าสามารถแทนค่าที่ค้างอยู่ได้
    Save(AppConfig),
    /// เขียนงานที่ค้างอยู่แล้วหยุด worker
    Shutdown,
}

impl ConfigWriter {
    /// เริ่ม worker ที่บันทึกไปยัง path เดียวตลอดอายุการทำงาน
    pub fn spawn(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("config-writer".to_owned())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        WriteCommand::Save(mut config) => {
                            let mut should_shutdown = false;
                            // รวม snapshot ที่รออยู่เพื่อไม่เขียนค่ารุ่นเก่าลงดิสก์โดยไม่จำเป็น
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
                                    "บันทึกการตั้งค่าล้มเหลว"
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

    /// ส่ง snapshot ไปบันทึกแบบไม่บล็อก thread ผู้เรียก
    pub fn save(&self, config: &AppConfig) {
        if self.sender.send(WriteCommand::Save(config.clone())).is_err() {
            tracing::error!("ไม่สามารถติดต่อ config writer ได้");
        }
    }

    /// ขอให้ worker หยุดหลังจัดการคำสั่งที่รับไว้แล้ว
    pub fn shutdown(&self) {
        let _ = self.sender.send(WriteCommand::Shutdown);
    }

    /// เขียนงานที่ค้างและ join worker หลัง GTK event loop หยุดแล้ว
    pub fn finish(&mut self) {
        self.shutdown();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::error!("config writer thread panic");
        }
    }
}

impl Drop for ConfigWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}
