//! ตัวเขียน transcript debug แบบ JSONL บนเธรดแยกจาก GTK และ Whisper

use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

use serde::Serialize;

use crate::{audio::LatestQueue, stt::SttDebugRecord};

// จำกัด raw transcript ที่รอเขียน เพื่อไม่ให้หน่วยความจำโตเมื่อดิสก์ทำงานช้า
const RECORD_QUEUE_CAPACITY: usize = 512;
const STATUS_QUEUE_CAPACITY: usize = 8;
const WRITER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// สถานะล่าสุดของการบันทึกไฟล์ debug
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DebugLogStatus {
    /// ปิดการเขียนไฟล์อยู่
    Disabled { path: PathBuf },
    /// เปิดไฟล์และพร้อมรับข้อมูลแล้ว
    Writing { path: PathBuf },
    /// เปิดหรือเขียนไฟล์ไม่สำเร็จ
    Error { path: PathBuf, message: String },
}

impl DebugLogStatus {
    /// คืน path เป้าหมายร่วมของทุกสถานะ
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::Disabled { path } | Self::Writing { path } | Self::Error { path, .. } => path,
        }
    }
}

/// handle ที่ runtime ใช้ควบคุม writer โดยไม่ทำ filesystem I/O เอง
pub struct DebugLogWriter {
    commands: Sender<WriterCommand>,
    records: LatestQueue<SttDebugRecord>,
    statuses: LatestQueue<DebugLogStatus>,
    accepting_records: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl DebugLogWriter {
    /// สร้าง worker โดยยังไม่สร้าง directory หรือไฟล์จนกว่าจะเปิด logging
    pub fn spawn(path: PathBuf) -> Self {
        let (commands, command_receiver) = mpsc::channel();
        let records = LatestQueue::new(RECORD_QUEUE_CAPACITY);
        let worker_records = records.clone();
        let statuses = LatestQueue::new(STATUS_QUEUE_CAPACITY);
        statuses.push_latest_reliable(DebugLogStatus::Disabled { path: path.clone() });
        let worker_statuses = statuses.clone();
        let accepting_records = Arc::new(AtomicBool::new(false));
        let worker_accepting_records = Arc::clone(&accepting_records);
        let worker = thread::Builder::new()
            .name("transcript-debug-log".to_owned())
            .spawn(move || {
                run_writer(
                    path,
                    command_receiver,
                    worker_records,
                    worker_statuses,
                    worker_accepting_records,
                )
            })
            .expect("failed to spawn transcript debug writer");

        Self {
            commands,
            records,
            statuses,
            accepting_records,
            worker: Some(worker),
        }
    }

    /// เปิดหรือปิดไฟล์แบบ asynchronous โดยการปิดจะล้าง raw text ที่ยังรอเขียน
    pub fn set_enabled(&self, enabled: bool) {
        self.accepting_records.store(enabled, Ordering::Release);
        if !enabled {
            self.records.clear_reliable();
        }
        let _ = self.commands.send(WriterCommand::SetEnabled(enabled));
    }

    /// ส่งข้อมูลหนึ่งชุดเข้าคิว bounded โดยไม่บล็อกผู้เรียก
    pub fn write_records(&self, records: impl IntoIterator<Item = SttDebugRecord>) {
        if !self.accepting_records.load(Ordering::Acquire) {
            return;
        }
        for record in records {
            self.records.push_latest(record);
        }
    }

    /// ดึงการเปลี่ยนสถานะทั้งหมดเพื่อให้ runtime ส่งสถานะล่าสุดไปยัง UI
    pub fn drain_statuses(&self) -> Vec<DebugLogStatus> {
        self.statuses.drain()
    }

    /// ปิด worker และรอให้ข้อมูลที่รับไว้ก่อนคำสั่ง shutdown ถูกเขียนให้เสร็จ
    pub fn finish(&mut self) -> Result<(), String> {
        let _ = self.commands.send(WriterCommand::Shutdown);
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| "Transcript debug writer panicked while shutting down".to_owned())
    }
}

impl Drop for DebugLogWriter {
    fn drop(&mut self) {
        let _ = self.commands.send(WriterCommand::Shutdown);
    }
}

