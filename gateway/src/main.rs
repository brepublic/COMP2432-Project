use tokio::runtime::Runtime;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use std::path::PathBuf;

use sensor_sim::{
    accelerometer::Accelerometer,
    force_sensor::ForceSensor,
    thermometer::Thermometer,
    traits::Sensor,
};

mod sensor_buffer;
mod aggregation;
mod storage;

use sensor_buffer::SharedBuffer;
use aggregation::AggregationEngine;
use storage::DataStorage;

fn main() {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("\nExit");
        r.store(false, Ordering::SeqCst);
    })
    .expect("fail to set Ctrl+C handler");

    let buffer = SharedBuffer::new(5000);

    let mut thermo_1 = Thermometer::new("thermo-1".to_string(), 10000);
    let mut thermo_2 = Thermometer::new("thermo-2".to_string(), 10000);
    let mut accel_1 = Accelerometer::new("accel-1".to_string(), 20000);
    let mut accel_2 = Accelerometer::new("accel-2".to_string(), 20000);
    let mut force_1 = ForceSensor::new("force-1".to_string(), 15000);

    thermo_1.start();
    thermo_2.start();
    accel_1.start();
    accel_2.start();
    force_1.start();

    let buffer_clone = buffer.clone();
    let running_clone = running.clone();
    let handle_t1 = thread::spawn(move || {
        let mut sensor = thermo_1;
        while running_clone.load(Ordering::Relaxed) {
            if let Some(reading) = sensor.read() {
                let enveloped = sensor_buffer::SensorReading::Thermo(reading, sensor.id());
                buffer_clone.push(enveloped);
            } else {
                thread::sleep(Duration::from_micros(100));
            }
        }
        sensor.stop();
        println!("thermo-1 reader stopped");
    });

    let buffer_clone = buffer.clone();
    let running_clone = running.clone();
    let handle_t2 = thread::spawn(move || {
        let mut sensor = thermo_2;
        while running_clone.load(Ordering::Relaxed) {
            if let Some(reading) = sensor.read() {
                let enveloped = sensor_buffer::SensorReading::Thermo(reading, sensor.id());
                buffer_clone.push(enveloped);
            } else {
                thread::sleep(Duration::from_micros(100));
            }
        }
        sensor.stop();
        println!("thermo-2 reader stopped");
    });

    let buffer_clone = buffer.clone();
    let running_clone = running.clone();
    let handle_a1 = thread::spawn(move || {
        let mut sensor = accel_1;
        while running_clone.load(Ordering::Relaxed) {
            if let Some(reading) = sensor.read() {
                let enveloped = sensor_buffer::SensorReading::Accel(reading, sensor.id());
                buffer_clone.push(enveloped);
            } else {
                thread::sleep(Duration::from_micros(100));
            }
        }
        sensor.stop();
        println!("accel-1 reader stopped");
    });

    let buffer_clone = buffer.clone();
    let running_clone = running.clone();
    let handle_a2 = thread::spawn(move || {
        let mut sensor = accel_2;
        while running_clone.load(Ordering::Relaxed) {
            if let Some(reading) = sensor.read() {
                let enveloped = sensor_buffer::SensorReading::Accel(reading, sensor.id());
                buffer_clone.push(enveloped);
            } else {
                thread::sleep(Duration::from_micros(100));
            }
        }
        sensor.stop();
        println!("accel-2 reader stopped");
    });

    let buffer_clone = buffer.clone();
    let running_clone = running.clone();
    let handle_f1 = thread::spawn(move || {
        let mut sensor = force_1;
        while running_clone.load(Ordering::Relaxed) {
            if let Some(reading) = sensor.read() {
                let enveloped = sensor_buffer::SensorReading::Force(reading, sensor.id());
                buffer_clone.push(enveloped);
            } else {
                thread::sleep(Duration::from_micros(100));
            }
        }
        sensor.stop();
        println!("force-1 reader stopped");
    });

    let storage = Arc::new(DataStorage::new(PathBuf::from("./data")));

    let mut engine = AggregationEngine::new(
        buffer.clone(),
        storage.clone(),
        Duration::from_secs(1),
        2,
        3.0,
    );
    engine.start();


    let dashboard_handle = thread::spawn(|| {
        let rt = Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(dashboard::run("127.0.0.1:5800"));
    });

    while running.load(Ordering::Relaxed) {
        let len = buffer.len();
        println!(" {}/{} ({:.1}%)", len, buffer.capacity(),
                (len as f64 / buffer.capacity() as f64) * 100.0);
        
        thread::sleep(Duration::from_secs(1));
    }

    println!("Exit");
    engine.shutdown();
    handle_t1.join().unwrap();
    handle_t2.join().unwrap();
    handle_a1.join().unwrap();
    handle_a2.join().unwrap();
    handle_f1.join().unwrap();

}