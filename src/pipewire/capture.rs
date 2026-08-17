use std::{mem::size_of, time::Instant};

use libspa_sys as spa_sys;
use pipewire as pw;
use pw::{prelude::*, spa};

use crate::audio::{LatestQueue, SAMPLE_RATE_HZ, SourceAudioChunk};

use super::{PipeWireEvent, StreamInfo};

struct CaptureUserData {
    audio: LatestQueue<SourceAudioChunk>,
    source_id: u32,
}

pub(super) struct CaptureSession {
    stream: pw::stream::Stream<CaptureUserData>,
}

impl CaptureSession {
    pub(super) fn disconnect(&self) {
        let _ = self.stream.disconnect();
    }
}

pub(super) fn create_capture(
    mainloop: &pw::MainLoop,
    info: &StreamInfo,
    audio: LatestQueue<SourceAudioChunk>,
    events: std::sync::mpsc::Sender<PipeWireEvent>,
) -> Result<CaptureSession, String> {
    let target = info
        .object_serial
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| info.node_name.as_deref().filter(|value| !value.is_empty()))
        .ok_or_else(|| {
            format!(
                "stream {} has no targetable PipeWire identity",
                info.runtime_id
            )
        })?;

    let stream = pw::stream::Stream::with_user_data(
        mainloop,
        "subtitle-live-selected-capture",
        pw::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Speech",
            "target.object" => target,
            "node.dont-reconnect" => "true",
        },
        CaptureUserData {
            audio,
            source_id: info.runtime_id,
        },
    )
    .state_changed({
        let state_events = events.clone();
        let source_id = info.runtime_id;
        move |_, state| match state {
            pw::stream::StreamState::Streaming => {
                let _ = state_events.send(PipeWireEvent::CaptureStarted(source_id));
            }
            pw::stream::StreamState::Error(message) => {
                let _ = state_events.send(PipeWireEvent::CaptureError {
                    runtime_id: source_id,
                    message: format!(
                        "PipeWire capture failed for stream {source_id}: {message}"
                    ),
                });
            }
            _ => {}
        }
    })
    .process(|stream, user_data| {
        let Some(mut buffer) = stream.dequeue_buffer() else {
            return;
        };
        let Some(data) = buffer.datas_mut().first_mut() else {
            return;
        };

        let offset = data.chunk().offset() as usize;
        let size = data.chunk().size() as usize;
        let Some(bytes) = data.data() else {
            return;
        };
        let end = offset.saturating_add(size).min(bytes.len());
        if offset >= end {
            return;
        }

        let samples: Vec<_> = bytes[offset..end]
            .chunks_exact(size_of::<f32>())
            .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
            .collect();
        let captured_end = Instant::now();
        let sample_duration =
            std::time::Duration::from_secs_f64(samples.len() as f64 / SAMPLE_RATE_HZ as f64);
        user_data.audio.push_latest(SourceAudioChunk {
            source_id: user_data.source_id,
            samples,
            captured_at: captured_end.checked_sub(sample_duration).unwrap_or(captured_end),
        });
    })
    .create()
    .map_err(|error| {
        format!(
            "failed to create capture for stream {}: {error}",
            info.runtime_id
        )
    })?;

    let format = spa::pod::Value::Object(spa::pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties: vec![
            spa::pod::Property {
                key: spa_sys::SPA_FORMAT_mediaType,
                flags: spa::pod::PropertyFlags::empty(),
                value: spa::pod::Value::Id(spa::utils::Id(spa_sys::SPA_MEDIA_TYPE_audio)),
            },
            spa::pod::Property {
                key: spa_sys::SPA_FORMAT_mediaSubtype,
                flags: spa::pod::PropertyFlags::empty(),
                value: spa::pod::Value::Id(spa::utils::Id(spa_sys::SPA_MEDIA_SUBTYPE_raw)),
            },
            spa::pod::Property {
                key: spa_sys::SPA_FORMAT_AUDIO_format,
                flags: spa::pod::PropertyFlags::empty(),
                value: spa::pod::Value::Id(spa::utils::Id(spa_sys::SPA_AUDIO_FORMAT_F32_LE)),
            },
            spa::pod::Property {
                key: spa_sys::SPA_FORMAT_AUDIO_rate,
                flags: spa::pod::PropertyFlags::empty(),
                value: spa::pod::Value::Int(SAMPLE_RATE_HZ as i32),
            },
            spa::pod::Property {
                key: spa_sys::SPA_FORMAT_AUDIO_channels,
                flags: spa::pod::PropertyFlags::empty(),
                value: spa::pod::Value::Int(1),
            },
        ],
    });
    let values = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &format,
    )
    .map_err(|error| format!("failed to serialize capture format: {error}"))?
    .0
    .into_inner();
    let mut params = [values.as_ptr().cast()];

    stream
        .connect(
            spa::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| {
            format!(
                "failed to connect capture for stream {}: {error}",
                info.runtime_id
            )
        })?;

    Ok(CaptureSession { stream })
}
