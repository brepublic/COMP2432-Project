use std::sync::Arc;
use std::collections::VecDeque;

use sensor_sim::accelerometer::AccelReading;
use sensor_sim::force_sensor::ForceReading;
use sensor_sim::thermometer::ThermoReading;

#[derive(Debug, Clone)]
pub enum SensorReading {
    Accel(AccelReading, String),
    Force(ForceReading, String),
    Thermo(ThermoReading, String),
}

pub struct SharedBuffer {
    queue: VecDeque<SensorReading>,
    capacity: usize,
}

impl SharedBuffer {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
        })
    }

    pub fn push(&self, reading: SensorReading) {
        let mut queue = self.queue.clone();
        queue.push_back(reading);
    }

    pub fn pop(&self) -> Option<SensorReading> {
        let mut queue = self.queue.clone();
        let val = queue.pop_front();
        val
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}