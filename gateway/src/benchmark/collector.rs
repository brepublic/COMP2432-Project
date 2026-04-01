use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_models::{AggregatedFrame, BufferTelemetrySnapshot, ThroughputTelemetrySnapshot};
use serde::{Deserialize, Serialize};

/// Cap stored latency samples so long stress runs stay bounded in memory (FIFO eviction).
const MAX_HTTP_LATENCY_SAMPLES: usize = 50_000;

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
    /// Max per-sensor instantaneous internal buffer utilization (0–1).
    pub peak_sensor_utilization: f64,
    /// Max per-sensor peak utilization (includes historical / frame-merged peaks from `/api/buffer`).
    pub peak_sensor_peak_utilization: f64,
    pub total_frames: usize,
    pub frame_tps_interval: f64,
    pub latest_window_end: u64,
    pub pipeline_latency_ms: u64,
    pub ingest_readings_per_sec: f64,
    pub ingest_total_pushed: u64,
    pub ingest_total_popped: u64,
    pub processing_readings_per_sec: f64,
    pub processing_total_readings: u64,
    pub serving_http_rps_interval: f64,
    pub serving_http_success_total: u64,
    pub cpu_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingResult {
    pub scenario_id: String,
    pub total_samples: usize,
    pub measured_samples: usize,
    pub samples: Vec<SampleRow>,
    /// Sum of `SensorStats.count` over frames from `/api/range` in the measure-phase window span.
    pub range_processing_readings_total: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpLoadMetrics {
    pub success: u64,
    pub fail: u64,
    #[serde(default)]
    pub latencies_us: VecDeque<u64>,
}

impl Default for HttpLoadMetrics {
    fn default() -> Self {
        Self {
            success: 0,
            fail: 0,
            latencies_us: VecDeque::new(),
        }
    }
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

    pub fn metrics_shared(&self) -> Arc<Mutex<HttpLoadMetrics>> {
        Arc::clone(&self.shared)
    }
}

fn push_latency_sample(buf: &mut VecDeque<u64>, sample: u64) {
    if buf.len() < MAX_HTTP_LATENCY_SAMPLES {
        buf.push_back(sample);
    } else {
        buf.pop_front();
        buf.push_back(sample);
    }
}

/// `paths` must be non-empty. Workers round-robin across paths with an atomic counter.
pub fn run_http_load(
    base_url: String,
    paths: Vec<String>,
    concurrency: usize,
    timeout_ms: u64,
) -> HttpLoadHandle {
    assert!(
        !paths.is_empty(),
        "run_http_load: paths must be non-empty"
    );
    let running = Arc::new(AtomicBool::new(true));
    let shared = Arc::new(Mutex::new(HttpLoadMetrics::default()));
    let paths = Arc::new(paths);
    let rr = Arc::new(AtomicUsize::new(0));

    let mut join_handles = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let running_clone = Arc::clone(&running);
        let shared_clone = Arc::clone(&shared);
        let paths_clone = Arc::clone(&paths);
        let rr_clone = Arc::clone(&rr);
        let base = base_url.clone();
        let handle = thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_millis(timeout_ms))
                .build();
            let Ok(client) = client else {
                return;
            };
            while running_clone.load(Ordering::SeqCst) {
                let idx = rr_clone.fetch_add(1, Ordering::Relaxed);
                let path = paths_clone[idx % paths_clone.len()].trim();
                let rel = if path.starts_with('/') {
                    path.to_string()
                } else {
                    format!("/{path}")
                };
                let url = format!("{}{}", base.trim_end_matches('/'), rel);
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
                        push_latency_sample(&mut g.latencies_us, latency_us);
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
    http_metrics_shared: Option<Arc<Mutex<HttpLoadMetrics>>>,
) -> Result<SamplingResult, String> {
    let mut file = File::create(&csv_path)
        .map_err(|e| format!("failed to create {}: {e}", csv_path.display()))?;
    writeln!(
        file,
        "ts_ms,phase,any_full,any_near_full,peak_sensor_utilization,peak_sensor_peak_utilization,total_frames,frame_tps_interval,latest_window_end,pipeline_latency_ms,ingest_readings_per_sec,ingest_total_pushed,ingest_total_popped,processing_readings_per_sec,processing_total_readings,serving_http_rps_interval,serving_http_success_total,cpu_pct"
    )
    .map_err(|e| format!("failed to write csv header: {e}"))?;

    let poll_interval = Duration::from_millis(cfg.poll_interval_ms.max(200));
    let total_duration = cfg.warmup_secs + cfg.measure_secs + cfg.cooldown_secs;
    let start = Instant::now();
    let mut samples = Vec::new();
    let mut cpu = CpuSampler::new(gateway_pid)?;
    let mut seen_frame_ids = std::collections::HashSet::<u64>::new();
    let mut processing_total_readings = 0u64;
    let mut prev_sample_ts_ms = now_millis();
    let mut prev_total_frames = 0usize;
    let mut prev_http_success = 0u64;

    let mut measure_window_end_min: Option<u64> = None;
    let mut measure_window_end_max: Option<u64> = None;

    // Poll endpoints that may scan `./data` (e.g. `/api/buffer`); keep timeout above worst-case disk load.
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(60))
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

        let buffer: BufferTelemetrySnapshot = fetch_json(&client, &cfg.base_url, "/api/buffer", gateway_pid)?;
        let stats: StatsResponse = fetch_json(&client, &cfg.base_url, "/api/stats", gateway_pid)?;
        let throughput: ThroughputTelemetrySnapshot =
            fetch_json(&client, &cfg.base_url, "/api/throughput", gateway_pid)?;
        let latest_frames: Vec<AggregatedFrame> =
            fetch_json(&client, &cfg.base_url, "/api/latest", gateway_pid)?;

        let peak_sensor_utilization = buffer
            .sensors
            .iter()
            .map(|s| s.utilization_ratio)
            .fold(0.0f64, f64::max);
        let peak_sensor_peak_utilization = buffer
            .sensors
            .iter()
            .map(|s| s.peak_utilization_ratio)
            .fold(0.0f64, f64::max);
        let latest_window_end = stats.latest_window_end.unwrap_or(0);
        if phase == "measure" && latest_window_end > 0 {
            measure_window_end_min = Some(
                measure_window_end_min
                    .map_or(latest_window_end, |m| m.min(latest_window_end)),
            );
            measure_window_end_max = Some(
                measure_window_end_max
                    .map_or(latest_window_end, |m| m.max(latest_window_end)),
            );
        }
        let now_ms = now_millis();
        let dt_ms = now_ms.saturating_sub(prev_sample_ts_ms).max(1);
        prev_sample_ts_ms = now_ms;
        let pipeline_latency_ms = now_ms.saturating_sub(latest_window_end);
        let frame_delta = stats.total_frames.saturating_sub(prev_total_frames);
        prev_total_frames = stats.total_frames;
        let frame_tps_interval = (frame_delta as f64 * 1000.0) / dt_ms as f64;

        let mut new_processing_readings = 0u64;
        for frame in latest_frames {
            if seen_frame_ids.insert(frame.frame_id) {
                let frame_count: u64 = frame
                    .sensor_stats
                    .values()
                    .map(|s| s.count as u64)
                    .sum();
                new_processing_readings = new_processing_readings.saturating_add(frame_count);
            }
        }
        processing_total_readings = processing_total_readings.saturating_add(new_processing_readings);
        let processing_readings_per_sec = (new_processing_readings as f64 * 1000.0) / dt_ms as f64;

        let (serving_http_success_total, serving_http_rps_interval) =
            if let Some(shared) = &http_metrics_shared {
                if let Ok(g) = shared.lock() {
                    let success_total = g.success;
                    let success_delta = success_total.saturating_sub(prev_http_success);
                    prev_http_success = success_total;
                    let rps = (success_delta as f64 * 1000.0) / dt_ms as f64;
                    (success_total, rps)
                } else {
                    (0, 0.0)
                }
            } else {
                (0, 0.0)
            };
        let cpu_pct = cpu.sample().unwrap_or(0.0);

        let row = SampleRow {
            ts_ms: now_ms,
            phase,
            any_full: buffer.any_full,
            any_near_full: buffer.any_near_full,
            peak_sensor_utilization,
            peak_sensor_peak_utilization,
            total_frames: stats.total_frames,
            frame_tps_interval,
            latest_window_end,
            pipeline_latency_ms,
            ingest_readings_per_sec: throughput.pushed_per_sec as f64,
            ingest_total_pushed: throughput.pushed_total,
            ingest_total_popped: throughput.popped_total,
            processing_readings_per_sec,
            processing_total_readings,
            serving_http_rps_interval,
            serving_http_success_total,
            cpu_pct,
        };
        writeln!(
            file,
            "{},{},{},{},{:.6},{:.6},{},{:.4},{},{},{:.4},{},{},{:.4},{},{:.4},{},{:.2}",
            row.ts_ms,
            row.phase,
            row.any_full,
            row.any_near_full,
            row.peak_sensor_utilization,
            row.peak_sensor_peak_utilization,
            row.total_frames,
            row.frame_tps_interval,
            row.latest_window_end,
            row.pipeline_latency_ms,
            row.ingest_readings_per_sec,
            row.ingest_total_pushed,
            row.ingest_total_popped,
            row.processing_readings_per_sec,
            row.processing_total_readings,
            row.serving_http_rps_interval,
            row.serving_http_success_total,
            row.cpu_pct
        )
        .map_err(|e| format!("failed to append sample row: {e}"))?;
        samples.push(row);
        thread::sleep(poll_interval);
    }

    let range_processing_readings_total =
        fetch_range_reading_total(&cfg.base_url, measure_window_end_min, measure_window_end_max);

    let measured_samples = samples.iter().filter(|r| r.phase == "measure").count();
    Ok(SamplingResult {
        scenario_id: cfg.scenario_id,
        total_samples: samples.len(),
        measured_samples,
        samples,
        range_processing_readings_total,
    })
}

fn fetch_range_reading_total(
    base_url: &str,
    measure_min_we: Option<u64>,
    measure_max_we: Option<u64>,
) -> Option<u64> {
    let (Some(from), Some(to)) = (measure_min_we, measure_max_we) else {
        return None;
    };
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .ok()?;
    let url = format!(
        "{}/api/range?from={}&to={}",
        base_url.trim_end_matches('/'),
        from,
        to
    );
    let response = client.get(&url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let frames: Vec<AggregatedFrame> = response.json().ok()?;
    let total: u64 = frames
        .iter()
        .map(|f| {
            f.sensor_stats
                .values()
                .map(|s| s.count as u64)
                .sum::<u64>()
        })
        .sum();
    Some(total)
}

fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::blocking::Client,
    base_url: &str,
    endpoint: &str,
    _gateway_pid: u32,
) -> Result<T, String> {
    let url = format!("{base_url}{endpoint}");
    let response = match client.get(&url).send() {
        Ok(resp) => resp,
        Err(e) => {
            return Err(format!("sampling {endpoint} failed: {e}"));
        }
    };

    match response.json::<T>() {
        Ok(parsed) => Ok(parsed),
        Err(e) => Err(format!("parse {endpoint} failed: {e}")),
    }
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
