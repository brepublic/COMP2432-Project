// gateway/src/main.rs
use tokio::runtime::Runtime;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use std::path::PathBuf;
use serde::Deserialize;

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

use sensor_buffer::{SensorBufferManager, SensorReading};
use aggregation::AggregationEngine;
use storage::DataStorage;

// Import the dashboard (Web server)

// Sensor generation rates (events per second).
const BUFFER_DEBUG_ENV: &str = "GATEWAY_DEBUG_BUFFER";
const GATEWAY_CONFIG_ENV: &str = "GATEWAY_CONFIG";

#[derive(Debug, Deserialize)]
struct GatewayConfig {
    sensors: SensorsConfig,
    buffer: BufferConfig,
}

#[derive(Debug, Deserialize)]
struct SensorsConfig {
    thermo_1_rate_per_sec: u32,
    thermo_2_rate_per_sec: u32,
    accel_1_rate_per_sec: u32,
    accel_2_rate_per_sec: u32,
    force_1_rate_per_sec: u32,
}

#[derive(Debug, Deserialize)]
struct BufferConfig {
    capacity: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            sensors: SensorsConfig {
                thermo_1_rate_per_sec: 50,
                thermo_2_rate_per_sec: 50,
                accel_1_rate_per_sec: 100,
                accel_2_rate_per_sec: 100,
                force_1_rate_per_sec: 75,
            },
            buffer: BufferConfig { capacity: 5000 },
        }
    }
}

fn load_gateway_config() -> GatewayConfig {
    let config_path = std::env::var(GATEWAY_CONFIG_ENV).unwrap_or_else(|_| "./config.toml".to_string());
    let cfg_text = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Warning: failed to read config file {}: {}. Using defaults.",
                config_path, e
            );
            return GatewayConfig::default();
        }
    };

    match toml::from_str::<GatewayConfig>(&cfg_text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!(
                "Warning: failed to parse config file {}: {}. Using defaults.",
                config_path, e
            );
            GatewayConfig::default()
        }
    }
}

fn main() {
    // Create an atomic boolean as a global stop flag
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    let cfg = load_gateway_config();

    // Set Ctrl+C handler for graceful shutdown
    ctrlc::set_handler(move || {
        println!("\nReceived stop signal, shutting down...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl+C handler");

    // 1. Create a buffer manager (capacity from config).
    // Optional debug mode: fail fast when the buffer is full.
    let fail_on_full = std::env::var("GATEWAY_FAIL_ON_FULL")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false);
    if fail_on_full {
        println!("Debug mode enabled: process will panic if buffer gets full.");
    }
    let mut buffer_manager = SensorBufferManager::new_with_policy(cfg.buffer.capacity, fail_on_full);
    let buffer_debug = std::env::var(BUFFER_DEBUG_ENV)
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false);

    // 2. Create and start sensors
    let mut thermo_1 =
        Thermometer::new("thermo-1".to_string(), cfg.sensors.thermo_1_rate_per_sec);
    let mut thermo_2 =
        Thermometer::new("thermo-2".to_string(), cfg.sensors.thermo_2_rate_per_sec);
    let mut accel_1 =
        Accelerometer::new("accel-1".to_string(), cfg.sensors.accel_1_rate_per_sec);
    let mut accel_2 =
        Accelerometer::new("accel-2".to_string(), cfg.sensors.accel_2_rate_per_sec);
    let mut force_1 =
        ForceSensor::new("force-1".to_string(), cfg.sensors.force_1_rate_per_sec);

    // 3. Register sensors (spawns one reader thread per sensor).
    buffer_manager.register_sensor(thermo_1, |reading, id, ts| {
        SensorReading::Thermo(reading, id, ts)
    });
    buffer_manager.register_sensor(thermo_2, |reading, id, ts| {
        SensorReading::Thermo(reading, id, ts)
    });
    buffer_manager.register_sensor(accel_1, |reading, id, ts| {
        SensorReading::Accel(reading, id, ts)
    });
    buffer_manager.register_sensor(accel_2, |reading, id, ts| {
        SensorReading::Accel(reading, id, ts)
    });
    buffer_manager.register_sensor(force_1, |reading, id, ts| {
        SensorReading::Force(reading, id, ts)
    });

    // 4. Create data storage (data files saved to ./data)
    let storage = Arc::new(DataStorage::new(PathBuf::from("./data")));

    // 5. Create aggregation engine (4 workers, 1s window, anomaly threshold 3.0)
    let mut engine = AggregationEngine::new(
        buffer_manager.shared(),
        storage.clone(),
        Duration::from_secs(1),
        4,
        3.0,
    );
    engine.start(); // Start aggregation worker threads

    // 6. Start the Web server thread (dashboard)
    let dashboard_addr =
        std::env::var("DASHBOARD_ADDR").unwrap_or_else(|_| "127.0.0.1:5800".to_string());

    let dashboard_handle = thread::spawn(move || {
        let rt = Runtime::new().expect("Failed to create tokio runtime");
        // `salvo::TcpListener::new(addr)` requires `&'static str` for the async task lifecycle.
        let dashboard_addr_static: &'static str =
            Box::leak(dashboard_addr.into_boxed_str());
        rt.block_on(dashboard::run(dashboard_addr_static));
    });

    // 7. Main thread monitors running state and prints buffer usage every second (optional)
    while running.load(Ordering::SeqCst) {
        if buffer_debug {
            let s = buffer_manager.utilization_stats();

            println!(
                "Buffer: len={}/{} ({:.1}%), pushed+{} /s, popped+{} /s",
                s.len,
                s.capacity,
                s.utilization_ratio * 100.0,
                s.pushed_per_sec,
                s.popped_per_sec
            );
        } else {
            let s = buffer_manager.utilization_stats();
            let len = s.len;
            let cap = s.capacity;
            println!(
                "Buffer usage: {}/{} ({:.1}%)",
                len,
                cap,
                s.utilization_ratio * 100.0
            );
        }
        
        // Check whether dashboard thread is still alive
        if dashboard_handle.is_finished() {
            println!("Warning: dashboard thread exited early!");
        }
        
        thread::sleep(Duration::from_secs(1));
    }

    // 8. Stop signal received: perform cleanup
    println!("Stopping reader threads...");
    buffer_manager.shutdown();

    println!("Stopping aggregation engine...");
    engine.shutdown();

    println!("Stopping Web server...");
    // The dashboard does not currently provide a graceful shutdown API.
    // For now, we do not wait for it (the process is expected to exit).

    println!("All threads stopped, exiting program.");
}