// gateway/src/aggregation.rs

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
    frame_id: Arc<AtomicU64>,
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
            frame_id: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn start(&mut self) {
        let window_ms = self.window_duration.as_millis().max(1) as u64;
        let grace_ms = std::cmp::max(50, window_ms / 2);

        // Pending windows keyed by `window_start`.
        let pending: Arc<Mutex<BTreeMap<u64, HashMap<String, Vec<SensorReading>>>>> =
            Arc::new(Mutex::new(BTreeMap::new()));

        for _ in 0..self.num_workers {
            let buffer = Arc::clone(&self.buffer);
            let storage = Arc::clone(&self.storage);
            let pending = Arc::clone(&pending);
            let shutdown = Arc::clone(&self.shutdown);
            let frame_id = Arc::clone(&self.frame_id);
            let threshold = self.anomaly_threshold;

            let handle = thread::spawn(move || loop {
                // On shutdown, finalize any remaining pending windows immediately.
                if shutdown.load(Ordering::SeqCst) && buffer.len() == 0 {
                    let windows = {
                        let mut guard = pending.lock().unwrap();
                        std::mem::take(&mut *guard)
                    };
                    for (ws, readings_by_sensor) in windows {
                        // Preserve deterministic ordering on shutdown as well.
                        let fid = frame_id.fetch_add(1, Ordering::SeqCst);
                        let frame = make_frame(
                            fid,
                            ws,
                            ws + window_ms,
                            readings_by_sensor,
                            threshold,
                            &storage,
                        );
                        storage.write(frame);
                    }
                    return;
                }

                // Pull readings from the shared buffer concurrently.
                if let Some(reading) = buffer.pop_timeout(Duration::from_millis(10)) {
                    let ts = reading.timestamp_millis();
                    let window_start = (ts / window_ms) * window_ms;
                    let sensor_id = reading.sensor_id().to_string();

                    let mut guard = pending.lock().unwrap();
                    guard
                        .entry(window_start)
                        .or_insert_with(HashMap::new)
                        .entry(sensor_id)
                        .or_insert_with(Vec::new)
                        .push(reading);
                }

                // Dispatch any windows that are "old enough" (claim by removing under lock).
                let now_ms = system_now_millis();
                let ready: Vec<(u64, u64, HashMap<String, Vec<SensorReading>>)> = {
                    let mut guard = pending.lock().unwrap();
                    let mut out = Vec::new();
                    loop {
                        let ws = if let Some((&ws, _)) = guard.iter().next() {
                            ws
                        } else {
                            break;
                        };
                        let we = ws + window_ms;
                        if now_ms >= we + grace_ms {
                            let readings_by_sensor = guard.remove(&ws).unwrap();
                            // Assign frame_id when a window is claimed, not when work finishes.
                            // This keeps IDs monotonic with window order even under multi-worker races.
                            let fid = frame_id.fetch_add(1, Ordering::SeqCst);
                            out.push((fid, ws, readings_by_sensor));
                        } else {
                            break;
                        }
                    }
                    out
                };

                for (fid, ws, readings_by_sensor) in ready {
                    let frame = make_frame(
                        fid,
                        ws,
                        ws + window_ms,
                        readings_by_sensor,
                        threshold,
                        &storage,
                    );
                    storage.write(frame);
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
        let mut count = 0usize;
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut sum = 0.0f32;
        let mut sum_sq = 0.0f32;

        for r in readings {
            let value = match r {
                SensorReading::Accel(v, _, _) => {
                    (v.acceleration_x + v.acceleration_y + v.acceleration_z) / 3.0
                }
                SensorReading::Force(v, _, _) => {
                    (v.force_x + v.force_y + v.force_z) / 3.0
                }
                SensorReading::Thermo(v, _, _) => v.temperature_celsius,
            };
            count += 1;
            if value < min {
                min = value;
            }
            if value > max {
                max = value;
            }
            sum += value;
            sum_sq += value * value;
        }

        let avg = if count > 0 { sum / count as f32 } else { 0.0 };
        let variance = if count > 0 {
            (sum_sq / count as f32) - (avg * avg)
        } else {
            0.0
        };
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
                SensorReading::Accel(v, _, _) => {
                    (v.acceleration_x + v.acceleration_y + v.acceleration_z) / 3.0
                }
                SensorReading::Force(v, _, _) => {
                    (v.force_x + v.force_y + v.force_z) / 3.0
                }
                SensorReading::Thermo(v, _, _) => v.temperature_celsius,
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

fn system_now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn make_frame(
    frame_id: u64,
    window_start: u64,
    window_end: u64,
    readings_by_sensor: HashMap<String, Vec<SensorReading>>,
    threshold: f32,
    storage: &DataStorage,
) -> AggregatedFrame {
    let mut stats_map: HashMap<String, SensorStats> = HashMap::new();
    let mut anomalies: Vec<Anomaly> = Vec::new();

    for (sensor_id, readings) in readings_by_sensor {
        let stats = AggregationEngine::compute_stats(&readings);
        AggregationEngine::detect_anomalies(
            &sensor_id,
            &readings,
            &stats,
            threshold,
            &mut anomalies,
        );
        stats_map.insert(sensor_id, stats);
    }

    let _ = storage; // keep signature symmetrical; storage used by caller.

    AggregatedFrame {
        frame_id,
        window_start,
        window_end,
        sensor_stats: stats_map,
        anomalies,
    }
}