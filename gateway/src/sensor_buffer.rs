// gateway/src/sensor_buffer.rs

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sensor_sim::accelerometer::AccelReading;
use sensor_sim::force_sensor::ForceReading;
use sensor_sim::thermometer::ThermoReading;

/// Unified sensor reading enum, including the sensor ID.
#[derive(Debug, Clone)]
pub enum SensorReading {
    Accel(AccelReading, String, u64),
    Force(ForceReading, String, u64),
    Thermo(ThermoReading, String, u64),
}

impl SensorReading {
    pub fn sensor_id(&self) -> &str {
        match self {
            SensorReading::Accel(_, id, _) => id,
            SensorReading::Force(_, id, _) => id,
            SensorReading::Thermo(_, id, _) => id,
        }
    }

    pub fn timestamp_millis(&self) -> u64 {
        match self {
            SensorReading::Accel(_, _, ts) => *ts,
            SensorReading::Force(_, _, ts) => *ts,
            SensorReading::Thermo(_, _, ts) => *ts,
        }
    }
}

/// Thread-safe shared buffer (bounded blocking queue).
pub struct SharedBuffer {
    queue: Mutex<VecDeque<SensorReading>>,
    cond: Condvar,
    capacity: usize,
    fail_on_full: bool,
    pushed_total: AtomicU64,
    popped_total: AtomicU64,
    full_waits_total: AtomicU64,
}

impl SharedBuffer {
    pub fn new_with_policy(capacity: usize, fail_on_full: bool) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            cond: Condvar::new(),
            capacity,
            fail_on_full,
            pushed_total: AtomicU64::new(0),
            popped_total: AtomicU64::new(0),
            full_waits_total: AtomicU64::new(0),
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
            self.full_waits_total.fetch_add(1, Ordering::Relaxed);
            queue = self.cond.wait(queue).unwrap();
        }
        queue.push_back(reading);
        self.pushed_total.fetch_add(1, Ordering::Relaxed);
        self.cond.notify_one(); // Wake a potential consumer
    }

    /// Pop a reading from the buffer, blocking until data arrives.
    pub fn pop(&self) -> SensorReading {
        let mut queue = self.queue.lock().unwrap();
        queue = self
            .cond
            .wait_while(queue, |q| q.is_empty())
            .unwrap();

        let val = queue.pop_front().unwrap();
        self.popped_total.fetch_add(1, Ordering::Relaxed);
        // Wake a potential producer waiting for free capacity.
        self.cond.notify_one();
        val
    }

    /// Pop a reading from the buffer. If the buffer is empty, block until data arrives.
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
            // Wake a potential producer waiting for free capacity.
            self.cond.notify_one();
            Some(val)
        }
    }

    /// Try to pop a reading without blocking.
    pub fn try_pop(&self) -> Option<SensorReading> {
        let mut queue = self.queue.lock().unwrap();
        let res = queue.pop_front();
        if res.is_some() {
            self.popped_total.fetch_add(1, Ordering::Relaxed);
            self.cond.notify_one(); // Wake a potential producer.
        }
        res
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

    pub fn full_waits_total(&self) -> u64 {
        self.full_waits_total.load(Ordering::Relaxed)
    }
}

/// Required by the spec: buffer manager that registers sensors and exposes pop operations.
pub struct SensorBufferManager {
    shared: Arc<SharedBuffer>,
    running: Arc<AtomicBool>,
    reader_handles: Vec<thread::JoinHandle<()>>,
    last_rates_snapshot: Mutex<(u64, u64, u64)>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BufferUtilizationStats {
    pub len: usize,
    pub capacity: usize,
    pub utilization_ratio: f64,
    pub pushed_total: u64,
    pub popped_total: u64,
    pub pushed_per_sec: u64,
    pub popped_per_sec: u64,
    pub full_waits_total: u64,
}

impl SensorBufferManager {
    pub fn new(capacity: usize) -> Self {
        Self::new_with_policy(capacity, false)
    }

    pub fn new_with_policy(capacity: usize, fail_on_full: bool) -> Self {
        Self {
            shared: SharedBuffer::new_with_policy(capacity, fail_on_full),
            running: Arc::new(AtomicBool::new(true)),
            reader_handles: Vec::new(),
            // (last_pushed, last_popped, last_ts_ms)
            last_rates_snapshot: Mutex::new((0, 0, now_millis())),
        }
    }

    pub fn shared(&self) -> Arc<SharedBuffer> {
        self.shared.clone()
    }

    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Register a sensor (spawns a reader thread).
    ///
    /// `wrap` converts the sensor reading + id + timestamp into this project's `SensorReading`.
    pub fn register_sensor<S, Wrap>(&mut self, mut sensor: S, wrap: Wrap)
    where
        S: sensor_sim::traits::Sensor + Send + 'static,
        S::SensorReading: Send + 'static,
        Wrap: Fn(S::SensorReading, String, u64) -> SensorReading + Send + Sync + 'static,
    {
        // Start generating data (spawns sensor internal thread).
        sensor.start();

        let sensor_id = sensor.id();
        let running = self.running.clone();
        let shared = self.shared.clone();

        let reader_handle = thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                if let Some(reading) = sensor.read() {
                    let ts = now_millis();
                    shared.push(wrap(reading, sensor_id.clone(), ts));
                } else {
                    thread::sleep(Duration::from_micros(100));
                }
            }
            // Stop generating data before exiting the thread.
            sensor.stop();
        });

        self.reader_handles.push(reader_handle);
    }

    /// Pop reading for processing (blocking).
    pub fn pop(&self) -> SensorReading {
        self.shared.pop()
    }

    /// Pop with timeout. Returns `None` on timeout.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<SensorReading> {
        self.shared.pop_timeout(timeout)
    }

    /// Non-blocking pop.
    pub fn try_pop(&self) -> Option<SensorReading> {
        self.shared.try_pop()
    }

    /// Get buffer utilization statistics (including rates).
    pub fn utilization_stats(&self) -> BufferUtilizationStats {
        let len = self.shared.len();
        let capacity = self.shared.capacity();
        let utilization_ratio = if capacity == 0 {
            0.0
        } else {
            len as f64 / capacity as f64
        };

        let (pushed_total, popped_total) = self.shared.totals();
        let full_waits_total = self.shared.full_waits_total();

        let now = now_millis();
        let mut snap = self.last_rates_snapshot.lock().unwrap();
        let (last_pushed, last_popped, last_ts) = *snap;
        let dt_ms = now.saturating_sub(last_ts);

        // Rate computation: per second (integer).
        let pushed_per_sec = if dt_ms >= 1000 {
            (pushed_total.saturating_sub(last_pushed) * 1000) / dt_ms.max(1)
        } else {
            0
        };
        let popped_per_sec = if dt_ms >= 1000 {
            (popped_total.saturating_sub(last_popped) * 1000) / dt_ms.max(1)
        } else {
            0
        };

        if dt_ms >= 1000 {
            *snap = (pushed_total, popped_total, now);
        }

        BufferUtilizationStats {
            len,
            capacity,
            utilization_ratio,
            pushed_total,
            popped_total,
            pushed_per_sec,
            popped_per_sec,
            full_waits_total,
        }
    }

    /// Shutdown all reader threads cleanly.
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        while let Some(handle) = self.reader_handles.pop() {
            handle.join().unwrap();
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}