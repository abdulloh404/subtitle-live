use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct LatestQueue<T> {
    inner: Arc<QueueInner<T>>,
}

struct QueueInner<T> {
    capacity: usize,
    dropped: AtomicU64,
    values: Mutex<VecDeque<T>>,
}

impl<T> LatestQueue<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "queue capacity must be greater than zero");
        Self {
            inner: Arc::new(QueueInner {
                capacity,
                dropped: AtomicU64::new(0),
                values: Mutex::new(VecDeque::with_capacity(capacity)),
            }),
        }
    }

    pub fn push_latest(&self, value: T) {
        let Ok(mut values) = self.inner.values.try_lock() else {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };

        self.push_locked(&mut values, value);
    }

    /// Bounded push for non-real-time producers whose control events must not be lost.
    pub fn push_latest_reliable(&self, value: T) {
        let mut values = match self.inner.values.lock() {
            Ok(values) => values,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.push_locked(&mut values, value);
    }

    fn push_locked(&self, values: &mut VecDeque<T>, value: T) {
        if values.len() == self.inner.capacity {
            values.pop_front();
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
        }
        values.push_back(value);
    }

    pub fn drain(&self) -> Vec<T> {
        let Ok(mut values) = self.inner.values.try_lock() else {
            return Vec::new();
        };
        values.drain(..).collect()
    }

    pub fn clear(&self) {
        if let Ok(mut values) = self.inner.values.try_lock() {
            values.clear();
        }
    }

    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.inner
            .values
            .try_lock()
            .map_or(0, |values| values.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Clone for LatestQueue<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}
