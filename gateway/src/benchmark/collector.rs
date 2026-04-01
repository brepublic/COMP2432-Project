use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_models::{AggregatedFrame, BufferTelemetrySnapshot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingConfig {
    pub scenario_id: String,
    pub base_url: String,
    pub warmup_secs: u64,
    pub measure_secs: u64,
    pub cooldown_secs: u64,
    pub poll_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleRow {
    pub ts_ms: u64,
    pub phase: String,
    pub any_full: bool,
    pub any_near_full: bool,
    pub peak_sensor_utilization: f64,
    pub total_frames: usize,
    pub latest_window_end: u64,
    pub pipeline_latency_ms: u64,
    pub cpu_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingResult {
    pub scenario_id: String,
    pub total_samples: usize,
    pub measured_samples: usize,
    pub samples: Vec<SampleRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpLoadMetrics {
    pub success: u64,
    pub fail: u64,
    pub latencies_us: Vec<u64>,
}

#[derive(Debug, Deserialize)]
struct StatsResponse {
    total_frames: usize,
    latest_window_end: Option<u64>,
}

pub struct HttpLoadHandle {
    running: Arc<AtomicBool>,
    join_handles: Vec<thread::JoinHandle<()>>,
    shared: Arc<Mutex<HttpLoadMetrics>>,
}

impl HttpLoadHandle {
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        for handle in self.join_handles.drain(..) {
            let _ = handle.join();
        }
    }

    pub fn metrics(&self) -> HttpLoadMetrics {
        self.shared
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

pub fn run_http_load(
    base_url: String,
    endpoint: String,
    concurrency: usize,
    timeout_ms: u64,
) -> HttpLoadHandle {
    let running = Arc::new(AtomicBool::new(true));
    let shared = Arc::new(Mutex::new(HttpLoadMetrics::default()));

    let mut join_handles = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let running_clone = Arc::clone(&running);
        let shared_clone = Arc::clone(&shared);
        let url = format!("{base_url}{endpoint}");
        let handle = thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(timeout_ms))
                .build();
            let Ok(client) = client else {
                return;
            };
            while running_clone.load(Ordering::SeqCst) {
                let t0 = Instant::now();
                let ok = client
                    .get(&url)
                    .send()
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                let latency_us = t0.elapsed().as_micros() as u64;
                if let Ok(mut g) = shared_clone.lock() {
                    if ok {
                        g.success += 1;
                        g.latencies_us.push(latency_us);
                    } else {
                        g.fail += 1;
                    }
                }
            }
        });
        join_handles.push(handle);
    }

    HttpLoadHandle {
        running,
        join_handles,
        shared,
    }
}

pub fn run_sampling(
    cfg: SamplingConfig,
    gateway_pid: u32,
    csv_path: PathBuf,
) -> Result<SamplingResult, String> {
    let mut file = File::create(&csv_path)
        .map_err(|e| format!("failed to create {}: {e}", csv_path.display()))?;
    writeln!(
        file,
        "ts_ms,phase,any_full,any_near_full,peak_sensor_utilization,total_frames,latest_window_end,pipeline_latency_ms,cpu_pct"
    )
    .map_err(|e| format!("failed to write csv header: {e}"))?;

    let poll_interval = Duration::from_millis(cfg.poll_interval_ms.max(200));
    let total_duration = cfg.warmup_secs + cfg.measure_secs + cfg.cooldown_secs;
    let start = Instant::now();
    let mut samples = Vec::new();
    let mut cpu = CpuSampler::new(gateway_pid)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("failed to build sampler client: {e}"))?;

    while start.elapsed().as_secs() < total_duration {
        let elapsed = start.elapsed().as_secs();
        let phase = if elapsed < cfg.warmup_secs {
            "warmup"
        } else if elapsed < cfg.warmup_secs + cfg.measure_secs {
            "measure"
        } else {
            "cooldown"
        }
        .to_string();

        let buffer: BufferTelemetrySnapshot = client
            .get(format!("{}/api/buffer", cfg.base_url))
            .send()
            .map_err(|e| format!("sampling /api/buffer failed: {e}"))?
            .json()
            .map_err(|e| format!("parse /api/buffer failed: {e}"))?;
        let stats: StatsResponse = client
            .get(format!("{}/api/stats", cfg.base_url))
            .send()
            .map_err(|e| format!("sampling /api/stats failed: {e}"))?
            .json()
            .map_err(|e| format!("parse /api/stats failed: {e}"))?;

        let peak_sensor_utilization = buffer
            .sensors
            .iter()
            .map(|s| s.utilization_ratio)
            .fold(0.0f64, f64::max);
        let latest_window_end = stats.latest_window_end.unwrap_or(0);
        let now_ms = now_millis();
        let pipeline_latency_ms = now_ms.saturating_sub(latest_window_end);
        let cpu_pct = cpu.sample().unwrap_or(0.0);

        let row = SampleRow {
            ts_ms: now_ms,
            phase,
            any_full: buffer.any_full,
            any_near_full: buffer.any_near_full,
            peak_sensor_utilization,
            total_frames: stats.total_frames,
            latest_window_end,
            pipeline_latency_ms,
            cpu_pct,
        };
        writeln!(
            file,
            "{},{},{},{},{:.6},{},{},{},{:.2}",
            row.ts_ms,
            row.phase,
            row.any_full,
            row.any_near_full,
            row.peak_sensor_utilization,
            row.total_frames,
            row.latest_window_end,
            row.pipeline_latency_ms,
            row.cpu_pct
        )
        .map_err(|e| format!("failed to append sample row: {e}"))?;
        samples.push(row);
        thread::sleep(poll_interval);
    }

    let measured_samples = samples.iter().filter(|r| r.phase == "measure").count();
    Ok(SamplingResult {
        scenario_id: cfg.scenario_id,
        total_samples: samples.len(),
        measured_samples,
        samples,
    })
}

