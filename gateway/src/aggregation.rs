// gateway/src/aggregation.rs

use std::time::Duration;
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use chrono::Utc; // Add chrono dependency

use crate::sensor_buffer::{SharedBuffer, SensorReading};
use crate::storage::DataStorage;

// Shared data models for aggregation frames and statistics.
pub use common_models::{Anomaly, AggregatedFrame, SensorStats};

pub struct AggregationEngine {
    buffer: Arc<SharedBuffer>,
    storage: Arc<DataStorage>,
    window_duration: Duration,
    num_workers: usize,
    anomaly_threshold: f32,
    worker_handles: Vec<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl AggregationEngine {
    pub fn new(
        buffer: Arc<SharedBuffer>,
        storage: Arc<DataStorage>,
        window_duration: Duration,
        num_workers: usize,
        anomaly_threshold: f32,
    ) -> Self {
        Self {
            buffer,
            storage,
            window_duration,
            num_workers,
            anomaly_threshold,
            worker_handles: Vec::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&mut self) {
        for id in 0..self.num_workers {
            let buffer = Arc::clone(&self.buffer);
            let storage = Arc::clone(&self.storage);
            let window_duration = self.window_duration;
            let threshold = self.anomaly_threshold;
            let shutdown = Arc::clone(&self.shutdown);

            let handle = thread::spawn(move || {
                let mut frame_id = id as u64;
                let mut sensor_data: HashMap<String, Vec<SensorReading>> = HashMap::new();

                // Use std::time::Instant to measure elapsed time; use chrono::Utc for timestamps
                let mut window_start_instant = std::time::Instant::now();
                let mut window_start_millis = Utc::now().timestamp_millis() as u64;

                loop {
                    // Check shutdown signal
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }

                    // Try to pop from the buffer (timeout 10ms)
                    if let Some(reading) = buffer.pop_timeout(Duration::from_millis(10)) {
                        let sensor_id = match &reading {
                            SensorReading::Accel(_, id) => id.clone(),
                            SensorReading::Force(_, id) => id.clone(),
                            SensorReading::Thermo(_, id) => id.clone(),
                        };
                        sensor_data.entry(sensor_id).or_insert_with(Vec::new).push(reading);
                    }

                    // Check whether the current window has ended
                    if window_start_instant.elapsed() >= window_duration {
                        let window_end_millis = Utc::now().timestamp_millis() as u64;
                        let mut stats_map = HashMap::new();
                        let mut anomalies = Vec::new();

                        for (sensor_id, readings) in &sensor_data {
                            let stats = Self::compute_stats(readings);
                            Self::detect_anomalies(sensor_id, readings, &stats, threshold, &mut anomalies);
                            stats_map.insert(sensor_id.clone(), stats);
                        }

                        let frame = AggregatedFrame {
                            frame_id,
                            window_start: window_start_millis,
                            window_end: window_end_millis,
                            sensor_stats: stats_map,
                            anomalies,
                        };

                        storage.write(frame);

                        // Prepare the next window
                        window_start_instant = std::time::Instant::now();
                        window_start_millis = window_end_millis;
                        sensor_data.clear();
                        frame_id += 1;
                    }
                }
            });

            self.worker_handles.push(handle);
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        while let Some(handle) = self.worker_handles.pop() {
            handle.join().unwrap();
        }
    }

    fn compute_stats(readings: &[SensorReading]) -> SensorStats {
        let mut count = 0;
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;

        for r in readings {
            let value = match r {
                SensorReading::Accel(r, _) => (r.acceleration_x + r.acceleration_y + r.acceleration_z) / 3.0,
                SensorReading::Force(r, _) => (r.force_x + r.force_y + r.force_z) / 3.0,
                SensorReading::Thermo(r, _) => r.temperature_celsius,
            };
            count += 1;
            if value < min { min = value; }
            if value > max { max = value; }
            sum += value;
            sum_sq += value * value;
        }

        let avg = if count > 0 { sum / count as f32 } else { 0.0 };
        let variance = if count > 0 { (sum_sq / count as f32) - (avg * avg) } else { 0.0 };
        let stddev = variance.sqrt();

        SensorStats {
            count,
            min,
            max,
            sum,
            sum_sq,
            avg,
            stddev,
        }
    }

    fn detect_anomalies(
        sensor_id: &str,
        readings: &[SensorReading],
        stats: &SensorStats,
        threshold: f32,
        anomalies: &mut Vec<Anomaly>,
    ) {
        for r in readings {
            let value = match r {
                SensorReading::Accel(r, _) => (r.acceleration_x + r.acceleration_y + r.acceleration_z) / 3.0,
                SensorReading::Force(r, _) => (r.force_x + r.force_y + r.force_z) / 3.0,
                SensorReading::Thermo(r, _) => r.temperature_celsius,
            };
            if stats.stddev > 0.0 && (value - stats.avg).abs() > threshold * stats.stddev {
                anomalies.push(Anomaly {
                    sensor_id: sensor_id.to_string(),
                    anomaly_type: "Deviation".to_string(),
                    severity: (value - stats.avg).abs() / stats.stddev,
                    description: format!("Value {} deviates from mean {}", value, stats.avg),
                });
            }
        }
    }
}