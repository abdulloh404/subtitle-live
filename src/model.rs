//! Catalog และ download worker สำหรับ Whisper model ที่โปรเจกต์รองรับ
//!
//! ไฟล์นี้เก็บ metadata ที่ใช้ร่วมกันระหว่าง config, runtime และ UI พร้อมดาวน์โหลด
//! และตรวจ checksum บน worker thread โดยไม่ block GTK main thread

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

const DOWNLOAD_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Metadata คงที่ของ Whisper model หนึ่งรายการ
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhisperModel {
    /// identifier ที่บันทึกใน config
    pub id: &'static str,
    /// ชื่อไฟล์ ggml ที่เก็บใน model directory
    pub file_name: &'static str,
    /// ขนาดไฟล์ที่ใช้ตรวจความครบถ้วนก่อนคำนวณ checksum
    pub size_bytes: u64,
    /// ขนาดแบบอ่านง่ายสำหรับแสดงใน UI
    pub size_label: &'static str,
    /// URL ที่ pin revision ของ repository เพื่อให้ checksum คงที่
    pub download_url: &'static str,
    /// SHA-256 ของไฟล์ model จาก repository ต้นทาง
    pub sha256: &'static str,
}

/// รายการ model ที่ใช้ใน milestone benchmark ของโปรเจกต์
pub const WHISPER_MODELS: [WhisperModel; 3] = [
    WhisperModel {
        id: "small.en",
        file_name: "ggml-small.en.bin",
        size_bytes: 487_614_201,
        size_label: "488 MB",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/80da2d8bfee42b0e836fc3a9890373e5defc00a6/ggml-small.en.bin",
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    },
    WhisperModel {
        id: "medium.en",
        file_name: "ggml-medium.en.bin",
        size_bytes: 1_533_774_781,
        size_label: "1.53 GB",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/80da2d8bfee42b0e836fc3a9890373e5defc00a6/ggml-medium.en.bin",
        sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356",
    },
    WhisperModel {
        id: "large-v3-turbo",
        file_name: "ggml-large-v3-turbo.bin",
        size_bytes: 1_624_555_275,
        size_label: "1.62 GB",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/6034871ec87c84e342efab769d4c5c06cd126db3/ggml-large-v3-turbo.bin",
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
];

/// ค้นหา model ด้วย identifier ที่บันทึกใน config
pub fn whisper_model(model_id: &str) -> Option<WhisperModel> {
    WHISPER_MODELS
        .iter()
        .copied()
        .find(|model| model.id == model_id)
}

/// สร้าง path ปลายทางของ model ภายใต้ directory มาตรฐาน
pub fn whisper_model_path(directory: &Path, model: WhisperModel) -> PathBuf {
    directory.join(model.file_name)
}

/// สถานะ model ที่ runtime ส่งให้หน้า Settings
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhisperModelStatus {
    /// identifier ของ model ที่เลือกใน config
    pub selected_model: String,
    /// path ที่ STT จะใช้โหลด model ที่เลือก
    pub selected_path: PathBuf,
    /// ระบุว่าไฟล์ model ที่เลือกมีอยู่จริงหรือไม่
    pub installed: bool,
    /// model ที่ download worker กำลังประมวลผล
    pub downloading_model: Option<String>,
    /// ข้อผิดพลาดล่าสุดของ download worker
    pub download_error: Option<String>,
}

/// Event จาก download worker ที่ runtime รับแบบไม่บล็อก
#[derive(Debug)]
pub enum ModelDownloadEvent {
    /// เริ่มดาวน์โหลด model แล้ว
    Started { model_id: &'static str },
    /// ดาวน์โหลด ตรวจ checksum และติดตั้ง model สำเร็จ
    Installed {
        model_id: &'static str,
        path: PathBuf,
    },
    /// ดาวน์โหลดหรือตรวจสอบ model ไม่สำเร็จ
    Failed {
        model_id: &'static str,
        error: String,
    },
}

enum ModelDownloadCommand {
    Download {
        model: WhisperModel,
        directory: PathBuf,
    },
    Shutdown,
}

enum DownloadOutcome {
    Installed(PathBuf),
    Failed(String),
    Shutdown,
}

/// Handle สำหรับส่งคำขอดาวน์โหลดและรับ event จาก worker thread
pub struct ModelDownloadService {
    commands: Sender<ModelDownloadCommand>,
    events: Receiver<ModelDownloadEvent>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ModelDownloadService {
    /// เริ่ม worker และคืน handle ทันที
    pub fn spawn() -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("model-download".to_owned())
            .spawn(move || run_download_worker(command_receiver, event_sender))
            .ok();
        Self {
            commands: command_sender,
            events: event_receiver,
            worker,
        }
    }

    /// ส่ง model ไปให้ worker ดาวน์โหลดโดยไม่ block ผู้เรียก
    pub fn download(&self, model: WhisperModel, directory: PathBuf) -> Result<(), String> {
        if self.worker.is_none() {
            return Err("Model download worker could not be started".to_owned());
        }
        self.commands
            .send(ModelDownloadCommand::Download { model, directory })
            .map_err(|_| "Model download worker is no longer running".to_owned())
    }

    /// นำ event ที่ค้างออกทั้งหมดโดยไม่รอ event ใหม่
    pub fn drain_events(&self) -> Vec<ModelDownloadEvent> {
        self.events.try_iter().collect()
    }

    /// ขอให้ worker ยกเลิก download ปัจจุบันและหยุดทำงาน
    pub fn request_shutdown(&self) {
        let _ = self.commands.send(ModelDownloadCommand::Shutdown);
    }

    /// รอ worker หลัง GTK event loop จบแล้ว
    pub fn finish(&mut self) -> Result<(), String> {
        self.request_shutdown();
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| "Model download worker panicked while shutting down".to_owned())
    }
}

impl Drop for ModelDownloadService {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

fn run_download_worker(
    commands: Receiver<ModelDownloadCommand>,
    events: Sender<ModelDownloadEvent>,
) {
    while let Ok(command) = commands.recv() {
        let ModelDownloadCommand::Download { model, directory } = command else {
            return;
        };
        let _ = events.send(ModelDownloadEvent::Started { model_id: model.id });
        match download_model(model, &directory, &commands) {
            DownloadOutcome::Installed(path) => {
                let _ = events.send(ModelDownloadEvent::Installed {
                    model_id: model.id,
                    path,
                });
            }
            DownloadOutcome::Failed(error) => {
                let _ = events.send(ModelDownloadEvent::Failed {
                    model_id: model.id,
                    error,
                });
            }
            DownloadOutcome::Shutdown => return,
        }
    }
}

fn download_model(
    model: WhisperModel,
    directory: &Path,
    commands: &Receiver<ModelDownloadCommand>,
) -> DownloadOutcome {
    if let Err(error) = fs::create_dir_all(directory) {
        return DownloadOutcome::Failed(format!(
            "Could not create model directory at {}: {error}",
            directory.display()
        ));
    }
    let destination = whisper_model_path(directory, model);
    if destination.is_file() {
        return DownloadOutcome::Installed(destination);
    }
    let temporary = destination.with_extension("bin.download");
    let mut child = match Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--retry-all-errors",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(&temporary)
        .arg(model.download_url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return DownloadOutcome::Failed(format!(
                "Could not start curl for {}: {error}",
                model.id
            ));
        }
    };
    let curl_status = match wait_for_download(&mut child, commands) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = fs::remove_file(&temporary);
            return DownloadOutcome::Shutdown;
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return DownloadOutcome::Failed(error);
        }
    };
    if !curl_status.success() {
        let mut stderr = String::new();
        if let Some(mut stream) = child.stderr.take() {
            let _ = stream.read_to_string(&mut stderr);
        }
        let _ = fs::remove_file(&temporary);
        let detail = stderr.trim();
        return DownloadOutcome::Failed(if detail.is_empty() {
            format!("Model download failed for {}", model.id)
        } else {
            format!("Model download failed for {}: {detail}", model.id)
        });
    }
    let actual_size = match fs::metadata(&temporary) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return DownloadOutcome::Failed(format!(
                "Could not inspect downloaded model {}: {error}",
                model.id
            ));
        }
    };
    if actual_size != model.size_bytes {
        let _ = fs::remove_file(&temporary);
        return DownloadOutcome::Failed(format!(
            "Downloaded model {} has unexpected size: expected {}, received {} bytes",
            model.id, model.size_bytes, actual_size
        ));
    }
    let checksum = match file_sha256(&temporary) {
        Ok(checksum) => checksum,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return DownloadOutcome::Failed(error);
        }
    };
    if !checksum.eq_ignore_ascii_case(model.sha256) {
        let _ = fs::remove_file(&temporary);
        return DownloadOutcome::Failed(format!(
            "Downloaded model {} failed SHA-256 verification",
            model.id
        ));
    }
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return DownloadOutcome::Failed(format!(
            "Could not install model at {}: {error}",
            destination.display()
        ));
    }
    DownloadOutcome::Installed(destination)
}