enum WriterCommand {
    SetEnabled(bool),
    Shutdown,
}

fn run_writer(
    path: PathBuf,
    commands: Receiver<WriterCommand>,
    records: LatestQueue<SttDebugRecord>,
    statuses: LatestQueue<DebugLogStatus>,
    accepting_records: Arc<AtomicBool>,
) {
    let mut output: Option<BufWriter<File>> = None;
    loop {
        match commands.recv_timeout(WRITER_POLL_INTERVAL) {
            Ok(WriterCommand::SetEnabled(true)) => {
                if output.is_none() {
                    match open_output(&path) {
                        Ok(file) => {
                            output = Some(BufWriter::new(file));
                            statuses.push_latest_reliable(DebugLogStatus::Writing {
                                path: path.clone(),
                            });
                        }
                        Err(message) => {
                            accepting_records.store(false, Ordering::Release);
                            records.clear_reliable();
                            statuses.push_latest_reliable(DebugLogStatus::Error {
                                path: path.clone(),
                                message,
                            });
                        }
                    }
                }
            }
            Ok(WriterCommand::SetEnabled(false)) => {
                accepting_records.store(false, Ordering::Release);
                records.clear_reliable();
                output = None;
                statuses.push_latest_reliable(DebugLogStatus::Disabled { path: path.clone() });
            }
            Ok(WriterCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                accepting_records.store(false, Ordering::Release);
                if let Some(writer) = output.as_mut() {
                    let _ = write_pending(writer, &records);
                    let _ = writer.flush();
                }
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Some(writer) = output.as_mut()
            && let Err(message) = write_pending(writer, &records)
        {
            accepting_records.store(false, Ordering::Release);
            output = None;
            records.clear_reliable();
            statuses.push_latest_reliable(DebugLogStatus::Error {
                path: path.clone(),
                message,
            });
        }
    }
}

/// สร้าง directory เฉพาะตอนเปิด logging แล้วเปิดไฟล์เดิมแบบ append
fn open_output(path: &PathBuf) -> Result<File, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Debug log path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("สร้าง directory debug log ไม่สำเร็จ: {error}"))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("เปิดไฟล์ debug log ไม่สำเร็จ: {error}"))
}

fn write_pending(
    writer: &mut BufWriter<File>,
    records: &LatestQueue<SttDebugRecord>,
) -> Result<(), String> {
    for record in records.drain() {
        let line = JsonRecord::from(&record);
        serde_json::to_writer(&mut *writer, &line)
            .map_err(|error| format!("เขียน JSON debug log ไม่สำเร็จ: {error}"))?;
        writer
            .write_all(b"\n")
            .map_err(|error| format!("เขียนบรรทัด debug log ไม่สำเร็จ: {error}"))?;
    }
    writer
        .flush()
        .map_err(|error| format!("flush debug log ไม่สำเร็จ: {error}"))
}

#[derive(Serialize)]
struct JsonRecord<'a> {
    timestamp_unix_ms: u64,
    audio_generation: u64,
    segment_id: u64,
    kind: &'static str,
    raw_text: &'a str,
    processed_text: &'a str,
    emitted_text: &'a str,
    whisper_model_latency_ms: f64,
    gpu_backend_latency_ms: f64,
    gpu_backend_requested: bool,
    gpu_device: i32,
    step_ms: u32,
    window_ms: u32,
}

impl<'a> From<&'a SttDebugRecord> for JsonRecord<'a> {
    fn from(record: &'a SttDebugRecord) -> Self {
        Self {
            timestamp_unix_ms: record.timestamp_unix_ms,
            audio_generation: record.audio_generation,
            segment_id: record.segment_id,
            kind: record.kind.as_str(),
            raw_text: &record.raw_text,
            processed_text: &record.processed_text,
            emitted_text: &record.emitted_text,
            whisper_model_latency_ms: record.whisper_model_duration.as_secs_f64() * 1_000.0,
            gpu_backend_latency_ms: record.gpu_backend_duration.as_secs_f64() * 1_000.0,
            gpu_backend_requested: record.gpu_backend_requested,
            gpu_device: record.gpu_device,
            step_ms: record.step_ms,
            window_ms: record.window_ms,
        }
    }
}
