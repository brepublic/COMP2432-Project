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
}

