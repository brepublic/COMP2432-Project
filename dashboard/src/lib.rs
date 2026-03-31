use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use salvo::prelude::*;
use salvo::server::ServerHandle;
use serde::Serialize;
use tokio::sync::Notify;

use common_models::{AggregatedFrame, Anomaly, BufferTelemetrySnapshot, SensorStats};
mod resource;

const DATA_DIR: &str = "./data";
const MAX_LATEST_FRAMES: usize = 10;
static BUFFER_TELEMETRY: OnceLock<RwLock<Option<BufferTelemetrySnapshot>>> = OnceLock::new();
static SERVER_HANDLE: OnceLock<ServerHandle> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_NOTIFY: OnceLock<Notify> = OnceLock::new();
static RUN_START_MILLIS: OnceLock<u64> = OnceLock::new();

fn buffer_store() -> &'static RwLock<Option<BufferTelemetrySnapshot>> {
    BUFFER_TELEMETRY.get_or_init(|| RwLock::new(None))
}

fn shutdown_notify() -> &'static Notify {
    SHUTDOWN_NOTIFY.get_or_init(Notify::new)
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn set_buffer_telemetry(snapshot: BufferTelemetrySnapshot) {
    if let Ok(mut guard) = buffer_store().write() {
        *guard = Some(snapshot);
    }
}

pub fn request_shutdown() {
    // Ensure shutdown path only runs once.
    if SHUTTING_DOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    // Wake browser clients first so they can show a prompt before process exits.
    shutdown_notify().notify_waiters();
    if let Some(handle) = SERVER_HANDLE.get() {
        let handle = handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            handle.stop_graceful(Some(Duration::from_secs(2)));
        });
    }
}

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

fn newest_frames(mut frames: Vec<AggregatedFrame>) -> Vec<AggregatedFrame> {
    frames.sort_by(|a, b| {
        b.window_end
            .cmp(&a.window_end)
            .then_with(|| b.frame_id.cmp(&a.frame_id))
    });
    frames.into_iter().take(MAX_LATEST_FRAMES).collect()
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

    if let Some(run_start) = RUN_START_MILLIS.get() {
        frames
            .into_iter()
            .filter(|frame| frame.window_end >= *run_start)
            .collect()
    } else {
        frames
    }
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

fn inject_shutdown_watcher(mut html: String) -> String {
    let script = r#"
<script>
(() => {
    const bannerId = "dashboard-shutdown-banner";
    let announced = false;

    function showShutdownNotice(message) {
        if (announced) return;
        announced = true;
        const text = message || "Dashboard is shutting down. Program has stopped.";
        alert(text);

        if (!document.getElementById(bannerId)) {
            const banner = document.createElement("div");
            banner.id = bannerId;
            banner.textContent = text;
            banner.style.cssText = [
                "position:fixed",
                "left:0",
                "right:0",
                "bottom:0",
                "padding:12px 16px",
                "background:#b91c1c",
                "color:#fff",
                "font-size:14px",
                "font-weight:600",
                "text-align:center",
                "z-index:2147483647",
                "box-shadow:0 -6px 20px rgba(0,0,0,0.25)"
            ].join(";");
            document.body.appendChild(banner);
        }
    }

    fetch("/api/shutdown/watch", { cache: "no-store" })
        .then((resp) => (resp.ok ? resp.json() : Promise.reject(new Error("HTTP " + resp.status))))
        .then((data) => {
            if (data && data.shutting_down) {
                showShutdownNotice(data.message);
            }
        })
        .catch(() => {
            // Ignore network errors (tab closed/manual refresh/etc.).
        });
})();
</script>
"#;

    if let Some(pos) = html.rfind("</body>") {
        html.insert_str(pos, script);
        html
    } else {
        html.push_str(script);
        html
    }
}

async fn load_template(name: &'static str) -> String {
    // Template reading is blocking IO; keep it off async executor threads.
    let html = tokio::task::spawn_blocking(move || load_template_sync(name))
        .await
        .unwrap_or_else(|_| format!("<!-- Failed to load template: {name} -->"));
    inject_shutdown_watcher(html)
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
async fn shutdown_page() -> Text<String> {
    let html = load_template("shutdown.html").await;
    Text::Html(html)
}

#[handler]
async fn latest_api() -> Json<Vec<AggregatedFrame>> {
    let frames = load_all_frames_on_pool().await;
    Json(newest_frames(frames))
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
        let anomalies = frames
            .iter()
            .flat_map(|f| f.anomalies.iter())
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

#[handler]
async fn range_api(req: &mut Request) -> Json<Vec<AggregatedFrame>> {
    // Query params: from/to are window timestamps in millis.
    let from = req.query::<u64>("from").unwrap_or(0);
    let to = req.query::<u64>("to").unwrap_or(u64::MAX);

    let mut frames = load_all_frames_on_pool().await;
    frames.retain(|f| f.window_end >= from && f.window_end <= to);

    // Return newest-first for convenience.
    frames.sort_by(|a, b| b.window_end.cmp(&a.window_end).then_with(|| b.frame_id.cmp(&a.frame_id)));
    Json(frames)
}

#[handler]
async fn buffer_api() -> Json<BufferTelemetrySnapshot> {
    let snapshot = if let Ok(guard) = buffer_store().read() {
        guard.clone().unwrap_or(BufferTelemetrySnapshot {
            sensors: Vec::new(),
            any_near_full: false,
            any_full: false,
            warnings: Vec::new(),
        })
    } else {
        BufferTelemetrySnapshot {
            sensors: Vec::new(),
            any_near_full: false,
            any_full: false,
            warnings: vec!["Failed to read buffer telemetry state".to_string()],
        }
    };
    Json(snapshot)
}

#[derive(Debug, Serialize)]
struct ShutdownResponse {
    ok: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct ShutdownWatchResponse {
    shutting_down: bool,
    message: String,
}

#[handler]
async fn shutdown_api() -> Json<ShutdownResponse> {
    request_shutdown();
    Json(ShutdownResponse {
        ok: true,
        message: "Dashboard is shutting down. Program has stopped.".to_string(),
    })
}

#[handler]
async fn shutdown_watch_api() -> Json<ShutdownWatchResponse> {
    if !SHUTTING_DOWN.load(Ordering::SeqCst) {
        shutdown_notify().notified().await;
    }
    Json(ShutdownWatchResponse {
        shutting_down: true,
        message: "Dashboard is shutting down. Program has stopped.".to_string(),
    })
}

pub async fn run(addr: &'static str) {
    let _ = RUN_START_MILLIS.set(now_millis());
    let api_router = Router::with_path("api")
        .push(Router::with_path("latest").get(latest_api))
        .push(Router::with_path("stats").get(stats_api))
        .push(Router::with_path("buffer").get(buffer_api))
        .push(Router::with_path("range").get(range_api))
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
        .push(Router::with_path("shutdown").get(shutdown_page))
        .push(sensor_router)
        .push(
            api_router
                .push(Router::with_path("shutdown").post(shutdown_api))
                .push(Router::with_path("shutdown/watch").get(shutdown_watch_api)),
        );

    let listener = TcpListener::new(addr).bind().await;
    let server = Server::new(listener);
    let _ = SERVER_HANDLE.set(server.handle());
    // If shutdown was requested before server handle initialization,
    // stop immediately after handle is installed.
    if SHUTTING_DOWN.load(Ordering::SeqCst) {
        if let Some(handle) = SERVER_HANDLE.get() {
            handle.stop_graceful(Some(Duration::from_secs(2)));
        }
    }
    server.serve(router).await;
}