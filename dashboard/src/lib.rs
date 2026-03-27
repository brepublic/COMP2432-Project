use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use salvo::prelude::*;
use serde::Serialize;

mod models;
use models::{AggregatedFrame, Anomaly, SensorStats};

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

fn render_data_page(title: &str, api_url: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8" />
    <title>{title}</title>
    <style>
        body {{ font-family: Arial, sans-serif; margin: 24px; }}
        pre {{ background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 8px; padding: 12px; white-space: pre-wrap; }}
        .meta {{ color: #555; margin-bottom: 8px; }}
        a {{ color: #0969da; }}
    </style>
</head>
<body>
    <h1>{title}</h1>
    <div class="meta">
        Auto-refresh interval: 1 second.
        <a href="/">Back to home</a>
    </div>
    <pre id="output">Loading...</pre>
    <script>
        async function refresh() {{
            try {{
                const response = await fetch("{api_url}", {{ cache: "no-store" }});
                if (!response.ok) {{
                    throw new Error("HTTP " + response.status);
                }}
                const data = await response.json();
                document.getElementById("output").textContent = JSON.stringify(data, null, 2);
            }} catch (error) {{
                document.getElementById("output").textContent = "Failed to fetch data: " + error;
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
async fn root() -> Text<&'static str> {
    Text::Html(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8" />
    <title>Sensor Dashboard</title>
</head>
<body>
    <h1>Sensor Dashboard</h1>
    <p>Live pages:</p>
    <ul>
        <li><a href="/latest">/latest</a> - latest aggregated frames (auto refresh)</li>
        <li><a href="/stats">/stats</a> - overall statistics (auto refresh)</li>
        <li><a href="/sensor/thermo-1">/sensor/&lt;id&gt;</a> - per-sensor live data (auto refresh)</li>
    </ul>
    <p>JSON APIs:</p>
    <ul>
        <li><code>/api/latest</code></li>
        <li><code>/api/stats</code></li>
        <li><code>/api/sensor/&lt;id&gt;</code></li>
    </ul>
</body>
</html>"#,
    )
}

#[handler]
async fn latest_page() -> Text<String> {
    Text::Html(render_data_page("Latest Aggregated Frames", "/api/latest"))
}

#[handler]
async fn stats_page() -> Text<String> {
    Text::Html(render_data_page("System Statistics", "/api/stats"))
}

#[handler]
async fn sensor_page(req: &mut Request) -> Text<String> {
    let sensor_id = req
        .param::<String>("id")
        .unwrap_or_else(|| "unknown".to_string());
    let title = format!("Sensor Live View: {sensor_id}");
    let api_url = format!("/api/sensor/{sensor_id}");
    Text::Html(render_data_page(&title, &api_url))
}

#[handler]
async fn latest_api() -> Json<Vec<AggregatedFrame>> {
    let mut frames = load_all_frames_on_pool().await;
    frames.sort_by_key(|f| f.window_end);
    let latest: Vec<AggregatedFrame> = frames.into_iter().rev().take(MAX_LATEST_FRAMES).collect();
    Json(latest)
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
    let sensor_router = Router::with_path("sensor").push(Router::with_path("<id>").get(sensor_page));
    let api_router = Router::with_path("api")
        .push(Router::with_path("latest").get(latest_api))
        .push(Router::with_path("stats").get(stats_api))
        .push(Router::with_path("sensor/<id>").get(sensor_api));

    let router = Router::new()
        .get(root)
        .push(Router::with_path("latest").get(latest_page))
        .push(Router::with_path("stats").get(stats_page))
        .push(sensor_router)
        .push(api_router);

    let listener = TcpListener::new(addr).bind().await;
    Server::new(listener).serve(router).await;
}