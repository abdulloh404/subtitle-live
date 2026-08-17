use std::{mem::size_of, time::Instant};

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::pod::Pod;

use crate::audio::{LatestQueue, SAMPLE_RATE_HZ, SourceAudioChunk};

use super::{PipeWireEvent, StreamInfo};

struct CaptureUserData {
    audio: LatestQueue<SourceAudioChunk>,
    source_id: u32,
}

pub(super) struct CaptureSession {
    _listener: pw::stream::StreamListener<CaptureUserData>,
    stream: pw::stream::StreamRc,
}

impl CaptureSession {
    pub(super) fn disconnect(&self) {
        let _ = self.stream.disconnect();
    }
}

pub(super) fn create_capture(
    core: &pw::core::CoreRc,
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

    // Literal keys keep compatibility with the crate's baseline feature set.
    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "subtitle-live-selected-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Speech",
            "target.object" => target,
            "node.dont-reconnect" => "true",
        },
    )
    .map_err(|error| {
        format!(
            "failed to create capture for stream {}: {error}",
            info.runtime_id
        )
    })?;

    let state_events = events.clone();
    let source_id = info.runtime_id;
    let listener = stream
        .add_local_listener_with_user_data(CaptureUserData { audio, source_id })
        .state_changed(move |_, _, _, state| match state {
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
        .register()
        .map_err(|error| {
            format!(
                "failed to register capture listener for stream {}: {error}",
                info.runtime_id
            )
        })?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE_HZ);
    audio_info.set_channels(1);
    let values = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|error| format!("failed to serialize capture format: {error}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values)
        .ok_or_else(|| "failed to create PipeWire capture format".to_owned())?];

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| {
            format!(
                "failed to connect capture for stream {}: {error}",
                info.runtime_id
            )
        })?;

    Ok(CaptureSession {
        _listener: listener,
        stream,
    })
}
