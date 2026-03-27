use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use salvo::prelude::*;
use serde::Serialize;

mod models;
use models::{AggregatedFrame, Anomaly, SensorStats};
mod resource;

const DATA_DIR: &str = "./data";
const MAX_LATEST_FRAMES: usize = 10;

#[derive(Debug, Serialize)]
struct SystemStats {
    total_frames: usize,
    total_anomalies: usize,
    latest_frame_id: Option<u64>,
    latest_window_end: Option<u64>,
    sensors_seen: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SensorLiveView {
    sensor_id: String,
    latest_frame_id: Option<u64>,
    latest_window_end: Option<u64>,
    stats: Option<SensorStats>,
    anomalies: Vec<Anomaly>,
}

fn load_all_frames() -> Vec<AggregatedFrame> {
    let base = PathBuf::from(DATA_DIR);
    let entries = match std::fs::read_dir(&base) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let mut frames = Vec::new();
    for path in paths {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(frame) = serde_json::from_str::<AggregatedFrame>(&line) {
                frames.push(frame);
            }
        }
    }

    frames
}

async fn load_all_frames_on_pool() -> Vec<AggregatedFrame> {
    tokio::task::spawn_blocking(load_all_frames)
        .await
        .unwrap_or_default()
}

fn load_template_sync(name: &str) -> String {
    let rel_path = format!("templates/{}", name);
    match resource::locate_resource(&rel_path) {
        Some(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|e| format!("<!-- Failed to read template {rel_path}: {e} -->")),
        None => format!("<!-- Template not found: {rel_path} -->"),
    }
}

async fn load_template(name: &'static str) -> String {
    // Template reading is blocking IO; keep it off async executor threads.
    tokio::task::spawn_blocking(move || load_template_sync(name))
        .await
        .unwrap_or_else(|_| format!("<!-- Failed to load template: {name} -->"))
}

