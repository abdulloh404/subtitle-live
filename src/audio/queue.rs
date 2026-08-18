//! คิวขนาดจำกัดที่เน้น latency ของเสียงสดมากกว่าการรักษาข้อมูลครบทุกก้อน
//!
//! ฝั่ง producer จะทิ้งข้อมูลเก่า หรือทิ้งข้อมูลใหม่เมื่อ mutex ไม่ว่าง เพื่อไม่ให้
//! ต้องรอ STT ที่ช้ากว่าและเพิ่ม latency ของระบบทั้งเส้นทาง

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// คิวขนาดจำกัดที่ clone ได้และรักษาข้อมูลล่าสุดไว้เมื่อระบบรับภาระไม่ทัน
pub struct LatestQueue<T> {
    inner: Arc<QueueInner<T>>,
}

struct QueueInner<T> {
    /// จำนวน item สูงสุดที่เก็บไว้พร้อมกัน
    capacity: usize,
    /// จำนวน item สะสมที่ถูกทิ้งจากทุก clone ของคิว
    dropped: AtomicU64,
    /// ข้อมูลที่ใช้ mutex เดียวร่วมกันระหว่าง producer และ consumer
    values: Mutex<VecDeque<T>>,
}

impl<T> LatestQueue<T> {
    /// สร้างคิวโดยบังคับให้ความจุมากกว่าศูนย์
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

    /// พยายามเพิ่มข้อมูลโดยไม่รอ mutex เพื่อให้เหมาะกับงาน real-time
    ///
    /// หาก consumer กำลังถือ mutex จะทิ้งข้อมูลใหม่นี้ และหากคิวเต็มจะทิ้ง
    /// ข้อมูลเก่าที่สุดแทน
    pub fn push_latest(&self, value: T) {
        let Ok(mut values) = self.inner.values.try_lock() else {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };

        self.push_locked(&mut values, value);
    }

    /// เพิ่มข้อมูลแบบรอ mutex สำหรับ producer ที่ไม่ใช่ real-time และห้ามทำ event สูญหาย
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

    /// นำข้อมูลทั้งหมดที่มีอยู่ออกจากคิวโดยไม่รอข้อมูลใหม่
    pub fn drain(&self) -> Vec<T> {
        let Ok(mut values) = self.inner.values.try_lock() else {
            return Vec::new();
        };
        values.drain(..).collect()
    }

    /// พยายามล้างข้อมูลโดยไม่รอ mutex สำหรับเส้นทาง real-time ที่ห้ามบล็อก
    /// และยังรักษาตัวนับจำนวนข้อมูลที่เคยถูกทิ้งไว้
    pub fn clear(&self) {
        if let Ok(mut values) = self.inner.values.try_lock() {
            values.clear();
        }
    }

    /// ล้างข้อมูลโดยรอ mutex สำหรับผู้เรียกที่ไม่ใช่ real-time และต้องรับประกันว่าคิวว่าง
    pub fn clear_reliable(&self) {
        let mut values = match self.inner.values.lock() {
            Ok(values) => values,
            Err(poisoned) => poisoned.into_inner(),
        };
        values.clear();
    }

    /// คืนจำนวนข้อมูลสะสมที่สูญหายจาก mutex ไม่ว่างหรือคิวเต็ม
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// คืนความลึกปัจจุบันของคิว หรือศูนย์เมื่อ thread อื่นกำลังถือ mutex
    pub fn len(&self) -> usize {
        self.inner
            .values
            .try_lock()
            .map_or(0, |values| values.len())
    }

    /// ตรวจว่าคิวที่มองเห็นในขณะนี้ว่างหรือไม่
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

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc,
        thread,
        time::Duration,
    };

    use super::LatestQueue;

    #[test]
    fn clear_does_not_wait_when_queue_is_locked() {
        let queue = LatestQueue::new(2);
        queue.push_latest_reliable(1);
        let holder_queue = queue.clone();
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = thread::spawn(move || {
            let _values = holder_queue.inner.values.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        locked_rx.recv().unwrap();
        queue.clear();
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn clear_reliable_waits_for_lock_and_empties_queue() {
        let queue = LatestQueue::new(2);
        queue.push_latest_reliable(1);
        let holder_queue = queue.clone();
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = thread::spawn(move || {
            let _values = holder_queue.inner.values.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        locked_rx.recv().unwrap();
        let clearer_queue = queue.clone();
        let (cleared_tx, cleared_rx) = mpsc::channel();
        let clearer = thread::spawn(move || {
            clearer_queue.clear_reliable();
            cleared_tx.send(()).unwrap();
        });

        assert!(cleared_rx.recv_timeout(Duration::from_millis(20)).is_err());
        release_tx.send(()).unwrap();
        cleared_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        holder.join().unwrap();
        clearer.join().unwrap();

        assert!(queue.is_empty());
    }
}
