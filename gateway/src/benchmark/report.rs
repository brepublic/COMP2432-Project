use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::benchmark::{ScenarioConfig, Thresholds};
use crate::benchmark::collector::{HttpLoadMetrics, SamplingResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub scenario_id: String,
    pub measured_samples: usize,
    pub overflow_events: usize,
    pub near_full_samples: usize,
    pub near_full_ratio: f64,
    pub peak_sensor_utilization: f64,
    pub frame_tps: f64,
    pub pipeline_latency_avg_ms: f64,
    pub pipeline_latency_p95_ms: f64,
    pub cpu_avg_pct: f64,
    pub cpu_peak_pct: f64,
    pub http_success: u64,
    pub http_fail: u64,
    pub http_error_rate: f64,
    pub http_rps: f64,
    pub http_latency_p50_ms: f64,
    pub http_latency_p95_ms: f64,
    pub http_latency_p99_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub actual: String,
    pub target: String,
}

pub fn build_summary(
    scenario: &ScenarioConfig,
    sampling: &SamplingResult,
    http_metrics_path: PathBuf,
) -> Result<ScenarioSummary, String> {
    let measured: Vec<_> = sampling
        .samples
        .iter()
        .filter(|r| r.phase == "measure")
        .cloned()
        .collect();
    let measured_samples = measured.len();
    let overflow_events = measured.iter().filter(|r| r.any_full).count();
    let near_full_samples = measured.iter().filter(|r| r.any_near_full).count();
    let near_full_ratio = ratio(near_full_samples as f64, measured_samples as f64);
    let peak_sensor_utilization = measured
        .iter()
        .map(|r| r.peak_sensor_utilization)
        .fold(0.0, f64::max);

    let frame_tps = if let (Some(first), Some(last)) = (measured.first(), measured.last()) {
        let dt = (last.ts_ms.saturating_sub(first.ts_ms)).max(1);
        let frames = last.total_frames.saturating_sub(first.total_frames);
        (frames as f64 * 1000.0) / dt as f64
    } else {
        0.0
    };

    let pipeline_latencies: Vec<u64> = measured.iter().map(|r| r.pipeline_latency_ms).collect();
    let pipeline_latency_avg_ms = average_u64(&pipeline_latencies);
    let pipeline_latency_p95_ms = percentile_u64(&pipeline_latencies, 95.0);

    let cpu_values: Vec<f64> = measured.iter().map(|r| r.cpu_pct).collect();
    let cpu_avg_pct = average_f64(&cpu_values);
    let cpu_peak_pct = cpu_values.iter().cloned().fold(0.0f64, f64::max);

    let http: HttpLoadMetrics = if http_metrics_path.exists() {
        serde_json::from_str(
            &std::fs::read_to_string(&http_metrics_path)
                .map_err(|e| format!("read http metrics {}: {e}", http_metrics_path.display()))?,
        )
        .map_err(|e| format!("parse http metrics {}: {e}", http_metrics_path.display()))?
    } else {
        HttpLoadMetrics::default()
    };

    let total_http = http.success + http.fail;
    let http_error_rate = ratio(http.fail as f64, total_http as f64);
    let http_rps = ratio(http.success as f64, scenario.measure_secs as f64);
    let lat_ms: Vec<f64> = http.latencies_us.iter().map(|v| *v as f64 / 1000.0).collect();

    Ok(ScenarioSummary {
        scenario_id: scenario.id.clone(),
        measured_samples,
        overflow_events,
        near_full_samples,
        near_full_ratio,
        peak_sensor_utilization,
        frame_tps,
        pipeline_latency_avg_ms,
        pipeline_latency_p95_ms,
        cpu_avg_pct,
        cpu_peak_pct,
        http_success: http.success,
        http_fail: http.fail,
        http_error_rate,
        http_rps,
        http_latency_p50_ms: percentile_f64(&lat_ms, 50.0),
        http_latency_p95_ms: percentile_f64(&lat_ms, 95.0),
        http_latency_p99_ms: percentile_f64(&lat_ms, 99.0),
    })
}