#[allow(dead_code)]
fn render_latest_page() -> String {
    r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8" />
    <title>Latest Aggregated Frames</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 24px; background: #f8fafc; color: #0f172a; }
        .header { display: flex; justify-content: space-between; align-items: baseline; }
        .meta { color: #475569; }
        .card { background: #fff; border: 1px solid #d0d7de; border-radius: 10px; padding: 12px; margin-bottom: 12px; }
        .muted { color: #64748b; }
        table { width: 100%; border-collapse: collapse; margin-top: 8px; }
        th, td { border-bottom: 1px solid #e2e8f0; text-align: left; padding: 8px; font-size: 14px; }
        .badge { display: inline-block; padding: 2px 8px; border-radius: 999px; background: #fee2e2; color: #991b1b; font-size: 12px; }
        a { color: #0969da; }
    </style>
</head>
<body>
    <div class="header">
        <h1>Latest Aggregated Frames</h1>
        <a href="/">Back to home</a>
    </div>
    <div class="meta">Auto refresh: every 1 second</div>
    <div id="container"></div>
    <script>
        function renderFrame(frame) {
            const sensors = Object.keys(frame.sensor_stats || {});
            const rows = sensors.map((id) => {
                const s = frame.sensor_stats[id];
                return `<tr>
                    <td>${id}</td>
                    <td>${s.count}</td>
                    <td>${s.avg.toFixed(3)}</td>
                    <td>${s.min.toFixed(3)}</td>
                    <td>${s.max.toFixed(3)}</td>
                    <td>${s.stddev.toFixed(3)}</td>
                </tr>`;
            }).join("");
            return `
                <div class="card">
                    <div><strong>Frame #${frame.frame_id}</strong> <span class="muted">window_end=${frame.window_end}</span></div>
                    <div style="margin-top:6px;">Anomalies: <span class="badge">${(frame.anomalies || []).length}</span></div>
                    <table>
                        <thead><tr><th>Sensor</th><th>Count</th><th>Avg</th><th>Min</th><th>Max</th><th>Stddev</th></tr></thead>
                        <tbody>${rows || `<tr><td colspan="6" class="muted">No sensor stats</td></tr>`}</tbody>
                    </table>
                </div>
            `;
        }
        async function refresh() {
            const container = document.getElementById("container");
            try {
                const response = await fetch("/api/latest", { cache: "no-store" });
                if (!response.ok) throw new Error("HTTP " + response.status);
                const frames = await response.json();
                if (!Array.isArray(frames) || frames.length === 0) {
                    container.innerHTML = '<div class="card muted">No aggregated frames yet.</div>';
                    return;
                }
                container.innerHTML = frames.map(renderFrame).join("");
            } catch (error) {
                container.innerHTML = `<div class="card">Failed to fetch data: ${error}</div>`;
            }
        }
        refresh();
        setInterval(refresh, 1000);
    </script>
</body>
</html>"#.to_string()
}

#[allow(dead_code)]
fn render_stats_page() -> String {
    r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8" />
    <title>System Statistics</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 24px; background: #f8fafc; color: #0f172a; }
        .header { display: flex; justify-content: space-between; align-items: baseline; }
        .meta { color: #475569; }
        .grid { display: grid; grid-template-columns: repeat(4, minmax(140px, 1fr)); gap: 10px; margin-top: 12px; }
        .card { background: #fff; border: 1px solid #d0d7de; border-radius: 10px; padding: 12px; }
        .label { color: #64748b; font-size: 12px; }
        .value { font-size: 22px; font-weight: 700; margin-top: 6px; }
        .sensor-list { margin-top: 14px; display: flex; gap: 8px; flex-wrap: wrap; }
        .chip { background: #e2e8f0; border-radius: 999px; padding: 4px 10px; font-size: 13px; }
        a { color: #0969da; }
    </style>
</head>
<body>
    <div class="header">
        <h1>System Statistics</h1>
        <a href="/">Back to home</a>
    </div>
    <div class="meta">Auto refresh: every 1 second</div>
    <div class="grid">
        <div class="card"><div class="label">Total Frames</div><div class="value" id="total-frames">-</div></div>
        <div class="card"><div class="label">Total Anomalies</div><div class="value" id="total-anomalies">-</div></div>
        <div class="card"><div class="label">Latest Frame ID</div><div class="value" id="latest-frame-id">-</div></div>
        <div class="card"><div class="label">Latest Window End</div><div class="value" id="latest-window-end">-</div></div>
    </div>
    <h3 style="margin-top:16px;">Sensors Seen</h3>
    <div id="sensor-list" class="sensor-list"></div>
    <script>
        async function refresh() {
            try {
                const response = await fetch("/api/stats", { cache: "no-store" });
                if (!response.ok) throw new Error("HTTP " + response.status);
                const data = await response.json();
                document.getElementById("total-frames").textContent = data.total_frames ?? "-";
                document.getElementById("total-anomalies").textContent = data.total_anomalies ?? "-";
                document.getElementById("latest-frame-id").textContent = data.latest_frame_id ?? "-";
                document.getElementById("latest-window-end").textContent = data.latest_window_end ?? "-";
                const list = document.getElementById("sensor-list");
                const sensors = Array.isArray(data.sensors_seen) ? data.sensors_seen : [];
                list.innerHTML = sensors.length
                    ? sensors.map(s => `<span class="chip">${s}</span>`).join("")
                    : `<span class="chip">No sensors yet</span>`;
            } catch (error) {
                document.getElementById("sensor-list").innerHTML = `<span class="chip">Failed to fetch data: ${error}</span>`;
            }
        }
        refresh();
        setInterval(refresh, 1000);
    </script>
</body>
</html>"#.to_string()
}

#[allow(dead_code)]
fn render_sensor_page(sensor_id: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8" />
    <title>Sensor Live View: {sensor_id}</title>
    <style>
        body {{ font-family: Arial, sans-serif; margin: 24px; background: #f8fafc; color: #0f172a; }}
        .header {{ display: flex; justify-content: space-between; align-items: baseline; }}
        .meta {{ color: #475569; }}
        .card {{ background: #fff; border: 1px solid #d0d7de; border-radius: 10px; padding: 12px; margin-bottom: 12px; }}
        .grid {{ display: grid; grid-template-columns: repeat(4, minmax(120px, 1fr)); gap: 10px; }}
        .label {{ color: #64748b; font-size: 12px; }}
        .value {{ font-size: 20px; font-weight: 700; margin-top: 6px; }}
        table {{ width: 100%; border-collapse: collapse; }}
        th, td {{ border-bottom: 1px solid #e2e8f0; text-align: left; padding: 8px; font-size: 14px; }}
        .ok {{ color: #166534; }}
        .warn {{ color: #991b1b; }}
        a {{ color: #0969da; }}
    </style>
</head>
<body>
    <div class="header">
        <h1>Sensor Live View: {sensor_id}</h1>
        <a href="/">Back to home</a>
    </div>
    <div class="meta">Auto refresh: every 1 second</div>
    <div class="grid">
        <div class="card"><div class="label">Latest Frame ID</div><div class="value" id="frame-id">-</div></div>
        <div class="card"><div class="label">Latest Window End</div><div class="value" id="window-end">-</div></div>
        <div class="card"><div class="label">Sample Count</div><div class="value" id="count">-</div></div>
        <div class="card"><div class="label">Anomaly Count</div><div class="value" id="anomaly-count">-</div></div>
    </div>
    <div class="card">
        <h3>Statistics</h3>
        <table>
            <thead><tr><th>Avg</th><th>Min</th><th>Max</th><th>Stddev</th></tr></thead>
            <tbody><tr><td id="avg">-</td><td id="min">-</td><td id="max">-</td><td id="stddev">-</td></tr></tbody>
        </table>
    </div>
    <div class="card">
        <h3>Anomalies</h3>
        <table>
            <thead><tr><th>Type</th><th>Severity</th><th>Description</th></tr></thead>
            <tbody id="anomaly-rows"><tr><td colspan="3" class="ok">No anomalies</td></tr></tbody>
        </table>
    </div>
    <script>
        async function refresh() {{
            try {{
                const response = await fetch("/api/sensor/{sensor_id}", {{ cache: "no-store" }});
                if (!response.ok) throw new Error("HTTP " + response.status);
                const data = await response.json();
                document.getElementById("frame-id").textContent = data.latest_frame_id ?? "-";
                document.getElementById("window-end").textContent = data.latest_window_end ?? "-";
                document.getElementById("anomaly-count").textContent = (data.anomalies || []).length;
                if (data.stats) {{
                    document.getElementById("count").textContent = data.stats.count;
                    document.getElementById("avg").textContent = data.stats.avg.toFixed(3);
                    document.getElementById("min").textContent = data.stats.min.toFixed(3);
                    document.getElementById("max").textContent = data.stats.max.toFixed(3);
                    document.getElementById("stddev").textContent = data.stats.stddev.toFixed(3);
                }} else {{
                    document.getElementById("count").textContent = "-";
                    document.getElementById("avg").textContent = "-";
                    document.getElementById("min").textContent = "-";
                    document.getElementById("max").textContent = "-";
                    document.getElementById("stddev").textContent = "-";
                }}
                const rows = (data.anomalies || []).map(a => `
                    <tr class="warn">
                        <td>${{a.anomaly_type}}</td>
                        <td>${{Number(a.severity).toFixed(3)}}</td>
                        <td>${{a.description}}</td>
                    </tr>
                `).join("");
                document.getElementById("anomaly-rows").innerHTML =
                    rows || '<tr><td colspan="3" class="ok">No anomalies</td></tr>';
            }} catch (error) {{
                document.getElementById("anomaly-rows").innerHTML =
                    `<tr><td colspan="3" class="warn">Failed to fetch data: ${{error}}</td></tr>`;
            }}
        }}
        refresh();
        setInterval(refresh, 1000);
    </script>
</body>
</html>"#
    )
}

#[handler]
async fn root() -> Text<String> {
    let html = load_template("index.html").await;
    Text::Html(html)
}

#[handler]
async fn latest_page() -> Text<String> {
    let html = load_template("latest.html").await;
    Text::Html(html)
}

#[handler]
async fn stats_page() -> Text<String> {
    let html = load_template("stats.html").await;
    Text::Html(html)
}

#[handler]
async fn sensor_page(req: &mut Request) -> Text<String> {
    let _ = req;
    let html = load_template("sensor.html").await;
    Text::Html(html)
}

#[handler]
async fn sensor_index_page() -> Text<String> {
    let html = load_template("sensor_index.html").await;
    Text::Html(html)
}

#[handler]
async fn latest_api() -> Json<Vec<AggregatedFrame>> {
    let mut frames = load_all_frames_on_pool().await;
    frames.sort_by(|a, b| {
        b.window_end
            .cmp(&a.window_end)
            .then_with(|| b.frame_id.cmp(&a.frame_id))
    });
    Json(frames.into_iter().take(MAX_LATEST_FRAMES).collect())
}

#[handler]
async fn stats_api() -> Json<SystemStats> {
    let frames = load_all_frames_on_pool().await;
    let total_frames = frames.len();
    let total_anomalies = frames.iter().map(|f| f.anomalies.len()).sum();

    let mut sensors_seen = BTreeSet::new();
    for frame in &frames {
        for key in frame.sensor_stats.keys() {
            sensors_seen.insert(key.clone());
        }
    }

    let latest = frames.iter().max_by_key(|f| f.window_end);
    Json(SystemStats {
        total_frames,
        total_anomalies,
        latest_frame_id: latest.map(|f| f.frame_id),
        latest_window_end: latest.map(|f| f.window_end),
        sensors_seen: sensors_seen.into_iter().collect(),
    })
}

#[handler]
async fn sensor_api(req: &mut Request) -> Json<SensorLiveView> {
    let sensor_id = req
        .param::<String>("id")
        .unwrap_or_else(|| "unknown".to_string());
    let frames = load_all_frames_on_pool().await;

    let mut selected: Option<&AggregatedFrame> = None;
    for frame in &frames {
        if frame.sensor_stats.contains_key(&sensor_id) {
            if let Some(prev) = selected {
                if frame.window_end > prev.window_end {
                    selected = Some(frame);
                }
            } else {
                selected = Some(frame);
            }
        }
    }

    if let Some(frame) = selected {
        let stats = frame.sensor_stats.get(&sensor_id).cloned();
        let anomalies = frame
            .anomalies
            .iter()
            .filter(|a| a.sensor_id == sensor_id)
            .cloned()
            .collect();
        Json(SensorLiveView {
            sensor_id,
            latest_frame_id: Some(frame.frame_id),
            latest_window_end: Some(frame.window_end),
            stats,
            anomalies,
        })
    } else {
        Json(SensorLiveView {
            sensor_id,
            latest_frame_id: None,
            latest_window_end: None,
            stats: None,
            anomalies: Vec::new(),
        })
    }
}

pub async fn run(addr: &'static str) {
    let api_router = Router::with_path("api")
        .push(Router::with_path("latest").get(latest_api))
        .push(Router::with_path("stats").get(stats_api))
        .push(Router::with_path("sensor/<id>").get(sensor_api));

    let router = Router::new()
        .get(root)
        .push(Router::with_path("latest").get(latest_page))
        .push(Router::with_path("stats").get(stats_page))
        .push(Router::with_path("sensor").get(sensor_index_page))
        .push(Router::with_path("sensor/<id>").get(sensor_page))
        .push(api_router);

    let listener = TcpListener::new(addr).bind().await;
    Server::new(listener).serve(router).await;
}