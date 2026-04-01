pub mod collector;
pub mod report;
pub mod scenarios;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::benchmark::collector::{run_http_load, run_sampling, HttpLoadHandle, SamplingConfig};
use crate::benchmark::report::{build_checks, build_summary, write_markdown_digest};
use crate::benchmark::scenarios::default_scenarios;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BenchmarkConfig {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub scenarios: Vec<ScenarioConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GlobalConfig {
    #[serde(default = "default_gateway_bin")]
    pub gateway_bin: String,
    #[serde(default = "default_dashboard_host")]
    pub dashboard_host: String,
    #[serde(default = "default_base_port")]
    pub base_port: u16,
    #[serde(default = "default_results_dir")]
    pub results_dir: String,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScenarioConfig {
    pub id: String,
    pub description: String,
    #[serde(default = "default_warmup_secs")]
    pub warmup_secs: u64,
    #[serde(default = "default_measure_secs")]
    pub measure_secs: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_buffer_capacity")]
    pub buffer_capacity: usize,
    pub sensors: SensorScenarioConfig,
    #[serde(default)]
    pub http_load: Option<HttpLoadConfig>,
    #[serde(default)]
    pub thresholds: Thresholds,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SensorScenarioConfig {
    pub thermo_count: usize,
    pub thermo_rates_per_sec: String,
    pub accel_count: usize,
    pub accel_rates_per_sec: String,
    pub force_count: usize,
    pub force_rates_per_sec: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpLoadConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_http_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_http_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_http_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Thresholds {
    #[serde(default = "default_max_error_rate")]
    pub max_http_error_rate: f64,
    #[serde(default = "default_max_api_p95_ms")]
    pub max_api_p95_ms: f64,
    #[serde(default = "default_max_api_p99_ms")]
    pub max_api_p99_ms: f64,
    #[serde(default = "default_max_cpu_avg_pct")]
    pub max_cpu_avg_pct: f64,
    #[serde(default = "default_max_cpu_peak_pct")]
    pub max_cpu_peak_pct: f64,
    #[serde(default = "default_max_near_full_ratio")]
    pub max_near_full_ratio: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            max_http_error_rate: default_max_error_rate(),
            max_api_p95_ms: default_max_api_p95_ms(),
            max_api_p99_ms: default_max_api_p99_ms(),
            max_cpu_avg_pct: default_max_cpu_avg_pct(),
            max_cpu_peak_pct: default_max_cpu_peak_pct(),
            max_near_full_ratio: default_max_near_full_ratio(),
        }
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            gateway_bin: default_gateway_bin(),
            dashboard_host: default_dashboard_host(),
            base_port: default_base_port(),
            results_dir: default_results_dir(),
            poll_interval_ms: default_poll_interval_ms(),
        }
    }
}

pub fn run(config_path: &Path) -> Result<(), String> {
    let cfg_text = fs::read_to_string(config_path)
        .map_err(|e| format!("failed to read benchmark config {}: {e}", config_path.display()))?;
    let mut cfg: BenchmarkConfig =
        toml::from_str(&cfg_text).map_err(|e| format!("failed to parse benchmark config: {e}"))?;
    if cfg.scenarios.is_empty() {
        cfg.scenarios = default_scenarios();
    }
    run_config(cfg)
}

pub fn run_config(cfg: BenchmarkConfig) -> Result<(), String> {
    let run_id = now_millis();
    let run_dir = PathBuf::from(&cfg.global.results_dir).join(format!("run_{run_id}"));
    fs::create_dir_all(&run_dir)
        .map_err(|e| format!("failed to create run output dir {}: {e}", run_dir.display()))?;

    println!("Benchmark output: {}", run_dir.display());

    for (idx, scenario) in cfg.scenarios.iter().enumerate() {
        println!("=== Running scenario {} ({}) ===", scenario.id, scenario.description);
        let scenario_dir = run_dir.join(&scenario.id);
        fs::create_dir_all(&scenario_dir)
            .map_err(|e| format!("failed to create scenario dir {}: {e}", scenario_dir.display()))?;

        let port = cfg.global.base_port.saturating_add(idx as u16);
        let dashboard_addr = format!("{}:{}", cfg.global.dashboard_host, port);
        let scenario_config_path = scenario_dir.join("gateway_config.toml");
        fs::write(
            &scenario_config_path,
            render_gateway_config(scenario, &dashboard_addr),
        )
        .map_err(|e| format!("failed to write scenario gateway config: {e}"))?;

        let stdout_path = scenario_dir.join("gateway.stdout.log");
        let stderr_path = scenario_dir.join("gateway.stderr.log");
        let mut child = std::process::Command::new(&cfg.global.gateway_bin)
            .env("GATEWAY_CONFIG", &scenario_config_path)
            .stdout(
                std::fs::File::create(&stdout_path)
                    .map_err(|e| format!("failed to create stdout log: {e}"))?,
            )
            .stderr(
                std::fs::File::create(&stderr_path)
                    .map_err(|e| format!("failed to create stderr log: {e}"))?,
            )
            .spawn()
            .map_err(|e| format!("failed to spawn gateway process {}: {e}", cfg.global.gateway_bin))?;

        let base_url = format!("http://{dashboard_addr}");
        wait_dashboard_ready(&base_url, Duration::from_secs(30))?;

        let mut http_handle: Option<HttpLoadHandle> =
            if let Some(http_cfg) = scenario.http_load.as_ref().filter(|h| h.enabled) {
                Some(run_http_load(
                    base_url.clone(),
                    http_cfg.endpoint.clone(),
                    http_cfg.concurrency,
                    http_cfg.timeout_ms,
                ))
            } else {
                None
            };

        let sampling = run_sampling(
            SamplingConfig {
                scenario_id: scenario.id.clone(),
                base_url: base_url.clone(),
                warmup_secs: scenario.warmup_secs,
                measure_secs: scenario.measure_secs,
                cooldown_secs: scenario.cooldown_secs,
                poll_interval_ms: cfg.global.poll_interval_ms,
            },
            child.id(),
            scenario_dir.join("samples.csv"),
        )?;

        if let Some(handle) = http_handle.as_mut() {
            let metrics = handle.metrics();
            handle.stop();
            fs::write(
                scenario_dir.join("http_metrics.json"),
                serde_json::to_string_pretty(&metrics).map_err(|e| format!("serialize http metrics: {e}"))?,
            )
            .map_err(|e| format!("write http metrics: {e}"))?;
        }

        // Terminate gateway process after scenario.
        let _ = child.kill();
        let _ = child.wait();

        let summary = build_summary(scenario, &sampling, scenario_dir.join("http_metrics.json"))?;
        let checks = build_checks(scenario, &summary);

        fs::write(
            scenario_dir.join("summary.json"),
            serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?,
        )
        .map_err(|e| format!("write summary: {e}"))?;
        fs::write(
            scenario_dir.join("checks.json"),
            serde_json::to_string_pretty(&checks).map_err(|e| format!("serialize checks: {e}"))?,
        )
        .map_err(|e| format!("write checks: {e}"))?;
        write_markdown_digest(
            &summary,
            &checks,
            scenario_dir.join("benchmark_digest.md"),
            scenario.id.as_str(),
        )?;
    }

    Ok(())
}

fn wait_dashboard_ready(base_url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("failed to build http client: {e}"))?;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client.get(format!("{base_url}/api/stats")).send() {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(format!("dashboard not ready in {}s: {base_url}", timeout.as_secs()))
}

fn render_gateway_config(s: &ScenarioConfig, dashboard_addr: &str) -> String {
    format!(
        "[sensors]\n\
thermo_count = {}\n\
thermo_rates_per_sec = \"{}\"\n\
accel_count = {}\n\
accel_rates_per_sec = \"{}\"\n\
force_count = {}\n\
force_rates_per_sec = \"{}\"\n\n\
[buffer]\n\
capacity = {}\n\n\
[dashboard]\n\
addr = \"{}\"\n",
        s.sensors.thermo_count,
        s.sensors.thermo_rates_per_sec,
        s.sensors.accel_count,
        s.sensors.accel_rates_per_sec,
        s.sensors.force_count,
        s.sensors.force_rates_per_sec,
        s.buffer_capacity,
        dashboard_addr
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn default_gateway_bin() -> String {
    "target/release/gateway".to_string()
}
fn default_dashboard_host() -> String {
    "127.0.0.1".to_string()
}
fn default_base_port() -> u16 {
    5900
}
fn default_results_dir() -> String {
    "bench_results".to_string()
}
fn default_poll_interval_ms() -> u64 {
    1000
}
fn default_warmup_secs() -> u64 {
    20
}
fn default_measure_secs() -> u64 {
    120
}
fn default_cooldown_secs() -> u64 {
    10
}
fn default_buffer_capacity() -> usize {
    5000
}
fn default_http_endpoint() -> String {
    "/api/latest".to_string()
}
fn default_http_concurrency() -> usize {
    1
}
fn default_http_timeout_ms() -> u64 {
    1000
}
fn default_max_error_rate() -> f64 {
    0.01
}
fn default_max_api_p95_ms() -> f64 {
    200.0
}
fn default_max_api_p99_ms() -> f64 {
    500.0
}
fn default_max_cpu_avg_pct() -> f64 {
    75.0
}
fn default_max_cpu_peak_pct() -> f64 {
    90.0
}
fn default_max_near_full_ratio() -> f64 {
    0.01
}
