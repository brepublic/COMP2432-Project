use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    thread::JoinHandle,
};

use super::traits::Sensor;
use os_lib::queue::*;
use std::sync::atomic::{AtomicU64};
use std::time::Duration;

/// Single temperature sample in degrees Celsius.
#[derive(Clone, Copy, Debug)]
pub struct ThermoReading {
    pub temperature_celsius: f32,
}

const MAX_QUEUE_SIZE: usize = 128;

/// Simulated thermometer: background thread enqueues [`ThermoReading`] values at `rate_per_sec`.
pub struct Thermometer {
    id: String,
    rate_per_sec: u32,
    _queue: Box<RWRoundQueue<ThermoReading>>,
    reader: QueueReader<ThermoReading>,
    writer: Option<QueueWriter<ThermoReading>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<QueueWriter<ThermoReading>>>,
    notify_seq: Arc<AtomicU64>,
    notify_mu: Arc<Mutex<()>>,
    notify_cv: Arc<Condvar>,
}

impl Thermometer {
    /// Spawns the producer thread if not already running; no-op when a handle already exists.
    ///
    /// # Arguments
    ///
    /// * `self` — Thermometer with `writer` still present (must not call `start` twice without `stop`).
    ///
    /// # Returns
    ///
    /// `()`. Panics if `start` was called twice without restoring `writer` via `stop`.
    pub fn start_thread(&mut self) {
        // Test whether handle already exists
        if self.handle.is_some() {
            return;
        }

        self.running.store(true, Ordering::Relaxed);

        let mut writer = self.writer.take().expect("start called twice");
        let rate_per_sec = self.rate_per_sec;
        let running = Arc::clone(&self.running);
        let notify_seq = Arc::clone(&self.notify_seq);
        let notify_cv = Arc::clone(&self.notify_cv);

        // Implementation for starting data generation thread
        self.handle = Some(std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                // Simulate temperature reading generation
                let reading = ThermoReading {
                    temperature_celsius: 20.0 + rand::random::<f32>() * 10.0,
                };

                // Write reading to the queue
                unsafe {
                    writer.write(reading);
                }
                notify_seq.fetch_add(1, Ordering::Release);
                notify_cv.notify_all();

                // Sleep according to rate_per_sec
                std::thread::sleep(std::time::Duration::from_millis(1000 / rate_per_sec as u64));
            } 
            return writer; 
        }));
    }

    /// Signals the producer to exit and joins it, putting the [`QueueWriter`] back in `self`.
    ///
    /// # Arguments
    ///
    /// * `self` — Thermometer with an active producer handle, if any.
    ///
    /// # Returns
    ///
    /// `()`. Panics if the thread panicked while joining.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let writer = handle.join().expect("thread panicked");
            self.writer = Some(writer);
        }
    }
}

impl Sensor for Thermometer {
    type SensorReading = ThermoReading;

    /// See [`Sensor::new`]. Allocates a `128`-slot ring and splits reader/writer handles.
    fn new(id: String, rate_per_sec: u32) -> Self {
        // Fix: keep the queue at a stable address before splitting. Previously we
        // split and then moved the queue into the struct, which invalidated the
        // raw pointers held by reader/writer (use-after-move/UB).
        let mut queue = Box::new(RWRoundQueue::new(MAX_QUEUE_SIZE).unwrap());
        let (reader, writer) = unsafe { queue.as_mut().split() };

        Thermometer {
            id,
            rate_per_sec,
            _queue: queue,
            reader,
            writer: Some(writer),
            running: Arc::new(AtomicBool::new(true)),
            handle: None,
            notify_seq: Arc::new(AtomicU64::new(0)),
            notify_mu: Arc::new(Mutex::new(())),
            notify_cv: Arc::new(Condvar::new()),
        }
    }

    /// See [`Sensor::start`]. Delegates to [`Thermometer::start_thread`].
    fn start(&mut self) {
        self.start_thread();
    }

    /// See [`Sensor::read`]. Reads from the internal [`QueueReader`].
    fn read(&self) -> Option<Self::SensorReading> {
        self.reader.read()
    }

    /// See [`Sensor::available`]. Uses reader-side queue length.
    fn available(&self) -> usize {
        self.reader.len()
    }

    /// See [`Sensor::id`].
    fn id(&self) -> String {
        self.id.clone()
    }

    /// See [`Sensor::stop`]. Calls [`Thermometer::stop`].
    fn stop(&mut self) {
        Thermometer::stop(self);
    }

    /// See [`Sensor::wait_for_data`]. Uses a condition variable when the buffer is empty.
    fn wait_for_data(&self, timeout: Duration) -> bool {
        if self.available() > 0 {
            return true;
        }
        let last = self.notify_seq.load(Ordering::Acquire);
        let guard = self.notify_mu.lock().unwrap();
        let _ = self
            .notify_cv
            .wait_timeout_while(guard, timeout, |_| {
                self.notify_seq.load(Ordering::Acquire) == last && self.available() == 0
            })
            .unwrap();
        self.available() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Sensor;
    use std::time::Duration;

    struct SensorGuard {
        sensor: Thermometer,
    }

    impl SensorGuard {
        /// Wraps a [`Thermometer`] for tests and stops it on drop.
        ///
        /// # Arguments
        ///
        /// * `id` — Sensor id string.
        /// * `rate_per_sec` — Producer rate passed to [`Sensor::new`].
        ///
        /// # Returns
        ///
        /// Guard owning the sensor.
        fn new(id: &str, rate_per_sec: u32) -> Self {
            Self {
                sensor: Thermometer::new(id.to_string(), rate_per_sec),
            }
        }
    }

    impl Drop for SensorGuard {
        /// Ensures [`Thermometer::stop`] runs so test threads do not leak.
        ///
        /// # Returns
        ///
        /// `()`.
        fn drop(&mut self) {
            self.sensor.stop();
        }
    }

    /// Polls `sensor.read()` until a value appears or `timeout_ms` elapses.
    ///
    /// # Arguments
    ///
    /// * `sensor` — Started or stopped thermometer under test.
    /// * `timeout_ms` — Wall-clock budget for polling.
    ///
    /// # Returns
    ///
    /// `Some(reading)` or `None` on timeout.
    fn wait_for_reading(sensor: &Thermometer, timeout_ms: u64) -> Option<ThermoReading> {
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(timeout_ms) {
            if let Some(reading) = sensor.read() {
                return Some(reading);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn start_produces_readings() {
        let mut guard = SensorGuard::new("t-1", 200);
        guard.sensor.start();

        let reading = wait_for_reading(&guard.sensor, 500);
        assert!(reading.is_some());
    }

    #[test]
    fn stop_halts_and_restart_resumes() {
        let mut guard = SensorGuard::new("t-2", 200);
        guard.sensor.start();

        assert!(wait_for_reading(&guard.sensor, 500).is_some());

        guard.sensor.stop();

        while guard.sensor.read().is_some() {}
        std::thread::sleep(Duration::from_millis(50));
        assert!(guard.sensor.read().is_none());

        guard.sensor.start();
        assert!(wait_for_reading(&guard.sensor, 500).is_some());
    }

    #[test]
    fn available_matches_reader_len() {
        let mut guard = SensorGuard::new("t-3", 200);
        guard.sensor.start();

        let _ = wait_for_reading(&guard.sensor, 500);
        assert_eq!(guard.sensor.available(), guard.sensor.reader.len());
    }
}
