// gateway/src/sensor_buffer.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::collections::VecDeque;
use std::time::Duration;

use sensor_sim::accelerometer::AccelReading;
use sensor_sim::force_sensor::ForceReading;
use sensor_sim::thermometer::ThermoReading;

/// Unified sensor reading enum, including the sensor ID.
#[derive(Debug, Clone)]
pub enum SensorReading {
    Accel(AccelReading, String),
    Force(ForceReading, String),
    Thermo(ThermoReading, String),
}

/// Thread-safe shared buffer (bounded blocking queue).
pub struct SharedBuffer {
    queue: Mutex<VecDeque<SensorReading>>,
    cond: Condvar,
    capacity: usize,
    fail_on_full: bool,
    pushed_total: AtomicU64,
    popped_total: AtomicU64,
}

impl SharedBuffer {
    #[allow(dead_code)]
    pub fn new(capacity: usize) -> Arc<Self> {
        Self::new_with_policy(capacity, false)
    }

    pub fn new_with_policy(capacity: usize, fail_on_full: bool) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            cond: Condvar::new(),
            capacity,
            fail_on_full,
            pushed_total: AtomicU64::new(0),
            popped_total: AtomicU64::new(0),
        })
    }

    /// Push a reading into the buffer. If the buffer is full, block until space is available.
    pub fn push(&self, reading: SensorReading) {
        let mut queue = self.queue.lock().unwrap();
        if self.fail_on_full && queue.len() >= self.capacity {
            panic!(
                "SharedBuffer overflow detected: len={} capacity={}. \
Set a larger capacity, reduce producer rate, or disable fail-fast mode.",
                queue.len(),
                self.capacity
            );
        }
        while queue.len() >= self.capacity {
            queue = self.cond.wait(queue).unwrap();
        }
        queue.push_back(reading);
        self.pushed_total.fetch_add(1, Ordering::Relaxed);
        self.cond.notify_one(); // Wake a potential consumer
    }

    /// Pop a reading from the buffer. If the buffer is empty, block until data arrives.
    #[allow(dead_code)]
    pub fn pop(&self) -> SensorReading {
        let mut queue = self.queue.lock().unwrap();
        while queue.is_empty() {
            queue = self.cond.wait(queue).unwrap();
        }
        let val = queue.pop_front().unwrap();
        self.cond.notify_one(); // Wake a potential producer
        val
    }

    /// Pop with timeout. Returns `None` on timeout.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<SensorReading> {
        let mut queue = self.queue.lock().unwrap();
        let result = self.cond
            .wait_timeout_while(queue, timeout, |q| q.is_empty())
            .unwrap();
        queue = result.0;
        if queue.is_empty() {
            None
        } else {
            let val = queue.pop_front().unwrap();
            self.popped_total.fetch_add(1, Ordering::Relaxed);
            Some(val)
        }
    }

    /// Current queue length.
    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    /// Buffer capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Total number of readings pushed/popped since start.
    pub fn totals(&self) -> (u64, u64) {
        (
            self.pushed_total.load(Ordering::Relaxed),
            self.popped_total.load(Ordering::Relaxed),
        )
    }
}