fn wait_for_download(
    child: &mut Child,
    commands: &Receiver<ModelDownloadCommand>,
) -> Result<Option<std::process::ExitStatus>, String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not read curl status: {error}"))?
        {
            return Ok(Some(status));
        }
        match commands.recv_timeout(DOWNLOAD_POLL_INTERVAL) {
            Ok(ModelDownloadCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            Ok(ModelDownloadCommand::Download { .. }) | Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let output = Command::new("sha256sum")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|error| format!("Could not start sha256sum: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not calculate SHA-256 for {}",
            path.display()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("sha256sum returned invalid UTF-8: {error}"))?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "sha256sum returned no checksum".to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{WHISPER_MODELS, whisper_model, whisper_model_path};

    #[test]
    fn benchmark_models_have_stable_catalog_entries() {
        assert_eq!(WHISPER_MODELS.len(), 3);
        assert_eq!(whisper_model("small.en"), Some(WHISPER_MODELS[0]));
        assert_eq!(whisper_model("medium.en"), Some(WHISPER_MODELS[1]));
        assert_eq!(whisper_model("large-v3-turbo"), Some(WHISPER_MODELS[2]));
    }

    #[test]
    fn model_path_uses_catalog_file_name() {
        assert_eq!(
            whisper_model_path(Path::new("/models"), WHISPER_MODELS[1]),
            Path::new("/models/ggml-medium.en.bin")
        );
    }
}
