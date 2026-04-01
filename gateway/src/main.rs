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
mod rates;
mod sensor_buffer;
mod aggregation;
mod storage;

use sensor_buffer::{SensorBufferManager, SensorReading};
use aggregation::AggregationEngine;
use storage::DataStorage;
use common_models::ThroughputTelemetrySnapshot;

// Import the dashboard (Web server)

// Sensor generation rates (events per second).
const BUFFER_DEBUG_ENV: &str = "GATEWAY_DEBUG_BUFFER";
const GATEWAY_CONFIG_ENV: &str = "GATEWAY_CONFIG";
#[derive(Debug, Deserialize)]
#[serde(default)]
struct SensorInternalBufferConfig {
    usable_capacity: usize,
    near_full_ratio: f64,
}

impl Default for SensorInternalBufferConfig {
    fn default() -> Self {
        Self {
            // sensor_sim ring is 128 slots with one reserved empty slot => 127 usable.
            usable_capacity: 127,
            near_full_ratio: 0.85,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GatewayConfig {
    sensors: SensorsConfig,
    buffer: BufferConfig,
    #[serde(default)]
    dashboard: DashboardConfig,
    #[serde(default)]
    sensor_internal_buffer: SensorInternalBufferConfig,
}

#[derive(Debug, Deserialize)]
struct SensorsConfig {
    thermo_count: usize,
    thermo_rates_per_sec: String,
    accel_count: usize,
    accel_rates_per_sec: String,
    force_count: usize,
    force_rates_per_sec: String,
}

#[derive(Debug, Deserialize)]
struct BufferConfig {
    capacity: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct DashboardConfig {
    addr: String,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:5800".to_string(),
        }
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            sensors: SensorsConfig {
                thermo_count: 2,
                thermo_rates_per_sec: "50 50".to_string(),
                accel_count: 2,
                accel_rates_per_sec: "100 100".to_string(),
                force_count: 1,
                force_rates_per_sec: "75".to_string(),
            },
            buffer: BufferConfig { capacity: 5000 },
            dashboard: DashboardConfig::default(),
            sensor_internal_buffer: SensorInternalBufferConfig::default(),
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

    // 2. Create and start sensors dynamically from config.
    let thermo_rates = rates::parse_rates(&cfg.sensors.thermo_rates_per_sec, 50);
    let accel_rates = rates::parse_rates(&cfg.sensors.accel_rates_per_sec, 100);
    let force_rates = rates::parse_rates(&cfg.sensors.force_rates_per_sec, 75);

    // 3. Register sensors (spawns one reader thread per sensor).
    for i in 0..cfg.sensors.thermo_count {
        let sensor_id = format!("thermo-{}", i + 1);
        let rate = rates::rate_for_index(&thermo_rates, i);
        let sensor = Thermometer::new(sensor_id, rate);
        buffer_manager.register_sensor(sensor, |reading, id, ts, internal_len| {
            SensorReading::Thermo(reading, id, ts, internal_len)
        });
    }
    for i in 0..cfg.sensors.accel_count {
        let sensor_id = format!("accel-{}", i + 1);
        let rate = rates::rate_for_index(&accel_rates, i);
        let sensor = Accelerometer::new(sensor_id, rate);
        buffer_manager.register_sensor(sensor, |reading, id, ts, internal_len| {
            SensorReading::Accel(reading, id, ts, internal_len)
        });
    }
    for i in 0..cfg.sensors.force_count {
        let sensor_id = format!("force-{}", i + 1);
        let rate = rates::rate_for_index(&force_rates, i);
        let sensor = ForceSensor::new(sensor_id, rate);
        buffer_manager.register_sensor(sensor, |reading, id, ts, internal_len| {
            SensorReading::Force(reading, id, ts, internal_len)
        });
    }

    let total_sensors = cfg.sensors.thermo_count + cfg.sensors.accel_count + cfg.sensors.force_count;

    // 4. Create data storage (data files saved to ./data)
    let storage = Arc::new(DataStorage::new(PathBuf::from("./data")));

    // 5. Create aggregation engine with a bounded worker pool sized by load and CPU.
    let cpu_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let aggregation_workers = total_sensors.clamp(1, cpu_workers.max(1));
    println!(
        "Configured sensors: thermo={}, accel={}, force={} (total={}), aggregation_workers={}",
        cfg.sensors.thermo_count,
        cfg.sensors.accel_count,
        cfg.sensors.force_count,
        total_sensors,
        aggregation_workers
    );

    let mut engine = AggregationEngine::new(
        buffer_manager.shared(),
        storage.clone(),
        Duration::from_secs(1),
        aggregation_workers,
        3.0,
    );
    engine.start(); // Start aggregation worker threads

    // 6. Start the Web server thread (dashboard)
    let dashboard_addr = if cfg.dashboard.addr.trim().is_empty() {
        std::env::var("DASHBOARD_ADDR").unwrap_or_else(|_| "0.0.0.0:5800".to_string())
    } else {
        cfg.dashboard.addr.clone()
    };

    dashboard::set_internal_buffer_policy( cfg.sensor_internal_buffer.usable_capacity, 
        cfg.sensor_internal_buffer.near_full_ratio,

    );

    let dashboard_handle = thread::spawn(move || {
        let rt = Runtime::new().expect("Failed to create tokio runtime");
        // `salvo::TcpListener::new(addr)` requires `&'static str` for the async task lifecycle.
        let dashboard_addr_static: &'static str =
            Box::leak(dashboard_addr.into_boxed_str());
        rt.block_on(dashboard::run(dashboard_addr_static));
    });

    // 7. Main thread monitors running state and prints buffer usage every second (optional)
    while running.load(Ordering::SeqCst) {
        let sensor_buffers = buffer_manager.sensor_internal_buffers_snapshot(
            cfg.sensor_internal_buffer.usable_capacity,
            cfg.sensor_internal_buffer.near_full_ratio,
        );
        dashboard::set_buffer_telemetry(sensor_buffers.clone());
        let throughput_stats = buffer_manager.utilization_stats();
        dashboard::set_throughput_telemetry(ThroughputTelemetrySnapshot {
            buffer_len: throughput_stats.len,
            buffer_capacity: throughput_stats.capacity,
            pushed_total: throughput_stats.pushed_total,
            popped_total: throughput_stats.popped_total,
            pushed_per_sec: throughput_stats.pushed_per_sec,
            popped_per_sec: throughput_stats.popped_per_sec,
            full_waits_total: throughput_stats.full_waits_total,
        });

        if sensor_buffers.any_full {
            println!("WARNING: at least one sensor internal buffer is FULL!");
            for warning in &sensor_buffers.warnings {
                println!("  - {}", warning);
            }
        } else if sensor_buffers.any_near_full {
            println!("WARNING: at least one sensor internal buffer is near full.");
            for warning in &sensor_buffers.warnings {
                println!("  - {}", warning);
            }
        }

        if buffer_debug {
            let s = throughput_stats.clone();

            println!(
                "Buffer: len={}/{} ({:.1}%), pushed+{} /s, popped+{} /s",
                s.len,
                s.capacity,
                s.utilization_ratio * 100.0,
                s.pushed_per_sec,
                s.popped_per_sec
            );
        } else {
            let s = throughput_stats;
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
    dashboard::request_shutdown();
    if let Err(e) = dashboard_handle.join() {
        eprintln!("Warning: dashboard thread join failed: {:?}", e);
    }

    println!("All threads stopped, exiting program.");
}
