use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use salvo::prelude::*;
use serde::Serialize;

use common_models::{AggregatedFrame, Anomaly, SensorStats};
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
        .push(
            Router::with_path("sensor").push(Router::with_path("{id}").get(sensor_api)),
        );

    let sensor_router = Router::with_path("sensor")
        .get(sensor_index_page)
        .push(Router::with_path("{id}").get(sensor_page));

    let router = Router::new()
        .get(root)
        .push(Router::with_path("latest").get(latest_page))
        .push(Router::with_path("stats").get(stats_page))
        .push(sensor_router)
        .push(api_router);

    let listener = TcpListener::new(addr).bind().await;
    Server::new(listener).serve(router).await;
}