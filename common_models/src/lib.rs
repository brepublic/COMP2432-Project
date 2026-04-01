use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorStats {
    pub count: usize,
    pub min: f32,
    pub max: f32,
    pub sum: f32,
    pub sum_sq: f32,
    pub avg: f32,
    pub stddev: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub sensor_id: String,
    pub anomaly_type: String,
    pub severity: f32,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedFrame {
    pub frame_id: u64,
    pub window_start: u64,
    pub window_end: u64,
    pub sensor_stats: HashMap<String, SensorStats>,
    pub anomalies: Vec<Anomaly>,
    /// Per-sensor max internal queue depth observed on any sample in this window.
    #[serde(default)]
    pub sensor_internal_buffer_max: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorBufferStatus {
    pub sensor_id: String,
    pub current_len: usize,
    pub capacity: usize,
    pub peak_len: usize,
    pub utilization_ratio: f64,
    pub peak_utilization_ratio: f64,
    pub near_full: bool,
    pub full: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferTelemetrySnapshot {
    pub sensors: Vec<SensorBufferStatus>,
    pub any_near_full: bool,
    pub any_full: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputTelemetrySnapshot {
    pub buffer_len: usize,
    pub buffer_capacity: usize,
    pub pushed_total: u64,
    pub popped_total: u64,
    pub pushed_per_sec: u64,
    pub popped_per_sec: u64,
    pub full_waits_total: u64,
}

