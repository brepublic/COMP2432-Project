// gateway/src/sensor_buffer.rs

use std::collections::VecDeque;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sensor_sim::accelerometer::AccelReading;
use sensor_sim::force_sensor::ForceReading;
use sensor_sim::thermometer::ThermoReading;
use common_models::{BufferTelemetrySnapshot, SensorBufferStatus};

/// Unified sensor reading enum, including the sensor ID.
#[derive(Debug, Clone)]
pub enum SensorReading {
    Accel(AccelReading, Arc<str>, u64),
    Force(ForceReading, Arc<str>, u64),
    Thermo(ThermoReading, Arc<str>, u64),
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
    not_empty: Condvar,
    not_full: Condvar,
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
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
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
            queue = self.not_full.wait(queue).unwrap();
        }
        queue.push_back(reading);
        self.pushed_total.fetch_add(1, Ordering::Relaxed);
        self.not_empty.notify_one(); // Wake a potential consumer
    }

    /// Pop a reading from the buffer. If the buffer is empty, block until data arrives.
    /// Pop with timeout. Returns `None` on timeout.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<SensorReading> {
        let mut queue = self.queue.lock().unwrap();
        let result = self.not_empty
            .wait_timeout_while(queue, timeout, |q| q.is_empty())
            .unwrap();
        queue = result.0;
        if queue.is_empty() {
            None
        } else {
            let val = queue.pop_front().unwrap();
            self.popped_total.fetch_add(1, Ordering::Relaxed);
            // Wake a potential producer waiting for free capacity.
            self.not_full.notify_one();
            Some(val)
        }
    }

    /// Wake all producers/consumers blocked on the buffer.
    pub fn wake_all(&self) {
        self.not_empty.notify_all();
        self.not_full.notify_all();
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
    per_sensor: Arc<Mutex<HashMap<String, SensorQueueSnapshot>>>,
}

#[derive(Debug, Clone)]
struct SensorQueueSnapshot {
    current_len: usize,
    peak_len: usize,
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
    pub fn new_with_policy(capacity: usize, fail_on_full: bool) -> Self {
        Self {
            shared: SharedBuffer::new_with_policy(capacity, fail_on_full),
            running: Arc::new(AtomicBool::new(true)),
            reader_handles: Vec::new(),
            // (last_pushed, last_popped, last_ts_ms)
            last_rates_snapshot: Mutex::new((0, 0, now_millis())),
            per_sensor: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn shared(&self) -> Arc<SharedBuffer> {
        self.shared.clone()
    }

    /// Register a sensor (spawns a reader thread).
    ///
    /// `wrap` converts the sensor reading + id + timestamp into this project's `SensorReading`.
    pub fn register_sensor<S, Wrap>(&mut self, mut sensor: S, wrap: Wrap)
    where
        S: sensor_sim::traits::Sensor + Send + 'static,
        S::SensorReading: Send + 'static,
        Wrap: Fn(S::SensorReading, Arc<str>, u64) -> SensorReading + Send + Sync + 'static,
    {
        // Start generating data (spawns sensor internal thread).
        sensor.start();

        let sensor_id_key = sensor.id();
        let sensor_id: Arc<str> = Arc::from(sensor_id_key.as_str());
        let running = self.running.clone();
        let shared = self.shared.clone();
        let per_sensor = Arc::clone(&self.per_sensor);

        {
            let mut guard = self.per_sensor.lock().unwrap();
            guard.entry(sensor_id_key.clone()).or_insert(SensorQueueSnapshot {
                current_len: 0,
                peak_len: 0,
            });
        }

        let reader_handle = thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                let available = sensor.available();
                {
                    let mut guard = per_sensor.lock().unwrap();
                    if !guard.contains_key(sensor_id_key.as_str()) {
                        guard.insert(
                            sensor_id_key.clone(),
                            SensorQueueSnapshot {
                                current_len: 0,
                                peak_len: 0,
                            },
                        );
                    }
                    let entry = guard.get_mut(sensor_id_key.as_str()).unwrap();
                    entry.current_len = available;
                    if available > entry.peak_len {
                        entry.peak_len = available;
                    }
                }

                if available == 0 {
                    // Block until new data arrives (or time out to re-check `running`).
                    let _ = sensor.wait_for_data(Duration::from_millis(50));
                    continue;
                }

                // Drain a burst of already-buffered sensor samples in one scheduling slice.
                // This keeps up with high-rate producers and protects the tiny sensor queue.
                let mut drained = 0usize;
                while running.load(Ordering::SeqCst) && drained < available {
                    if let Some(reading) = sensor.read() {
                        let ts = now_millis();
                        shared.push(wrap(reading, Arc::clone(&sensor_id), ts));
                        drained += 1;
                    } else {
                        break;
                    }
                }
            }
            // Stop generating data before exiting the thread.
            sensor.stop();
        });

        self.reader_handles.push(reader_handle);
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

    pub fn sensor_internal_buffers_snapshot(
        &self,
        per_sensor_capacity: usize,
        near_full_ratio: f64,
    ) -> BufferTelemetrySnapshot {
        let guard = self.per_sensor.lock().unwrap();
        let mut sensors = Vec::with_capacity(guard.len());
        let mut warnings = Vec::new();
        let mut any_near_full = false;
        let mut any_full = false;

        for (sensor_id, sample) in guard.iter() {
            let current_ratio = if per_sensor_capacity == 0 {
                0.0
            } else {
                sample.current_len as f64 / per_sensor_capacity as f64
            };
            let peak_ratio = if per_sensor_capacity == 0 {
                0.0
            } else {
                sample.peak_len as f64 / per_sensor_capacity as f64
            };
            let full = sample.current_len >= per_sensor_capacity && per_sensor_capacity > 0;
            let near_full =
                !full && per_sensor_capacity > 0 && current_ratio >= near_full_ratio;

            if full {
                any_full = true;
                warnings.push(format!(
                    "Sensor {} internal buffer is FULL ({}/{})",
                    sensor_id, sample.current_len, per_sensor_capacity
                ));
            } else if near_full {
                any_near_full = true;
                warnings.push(format!(
                    "Sensor {} internal buffer is near full ({}/{})",
                    sensor_id, sample.current_len, per_sensor_capacity
                ));
            }

            sensors.push(SensorBufferStatus {
                sensor_id: sensor_id.clone(),
                current_len: sample.current_len,
                capacity: per_sensor_capacity,
                peak_len: sample.peak_len,
                utilization_ratio: current_ratio,
                peak_utilization_ratio: peak_ratio,
                near_full,
                full,
            });
        }

        sensors.sort_by(|a, b| a.sensor_id.cmp(&b.sensor_id));

        BufferTelemetrySnapshot {
            sensors,
            any_near_full,
            any_full,
            warnings,
        }
    }

    /// Shutdown all reader threads cleanly.
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // Ensure reader threads blocked on full-buffer waits can re-check `running`.
        self.shared.wake_all();
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