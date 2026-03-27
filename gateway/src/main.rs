// gateway/src/main.rs
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

// Import custom modules
mod sensor_buffer;
mod aggregation;
mod storage;

use sensor_buffer::SharedBuffer;
use aggregation::AggregationEngine;
use storage::DataStorage;

// Import the dashboard (Web server)

fn main() {
    // Create an atomic boolean as a global stop flag
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Set Ctrl+C handler for graceful shutdown
    ctrlc::set_handler(move || {
        println!("\nReceived stop signal, shutting down...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl+C handler");

    // 1. Create a shared buffer (capacity 5000, enough for bursts)
    // Optional debug mode: fail fast when the buffer is full.
    let fail_on_full = std::env::var("GATEWAY_FAIL_ON_FULL")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false);
    if fail_on_full {
        println!("Debug mode enabled: process will panic if SharedBuffer gets full.");
    }
    let buffer = SharedBuffer::new_with_policy(5000, fail_on_full);

    // 2. Create and start sensors
    let mut thermo_1 = Thermometer::new("thermo-1".to_string(), 10);
    let mut thermo_2 = Thermometer::new("thermo-2".to_string(), 10);
    let mut accel_1 = Accelerometer::new("accel-1".to_string(), 20);
    let mut accel_2 = Accelerometer::new("accel-2".to_string(), 20);
    let mut force_1 = ForceSensor::new("force-1".to_string(), 15);

    thermo_1.start();
    thermo_2.start();
    accel_1.start();
    accel_2.start();
    force_1.start();

    // 3. Spawn reader threads for each sensor and push readings to the buffer
    let buffer_clone = buffer.clone();
    let running_clone = running.clone();
    let handle_t1 = thread::spawn(move || {
        let mut sensor = thermo_1;
        while running_clone.load(Ordering::SeqCst) {
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
        while running_clone.load(Ordering::SeqCst) {
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
        while running_clone.load(Ordering::SeqCst) {
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
        while running_clone.load(Ordering::SeqCst) {
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
        while running_clone.load(Ordering::SeqCst) {
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

    // 4. Create data storage (data files saved to ./data)
    let storage = Arc::new(DataStorage::new(PathBuf::from("./data")));

    // 5. Create aggregation engine (4 workers, 1s window, anomaly threshold 3.0)
    let mut engine = AggregationEngine::new(
        buffer.clone(),
        storage.clone(),
        Duration::from_secs(1),
        4,
        3.0,
    );
    engine.start(); // Start aggregation worker threads

    // 6. Start the Web server thread (dashboard)

    let dashboard_handle = thread::spawn(|| {
        let rt = Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(dashboard::run("127.0.0.1:5800"));
    });

    // 7. Main thread monitors running state and prints buffer usage every second (optional)
    while running.load(Ordering::SeqCst) {
        let len = buffer.len();
        println!(
            "Buffer usage: {}/{} ({:.1}%)",
            len,
            buffer.capacity(),
                (len as f64 / buffer.capacity() as f64) * 100.0);
        
        // Check whether dashboard thread is still alive
        if dashboard_handle.is_finished() {
            println!("Warning: dashboard thread exited early!");
        }
        
        thread::sleep(Duration::from_secs(1));
    }

    // 8. Stop signal received: perform cleanup
    println!("Stopping aggregation engine...");
    engine.shutdown();

    println!("Waiting for reader threads to finish...");
    // Wait for all reader threads to finish (they exit after `running` becomes false)
    handle_t1.join().unwrap();
    handle_t2.join().unwrap();
    handle_a1.join().unwrap();
    handle_a2.join().unwrap();
    handle_f1.join().unwrap();

    println!("Stopping Web server...");
    // The dashboard does not currently provide a graceful shutdown API.
    // For now, we do not wait for it (the process is expected to exit).

    println!("All threads stopped, exiting program.");
}