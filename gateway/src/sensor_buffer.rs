use std::sync::{Arc, Mutex, Condvar};
use std::collections::VecDeque;
use std::time::Duration;

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
    queue: Mutex<VecDeque<SensorReading>>,
    cond: Condvar,
    capacity: usize,
}

impl SharedBuffer {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            cond: Condvar::new(),
            capacity,
        })
    }

    pub fn push(&self, reading: SensorReading) {
        let mut queue = self.queue.lock().unwrap();
        while queue.len() >= self.capacity {
            queue = self.cond.wait(queue).unwrap();
        }
        queue.push_back(reading);
        self.cond.notify_one(); 
    }

    pub fn pop(&self) -> SensorReading {
        let mut queue = self.queue.lock().unwrap();
        while queue.is_empty() {
            queue = self.cond.wait(queue).unwrap();
        }
        let val = queue.pop_front().unwrap();
        self.cond.notify_one(); 
        val
    }

    pub fn pop_timeout(&self, timeout: Duration) -> Option<SensorReading> {
        let mut queue = self.queue.lock().unwrap();
        let result = self.cond
            .wait_timeout_while(queue, timeout, |q| q.is_empty())
            .unwrap();
        queue = result.0;
        if queue.is_empty() {
            None
        } else {
            Some(queue.pop_front().unwrap())
        }
    }

    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}