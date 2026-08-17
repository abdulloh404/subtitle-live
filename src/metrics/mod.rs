use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

const SAMPLE_CAPACITY: usize = 256;

#[derive(Clone, Default)]
pub struct LatencyTracker {
    inner: Arc<Mutex<LatencySamples>>,
}

#[derive(Default)]
struct LatencySamples {
    inference: VecDeque<Duration>,
    total: VecDeque<Duration>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MetricsSnapshot {
    pub inference_latest: Option<Duration>,
    pub inference_p50: Option<Duration>,
    pub inference_p95: Option<Duration>,
    pub total_latest: Option<Duration>,
    pub total_p50: Option<Duration>,
    pub total_p95: Option<Duration>,
    pub source_queue_dropped: u64,
    pub mixed_queue_dropped: u64,
}

impl LatencyTracker {
    pub fn record(&self, inference: Duration, total: Duration) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        push_bounded(&mut inner.inference, inference);
        push_bounded(&mut inner.total, total);
    }

    pub fn snapshot(&self, source_dropped: u64, mixed_dropped: u64) -> MetricsSnapshot {
        let Ok(inner) = self.inner.try_lock() else {
            return MetricsSnapshot {
                source_queue_dropped: source_dropped,
                mixed_queue_dropped: mixed_dropped,
                ..MetricsSnapshot::default()
            };
        };

        MetricsSnapshot {
            inference_latest: inner.inference.back().copied(),
            inference_p50: percentile(&inner.inference, 50),
            inference_p95: percentile(&inner.inference, 95),
            total_latest: inner.total.back().copied(),
            total_p50: percentile(&inner.total, 50),
            total_p95: percentile(&inner.total, 95),
            source_queue_dropped: source_dropped,
            mixed_queue_dropped: mixed_dropped,
        }
    }
}

fn push_bounded(values: &mut VecDeque<Duration>, value: Duration) {
    if values.len() == SAMPLE_CAPACITY {
        values.pop_front();
    }
    values.push_back(value);
}

fn percentile(values: &VecDeque<Duration>, percentile: usize) -> Option<Duration> {
    if values.is_empty() {
        return None;
    }
    let mut sorted: Vec<_> = values.iter().copied().collect();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted.get(index).copied()
}