struct CpuSampler {
    pid: u32,
    last_proc_jiffies: AtomicU64,
    last_total_jiffies: AtomicU64,
}

impl CpuSampler {
    fn new(pid: u32) -> Result<Self, String> {
        let proc = read_proc_jiffies(pid)?;
        let total = read_total_jiffies()?;
        Ok(Self {
            pid,
            last_proc_jiffies: AtomicU64::new(proc),
            last_total_jiffies: AtomicU64::new(total),
        })
    }

    fn sample(&mut self) -> Result<f64, String> {
        let cur_proc = read_proc_jiffies(self.pid)?;
        let cur_total = read_total_jiffies()?;
        let prev_proc = self.last_proc_jiffies.swap(cur_proc, Ordering::SeqCst);
        let prev_total = self.last_total_jiffies.swap(cur_total, Ordering::SeqCst);

        let d_proc = cur_proc.saturating_sub(prev_proc);
        let d_total = cur_total.saturating_sub(prev_total);
        if d_total == 0 {
            return Ok(0.0);
        }
        Ok((d_proc as f64 / d_total as f64) * 100.0)
    }
}

fn read_proc_jiffies(pid: u32) -> Result<u64, String> {
    let path = format!("/proc/{pid}/stat");
    let content = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
    let rparen = content.rfind(')').ok_or_else(|| format!("bad stat format: {path}"))?;
    let after = content[rparen + 2..].split_whitespace().collect::<Vec<_>>();
    if after.len() < 15 {
        return Err(format!("stat fields too short: {path}"));
    }
    let utime: u64 = after[11]
        .parse()
        .map_err(|e| format!("parse utime in {path}: {e}"))?;
    let stime: u64 = after[12]
        .parse()
        .map_err(|e| format!("parse stime in {path}: {e}"))?;
    Ok(utime + stime)
}

fn read_total_jiffies() -> Result<u64, String> {
    let content = std::fs::read_to_string("/proc/stat").map_err(|e| format!("read /proc/stat: {e}"))?;
    let first = content
        .lines()
        .next()
        .ok_or_else(|| "missing cpu line in /proc/stat".to_string())?;
    let mut parts = first.split_whitespace();
    let tag = parts.next().unwrap_or_default();
    if tag != "cpu" {
        return Err("invalid /proc/stat cpu line".to_string());
    }
    let mut sum = 0u64;
    for p in parts {
        sum = sum.saturating_add(p.parse::<u64>().map_err(|e| format!("parse /proc/stat: {e}"))?);
    }
    Ok(sum)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[allow(dead_code)]
fn _parse_latest_frames(_v: &[AggregatedFrame]) -> usize {
    0
}
