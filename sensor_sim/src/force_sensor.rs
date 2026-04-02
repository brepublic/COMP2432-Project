use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
};

use crate::traits::Sensor;
use os_lib::queue::*;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Single tri-axial force sample (simulated units).
#[derive(Clone, Copy, Debug)]
pub struct ForceReading {
    pub force_x: f32,
    pub force_y: f32,
    pub force_z: f32,
}

const MAX_QUEUE_SIZE: usize = 128;

/// Simulated force sensor enqueueing [`ForceReading`] at `rate_per_sec`.
pub struct ForceSensor {
    id: String,
    rate_per_sec: u32,
    _queue: Box<RWRoundQueue<ForceReading>>,
    reader: QueueReader<ForceReading>,
    writer: Option<QueueWriter<ForceReading>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<QueueWriter<ForceReading>>>,
    notify_seq: Arc<AtomicU64>,
    notify_mu: Arc<Mutex<()>>,
    notify_cv: Arc<Condvar>,
}

impl ForceSensor {
    /// Spawns the producer thread once; same contract as [`Accelerometer::start_thread`].
    ///
    /// # Arguments
    ///
    /// * `self` — Force sensor with writer available.
    ///
    /// # Returns
    ///
    /// `()`.
    pub fn start_thread(&mut self) {
        if self.handle.is_some() {
            return;
        }

        self.running.store(true, Ordering::Relaxed);

        let mut writer = self.writer.take().expect("start called twice");
        let rate_per_sec = self.rate_per_sec;
        let running = Arc::clone(&self.running);
        let notify_seq = Arc::clone(&self.notify_seq);
        let notify_cv = Arc::clone(&self.notify_cv);

        self.handle = Some(std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let reading = ForceReading {
                    force_x: rand::random::<f32>() * 100.0,
                    force_y: rand::random::<f32>() * 100.0,
                    force_z: rand::random::<f32>() * 100.0,
                };

                unsafe {
                    writer.write(reading);
                }
                notify_seq.fetch_add(1, Ordering::Release);
                notify_cv.notify_all();

                std::thread::sleep(std::time::Duration::from_millis(
                    1000 / rate_per_sec as u64,
                ));
            }
            writer
        }));
    }

    /// Stops the producer thread; same as [`Accelerometer::stop`].
    ///
    /// # Arguments
    ///
    /// * `self` — Force sensor state.
    ///
    /// # Returns
    ///
    /// `()`.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let writer = handle.join().expect("thread panicked");
            self.writer = Some(writer);
        }
    }
}

impl Sensor for ForceSensor {
    type SensorReading = ForceReading;

    /// See [`Sensor::new`].
    fn new(id: String, rate_per_sec: u32) -> Self {
        let mut queue = Box::new(RWRoundQueue::new(MAX_QUEUE_SIZE).unwrap());
        let (reader, writer) = unsafe { queue.as_mut().split() };

        ForceSensor {
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

    /// See [`Sensor::start`].
    fn start(&mut self) {
        self.start_thread();
    }

    /// See [`Sensor::read`].
    fn read(&self) -> Option<Self::SensorReading> {
        self.reader.read()
    }

    /// See [`Sensor::available`].
    fn available(&self) -> usize {
        self.reader.len()
    }

    /// See [`Sensor::id`].
    fn id(&self) -> String {
        self.id.clone()
    }

    /// See [`Sensor::stop`].
    fn stop(&mut self) {
        ForceSensor::stop(self);
    }

    /// See [`Sensor::wait_for_data`].
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
