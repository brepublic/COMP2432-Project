use std::time::Duration;
use std::sync::{Arc, Mutex};

use crate::sensor_buffer::SharedBuffer;
use crate::storage::DataStorage;

pub struct AggregationEngine {
    buffer: Arc<SharedBuffer>,
    storage: Arc<DataStorage>,
    window_duration: Duration,
    num_workers: usize,
    anomaly_threshold: f32,
    shutdown: Arc<Mutex<bool>>,
}

impl AggregationEngine {
    pub fn new(buffer: Arc<SharedBuffer>,storage: Arc<DataStorage>,window_duration: Duration,num_workers: usize,anomaly_threshold: f32,) -> Self {
        Self {buffer,storage,window_duration,num_workers,anomaly_threshold,shutdown: Arc::new(Mutex::new(false)),}
    }

    pub fn start(&mut self) {

    }

    pub fn shutdown(&mut self) {

    }
}