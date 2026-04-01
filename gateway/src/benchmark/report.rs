use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::benchmark::{expected_ingest_per_sec, ScenarioConfig, Thresholds};
use crate::benchmark::collector::{HttpLoadMetrics, SamplingResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub scenario_id: String,
    pub measured_samples: usize,
    pub overflow_events: usize,
    pub near_full_samples: usize,
    pub near_full_ratio: f64,
    pub peak_sensor_utilization: f64,
    pub peak_sensor_peak_utilization: f64,
    pub ingest_throughput_avg_rps: f64,
    pub ingest_throughput_p95_rps: f64,
    pub processing_throughput_avg_rps: f64,
    pub processing_throughput_p95_rps: f64,
    pub serving_throughput_avg_rps: f64,
    pub serving_throughput_p95_rps: f64,
    pub frame_tps_avg: f64,
    pub frame_tps_stddev: f64,
    pub ingest_total_pushed_end: u64,
    pub ingest_total_popped_end: u64,
    /// Cumulative count from `/api/latest`-based sampling (may undercount when the latest window is small).
    pub processing_total_readings_end: u64,
    /// Sum of reading counts from `/api/range` over the measure-phase window span (when available).
    pub processing_total_readings_range: Option<u64>,
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
    /// Expected readings produced over `measure_secs` at configured sensor rates.
    pub ingest_expected_measure_total: u64,
    /// `ingest_total_pushed` delta between first and last measure-phase samples.
    pub ingest_delta_measure: u64,
    /// `ingest_delta_measure / ingest_expected_measure_total` (0 if expected is 0).
    pub ingest_measure_ratio: f64,
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
    let peak_sensor_peak_utilization = measured
        .iter()
        .map(|r| r.peak_sensor_peak_utilization)
        .fold(0.0, f64::max);

    let ingest_series: Vec<f64> = measured.iter().map(|r| r.ingest_readings_per_sec).collect();
    let processing_series: Vec<f64> = measured
        .iter()
        .map(|r| r.processing_readings_per_sec)
        .collect();
    let serving_series: Vec<f64> = measured
        .iter()
        .map(|r| r.serving_http_rps_interval)
        .collect();
    let frame_tps_series: Vec<f64> = measured.iter().map(|r| r.frame_tps_interval).collect();

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
    let lat_ms: Vec<f64> = http
        .latencies_us
        .iter()
        .map(|v| *v as f64 / 1000.0)
        .collect();

    let rps = expected_ingest_per_sec(&scenario.sensors);
    let ingest_expected_measure_total = scenario.measure_secs.saturating_mul(rps);
    let (ingest_delta_measure, ingest_measure_ratio) =
        if let (Some(first), Some(last)) = (measured.first(), measured.last()) {
            let delta = last
                .ingest_total_pushed
                .saturating_sub(first.ingest_total_pushed);
            let ratio_val = ratio(delta as f64, ingest_expected_measure_total as f64);
            (delta, ratio_val)
        } else {
            (0, 0.0)
        };

    Ok(ScenarioSummary {
        scenario_id: scenario.id.clone(),
        measured_samples,
        overflow_events,
        near_full_samples,
        near_full_ratio,
        peak_sensor_utilization,
        peak_sensor_peak_utilization,
        ingest_throughput_avg_rps: average_f64(&ingest_series),
        ingest_throughput_p95_rps: percentile_f64(&ingest_series, 95.0),
        processing_throughput_avg_rps: average_f64(&processing_series),
        processing_throughput_p95_rps: percentile_f64(&processing_series, 95.0),
        serving_throughput_avg_rps: average_f64(&serving_series),
        serving_throughput_p95_rps: percentile_f64(&serving_series, 95.0),
        frame_tps_avg: average_f64(&frame_tps_series),
        frame_tps_stddev: stddev_f64(&frame_tps_series),
        ingest_total_pushed_end: measured.last().map(|r| r.ingest_total_pushed).unwrap_or(0),
        ingest_total_popped_end: measured.last().map(|r| r.ingest_total_popped).unwrap_or(0),
        processing_total_readings_end: measured
            .last()
            .map(|r| r.processing_total_readings)
            .unwrap_or(0),
        processing_total_readings_range: sampling.range_processing_readings_total,
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
        ingest_expected_measure_total,
        ingest_delta_measure,
        ingest_measure_ratio,
    })
}

pub fn build_checks(scenario: &ScenarioConfig, summary: &ScenarioSummary) -> Vec<CheckResult> {
    let t: &Thresholds = &scenario.thresholds;
    let processing_total_for_ingest_check = summary
        .processing_total_readings_range
        .unwrap_or(summary.processing_total_readings_end);

    let mut checks = vec![
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
        CheckResult {
            name: "processing_not_exceed_ingest".to_string(),
            passed: processing_total_for_ingest_check <= summary.ingest_total_pushed_end,
            actual: format!(
                "processing_total={} ingest_total={}",
                processing_total_for_ingest_check, summary.ingest_total_pushed_end
            ),
            target: "processing_total <= ingest_total (uses /api/range sum when available)".to_string(),
        },
    ];

    if t.min_ingest_ratio > 0.0 && summary.ingest_expected_measure_total > 0 {
        checks.push(CheckResult {
            name: "ingest_meets_expected".to_string(),
            passed: summary.ingest_measure_ratio >= t.min_ingest_ratio,
            actual: format!(
                "ratio {:.4} (delta {} / expected {})",
                summary.ingest_measure_ratio,
                summary.ingest_delta_measure,
                summary.ingest_expected_measure_total
            ),
            target: format!(">= {:.4}", t.min_ingest_ratio),
        });
    }

    checks
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
    md.push_str(&format!(
        "- Peak sensor utilization (instant / peak-historical max): {:.4} / {:.4}\n",
        summary.peak_sensor_utilization, summary.peak_sensor_peak_utilization
    ));
    md.push_str(&format!(
        "- Ingest throughput avg/p95 (readings/s): {:.2}/{:.2}\n",
        summary.ingest_throughput_avg_rps, summary.ingest_throughput_p95_rps
    ));
    md.push_str(&format!(
        "- Processing throughput avg/p95 (readings/s): {:.2}/{:.2}\n",
        summary.processing_throughput_avg_rps, summary.processing_throughput_p95_rps
    ));
    md.push_str("(Processing series from `/api/latest` window; may undercount under heavy load.)\n");
    md.push_str(&format!(
        "- Serving throughput avg/p95 (success rps): {:.2}/{:.2}\n",
        summary.serving_throughput_avg_rps, summary.serving_throughput_p95_rps
    ));
    md.push_str(&format!(
        "- Frame TPS health avg/stddev: {:.3}/{:.3}\n",
        summary.frame_tps_avg, summary.frame_tps_stddev
    ));
    md.push_str(&format!(
        "- Totals (ingest pushed/popped, processing latest-accum, processing range): {}/{}/{}/{:?}\n",
        summary.ingest_total_pushed_end,
        summary.ingest_total_popped_end,
        summary.processing_total_readings_end,
        summary.processing_total_readings_range
    ));
    md.push_str(&format!(
        "- Ingest budget (measure phase): expected {}, delta {}, ratio {:.4}\n",
        summary.ingest_expected_measure_total,
        summary.ingest_delta_measure,
        summary.ingest_measure_ratio
    ));
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

fn stddev_f64(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mean = average_f64(v);
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / v.len() as f64;
    var.sqrt()
}