pub fn build_checks(scenario: &ScenarioConfig, summary: &ScenarioSummary) -> Vec<CheckResult> {
    let t: &Thresholds = &scenario.thresholds;
    vec![
        CheckResult {
            name: "zero_data_loss_overflow".to_string(),
            passed: summary.overflow_events == 0,
            actual: summary.overflow_events.to_string(),
            target: "0".to_string(),
        },
        CheckResult {
            name: "near_full_ratio".to_string(),
            passed: summary.near_full_ratio <= t.max_near_full_ratio,
            actual: format!("{:.4}", summary.near_full_ratio),
            target: format!("<= {:.4}", t.max_near_full_ratio),
        },
        CheckResult {
            name: "http_error_rate".to_string(),
            passed: summary.http_error_rate <= t.max_http_error_rate,
            actual: format!("{:.4}", summary.http_error_rate),
            target: format!("<= {:.4}", t.max_http_error_rate),
        },
        CheckResult {
            name: "http_latency_p95_ms".to_string(),
            passed: summary.http_latency_p95_ms <= t.max_api_p95_ms,
            actual: format!("{:.2}", summary.http_latency_p95_ms),
            target: format!("<= {:.2}", t.max_api_p95_ms),
        },
        CheckResult {
            name: "http_latency_p99_ms".to_string(),
            passed: summary.http_latency_p99_ms <= t.max_api_p99_ms,
            actual: format!("{:.2}", summary.http_latency_p99_ms),
            target: format!("<= {:.2}", t.max_api_p99_ms),
        },
        CheckResult {
            name: "cpu_avg_pct".to_string(),
            passed: summary.cpu_avg_pct <= t.max_cpu_avg_pct,
            actual: format!("{:.2}", summary.cpu_avg_pct),
            target: format!("<= {:.2}", t.max_cpu_avg_pct),
        },
        CheckResult {
            name: "cpu_peak_pct".to_string(),
            passed: summary.cpu_peak_pct <= t.max_cpu_peak_pct,
            actual: format!("{:.2}", summary.cpu_peak_pct),
            target: format!("<= {:.2}", t.max_cpu_peak_pct),
        },
    ]
}

pub fn write_markdown_digest(
    summary: &ScenarioSummary,
    checks: &[CheckResult],
    out_path: PathBuf,
    scenario_id: &str,
) -> Result<(), String> {
    let passed = checks.iter().filter(|c| c.passed).count();
    let total = checks.len();
    let mut md = String::new();
    md.push_str(&format!("# Benchmark Digest: {scenario_id}\n\n"));
    md.push_str(&format!("- Checks passed: {passed}/{total}\n"));
    md.push_str(&format!("- Overflow events: {}\n", summary.overflow_events));
    md.push_str(&format!("- Near-full ratio: {:.4}\n", summary.near_full_ratio));
    md.push_str(&format!("- Frame throughput (tps): {:.2}\n", summary.frame_tps));
    md.push_str(&format!("- Pipeline latency avg/p95 (ms): {:.2}/{:.2}\n", summary.pipeline_latency_avg_ms, summary.pipeline_latency_p95_ms));
    md.push_str(&format!("- HTTP success/fail: {}/{}\n", summary.http_success, summary.http_fail));
    md.push_str(&format!("- HTTP latency p50/p95/p99 (ms): {:.2}/{:.2}/{:.2}\n", summary.http_latency_p50_ms, summary.http_latency_p95_ms, summary.http_latency_p99_ms));
    md.push_str(&format!("- CPU avg/peak (%): {:.2}/{:.2}\n\n", summary.cpu_avg_pct, summary.cpu_peak_pct));
    md.push_str("## Check Details\n");
    for c in checks {
        md.push_str(&format!(
            "- {}: {} (actual {}, target {})\n",
            c.name,
            if c.passed { "PASS" } else { "FAIL" },
            c.actual,
            c.target
        ));
    }
    std::fs::write(&out_path, md).map_err(|e| format!("write {}: {e}", out_path.display()))
}

fn ratio(a: f64, b: f64) -> f64 {
    if b <= 0.0 {
        0.0
    } else {
        a / b
    }
}

fn average_u64(v: &[u64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().sum::<u64>() as f64 / v.len() as f64
}

fn average_f64(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().sum::<f64>() / v.len() as f64
}

fn percentile_u64(v: &[u64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let idx = (((p / 100.0) * ((s.len() - 1) as f64)).round() as usize).min(s.len() - 1);
    s[idx] as f64
}

fn percentile_f64(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (((p / 100.0) * ((s.len() - 1) as f64)).round() as usize).min(s.len() - 1);
    s[idx]
}
