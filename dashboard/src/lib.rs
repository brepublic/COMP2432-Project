use std::cmp::Reverse;
use std::collections::{BinaryHeap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use salvo::prelude::*;
use salvo::server::ServerHandle;
use serde::Serialize;
use tokio::sync::Notify;

use common_models::{
    AggregatedFrame, Anomaly, BufferTelemetrySnapshot, SensorBufferStatus, SensorStats,
    ThroughputTelemetrySnapshot,
};
mod resource;

const DATA_DIR: &str = "./data";
const MAX_LATEST_FRAMES: usize = 10;
const MAX_CACHED_FRAMES: usize = 65_536;
const FRAMES_CACHE_TTL_MS: u64 = 5000;
static BUFFER_TELEMETRY: OnceLock<RwLock<Option<BufferTelemetrySnapshot>>> = OnceLock::new();
static THROUGHPUT_TELEMETRY: OnceLock<RwLock<Option<ThroughputTelemetrySnapshot>>> = OnceLock::new();
static FRAMES_CACHE: OnceLock<Mutex<FramesCacheState>> = OnceLock::new();
static SERVER_HANDLE: OnceLock<ServerHandle> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_NOTIFY: OnceLock<Notify> = OnceLock::new();
static INTERNAL_BUFFER_POLICY: OnceLock<InternalBufferPolicy> = OnceLock::new();

#[derive(Clone, Copy)]
struct InternalBufferPolicy {
    usable_capacity: usize,
    near_full_ratio: f64,
}

struct FramesCacheState {
    refreshed_at_ms: u64,
    refreshing: bool,
    frames: Vec<AggregatedFrame>,
    /// Frames written while a disk refresh is in flight (merged when the load finishes).
    pending_pushes: Vec<AggregatedFrame>,
    /// Woken when an in-flight `load_all_frames` completes so waiters retry instead of stale data.
    load_complete: Arc<Notify>,
}

impl Default for FramesCacheState {
    fn default() -> Self {
        Self {
            refreshed_at_ms: 0,
            refreshing: false,
            frames: Vec::new(),
            pending_pushes: Vec::new(),
            load_complete: Arc::new(Notify::new()),
        }
    }
}

fn merge_frame_into_vec(frames: &mut Vec<AggregatedFrame>, frame: AggregatedFrame) {
    frames.retain(|f| f.window_end != frame.window_end || f.frame_id != frame.frame_id);
    frames.push(frame);
    frames.sort_by(|a, b| {
        a.window_end
            .cmp(&b.window_end)
            .then_with(|| a.frame_id.cmp(&b.frame_id))
    });
    if frames.len() > MAX_CACHED_FRAMES {
        let drop = frames.len() - MAX_CACHED_FRAMES;
        frames.drain(0..drop);
    }
}

/// Called from the gateway after each frame is persisted so the dashboard can update without a full disk scan.
pub fn record_aggregated_frame(frame: AggregatedFrame) {
    let notify = {
        let Ok(mut cache) = frames_cache_store().lock() else {
            return;
        };
        if cache.refreshing {
            cache.pending_pushes.push(frame);
            None
        } else {
            merge_frame_into_vec(&mut cache.frames, frame);
            cache.refreshed_at_ms = now_millis();
            Some(cache.load_complete.clone())
        }
    };
    if let Some(n) = notify {
        n.notify_waiters();
    }
}

fn buffer_store() -> &'static RwLock<Option<BufferTelemetrySnapshot>> {
    BUFFER_TELEMETRY.get_or_init(|| RwLock::new(None))
}

fn shutdown_notify() -> &'static Notify {
    SHUTDOWN_NOTIFY.get_or_init(Notify::new)
}

fn throughput_store() -> &'static RwLock<Option<ThroughputTelemetrySnapshot>> {
    THROUGHPUT_TELEMETRY.get_or_init(|| RwLock::new(None))
}

fn frames_cache_store() -> &'static Mutex<FramesCacheState> {
    FRAMES_CACHE.get_or_init(|| Mutex::new(FramesCacheState::default()))
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

/// Must be called before `run` (e.g. from the gateway) so `/api/buffer` uses correct capacity and ratio.
pub fn set_internal_buffer_policy(usable_capacity: usize, near_full_ratio: f64) {
    let _ = INTERNAL_BUFFER_POLICY.set(InternalBufferPolicy {
        usable_capacity,
        near_full_ratio,
    });
}

fn internal_buffer_policy() -> InternalBufferPolicy {
    INTERNAL_BUFFER_POLICY.get().copied().unwrap_or(InternalBufferPolicy {
        usable_capacity: 127,
        near_full_ratio: 0.85,
    })
}

fn merge_buffer_telemetry_with_frames(
    base: BufferTelemetrySnapshot,
    frames: &[AggregatedFrame],
    capacity: usize,
    near_full_ratio: f64,
) -> BufferTelemetrySnapshot {
    let mut disk_peak: HashMap<String, usize> = HashMap::new();
    for frame in frames {
        for (id, &v) in frame.sensor_internal_buffer_max.iter() {
            disk_peak
                .entry(id.clone())
                .and_modify(|m| *m = (*m).max(v))
                .or_insert(v);
        }
    }

    let mut sensor_ids: BTreeSet<String> =
        base.sensors.iter().map(|s| s.sensor_id.clone()).collect();
    for id in disk_peak.keys() {
        sensor_ids.insert(id.clone());
    }

    let mut sensors = Vec::new();
    let mut any_near_full = false;
    let mut any_full = false;
    let mut warnings = Vec::new();

    for sensor_id in sensor_ids {
        let row = base.sensors.iter().find(|s| s.sensor_id == sensor_id);
        let current_len = row.map(|s| s.current_len).unwrap_or(0);
        let reader_peak = row.map(|s| s.peak_len).unwrap_or(0);
        let frame_peak = disk_peak.get(&sensor_id).copied().unwrap_or(0);
        let peak_len = reader_peak.max(frame_peak);

        let current_ratio = if capacity == 0 {
            0.0
        } else {
            current_len as f64 / capacity as f64
        };
        let peak_ratio = if capacity == 0 {
            0.0
        } else {
            peak_len as f64 / capacity as f64
        };

        let full = (current_len >= capacity && capacity > 0) || (peak_len >= capacity && capacity > 0);
        let near_full = !full
            && capacity > 0
            && (current_ratio >= near_full_ratio || peak_ratio >= near_full_ratio);

        if full {
            any_full = true;
            let worst = current_len.max(peak_len);
            warnings.push(format!(
                "Sensor {} internal buffer is FULL ({}/{})",
                sensor_id, worst, capacity
            ));
        } else if near_full {
            any_near_full = true;
            let worst = if current_ratio >= near_full_ratio {
                current_len
            } else {
                peak_len
            };
            warnings.push(format!(
                "Sensor {} internal buffer is near full ({}/{})",
                sensor_id, worst, capacity
            ));
        }

        sensors.push(SensorBufferStatus {
            sensor_id,
            current_len,
            capacity,
            peak_len,
            utilization_ratio: current_ratio,
            peak_utilization_ratio: peak_ratio,
            near_full,
            full,
        });
    }

    sensors.sort_by(|a, b| a.sensor_id.cmp(&b.sensor_id));

    BufferTelemetrySnapshot {
        sensors,
        any_near_full,
        any_full,
        warnings,
    }
}

pub fn set_throughput_telemetry(snapshot: ThroughputTelemetrySnapshot) {
    if let Ok(mut guard) = throughput_store().write() {
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

/// Ordering for sort: newer frames first (higher `window_end`, then higher `frame_id`).
fn cmp_latest_desc(a: &AggregatedFrame, b: &AggregatedFrame) -> std::cmp::Ordering {
    b.window_end
        .cmp(&a.window_end)
        .then_with(|| b.frame_id.cmp(&a.frame_id))
}

/// Same result as sorting all frames with `cmp_latest_desc` and taking the first `MAX_LATEST_FRAMES`,
/// without full sort when `frames.len()` is large.
fn newest_frames_bounded(frames: &[AggregatedFrame]) -> Vec<AggregatedFrame> {
    if frames.is_empty() {
        return Vec::new();
    }
    if frames.len() <= MAX_LATEST_FRAMES {
        let mut v = frames.to_vec();
        v.sort_by(|a, b| cmp_latest_desc(a, b));
        return v;
    }
    let mut heap: BinaryHeap<Reverse<(u64, u64, usize)>> = BinaryHeap::new();
    for (i, f) in frames.iter().enumerate() {
        let key = (f.window_end, f.frame_id);
        if heap.len() < MAX_LATEST_FRAMES {
            heap.push(Reverse((key.0, key.1, i)));
            continue;
        }
        let top = heap.peek().unwrap();
        let Reverse((min_we, min_fid, _)) = top;
        if key.0 > *min_we || (key.0 == *min_we && key.1 > *min_fid) {
            heap.pop();
            heap.push(Reverse((key.0, key.1, i)));
        }
    }
    let mut out: Vec<AggregatedFrame> = heap
        .into_iter()
        .map(|r| frames[r.0.2].clone())
        .collect();
    out.sort_by(|a, b| cmp_latest_desc(a, b));
    out
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

fn apply_disk_refresh_to_cache(refreshed_frames: &mut Vec<AggregatedFrame>) -> Option<Arc<Notify>> {
    let Ok(mut cache) = frames_cache_store().lock() else {
        return None;
    };
    for p in cache.pending_pushes.drain(..) {
        merge_frame_into_vec(refreshed_frames, p);
    }
    cache.frames = refreshed_frames.clone();
    cache.refreshed_at_ms = now_millis();
    cache.refreshing = false;
    Some(cache.load_complete.clone())
}

enum FramesLoadDecision {
    Wait(Arc<Notify>),
    /// First load or cache empty: block until disk read completes.
    SyncLoad,
    /// Stale cache with data: return immediately, refresh on thread pool in background.
    ReturnStaleStartBg(Vec<AggregatedFrame>),
}

async fn load_all_frames_on_pool() -> Vec<AggregatedFrame> {
    loop {
        let decision = {
            let Ok(mut cache) = frames_cache_store().lock() else {
                return Vec::new();
            };
            let age_ms = now_millis().saturating_sub(cache.refreshed_at_ms);
            let cache_fresh = age_ms <= FRAMES_CACHE_TTL_MS;
            if cache_fresh {
                return cache.frames.clone();
            }
            if cache.refreshing {
                if !cache.frames.is_empty() {
                    return cache.frames.clone();
                }
                FramesLoadDecision::Wait(cache.load_complete.clone())
            } else if cache.frames.is_empty() {
                cache.refreshing = true;
                FramesLoadDecision::SyncLoad
            } else {
                cache.refreshing = true;
                let out = cache.frames.clone();
                FramesLoadDecision::ReturnStaleStartBg(out)
            }
        };

        match decision {
            FramesLoadDecision::ReturnStaleStartBg(out) => {
                tokio::spawn(async move {
                    let mut refreshed_frames = tokio::task::spawn_blocking(load_all_frames)
                        .await
                        .unwrap_or_default();
                    if let Some(n) = apply_disk_refresh_to_cache(&mut refreshed_frames) {
                        n.notify_waiters();
                    }
                });
                return out;
            }
            FramesLoadDecision::Wait(notify) => {
                // Avoid lost wakeup: `notify_waiters()` before we `.await` does not wake us (Tokio Notify).
                // Poll `refreshing` periodically so fast loads still unblock waiters.
                loop {
                    {
                        let Ok(cache) = frames_cache_store().lock() else {
                            return Vec::new();
                        };
                        if !cache.refreshing {
                            break;
                        }
                    }
                    tokio::select! {
                        biased;
                        _ = notify.notified() => {}
                        _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                    }
                }
                continue;
            }
            FramesLoadDecision::SyncLoad => {
                let mut refreshed_frames = tokio::task::spawn_blocking(load_all_frames)
                    .await
                    .unwrap_or_default();

                let notify = apply_disk_refresh_to_cache(&mut refreshed_frames);
                if let Some(n) = notify {
                    n.notify_waiters();
                }
                return refreshed_frames;
            }
        }
    }
}

async fn load_latest_frames_for_api() -> Vec<AggregatedFrame> {
    loop {
        let decision = {
            let Ok(mut cache) = frames_cache_store().lock() else {
                return Vec::new();
            };
            let age_ms = now_millis().saturating_sub(cache.refreshed_at_ms);
            let cache_fresh = age_ms <= FRAMES_CACHE_TTL_MS;
            if cache_fresh {
                return newest_frames_bounded(&cache.frames);
            }
            if cache.refreshing {
                if !cache.frames.is_empty() {
                    return newest_frames_bounded(&cache.frames);
                }
                FramesLoadDecision::Wait(cache.load_complete.clone())
            } else if cache.frames.is_empty() {
                cache.refreshing = true;
                FramesLoadDecision::SyncLoad
            } else {
                cache.refreshing = true;
                let out = cache.frames.clone();
                FramesLoadDecision::ReturnStaleStartBg(out)
            }
        };

        match decision {
            FramesLoadDecision::ReturnStaleStartBg(out) => {
                tokio::spawn(async move {
                    let mut refreshed_frames = tokio::task::spawn_blocking(load_all_frames)
                        .await
                        .unwrap_or_default();
                    if let Some(n) = apply_disk_refresh_to_cache(&mut refreshed_frames) {
                        n.notify_waiters();
                    }
                });
                return newest_frames_bounded(&out);
            }
            FramesLoadDecision::Wait(notify) => {
                loop {
                    {
                        let Ok(cache) = frames_cache_store().lock() else {
                            return Vec::new();
                        };
                        if !cache.refreshing {
                            break;
                        }
                    }
                    tokio::select! {
                        biased;
                        _ = notify.notified() => {}
                        _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                    }
                }
                continue;
            }
            FramesLoadDecision::SyncLoad => {
                let mut refreshed_frames = tokio::task::spawn_blocking(load_all_frames)
                    .await
                    .unwrap_or_default();

                let notify = apply_disk_refresh_to_cache(&mut refreshed_frames);
                if let Some(n) = notify {
                    n.notify_waiters();
                }
                return newest_frames_bounded(&refreshed_frames);
            }
        }
    }
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

    // Short polling avoids holding an HTTP/1.1 connection open forever (long-poll would
    // compete with document navigation and page refresh fetches under per-host conn limits).
    async function pollOnce() {
        try {
            const resp = await fetch("/api/shutdown/status", { cache: "no-store" });
            if (!resp.ok) return;
            const data = await resp.json();
            if (data && data.shutting_down) {
                showShutdownNotice(data.message);
            }
        } catch (_) {
            /* tab closing / navigation */
        }
    }
    pollOnce();
    setInterval(pollOnce, 2000);
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
    Json(load_latest_frames_for_api().await)
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
    let empty = BufferTelemetrySnapshot {
        sensors: Vec::new(),
        any_near_full: false,
        any_full: false,
        warnings: Vec::new(),
    };

    let (base, read_poisoned) = match buffer_store().read() {
        Ok(guard) => (guard.clone().unwrap_or(empty.clone()), false),
        Err(_) => (empty, true),
    };

    let frames = load_all_frames_on_pool().await;
    let policy = internal_buffer_policy();
    let mut merged = merge_buffer_telemetry_with_frames(
        base,
        &frames,
        policy.usable_capacity,
        policy.near_full_ratio,
    );

    if read_poisoned {
        merged
            .warnings
            .insert(0, "Failed to read buffer telemetry state".to_string());
    }

    Json(merged)
}

#[handler]
async fn throughput_api() -> Json<ThroughputTelemetrySnapshot> {
    let snapshot = if let Ok(guard) = throughput_store().read() {
        guard.clone().unwrap_or(ThroughputTelemetrySnapshot {
            buffer_len: 0,
            buffer_capacity: 0,
            pushed_total: 0,
            popped_total: 0,
            pushed_per_sec: 0,
            popped_per_sec: 0,
            full_waits_total: 0,
        })
    } else {
        ThroughputTelemetrySnapshot {
            buffer_len: 0,
            buffer_capacity: 0,
            pushed_total: 0,
            popped_total: 0,
            pushed_per_sec: 0,
            popped_per_sec: 0,
            full_waits_total: 0,
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
async fn shutdown_status_api() -> Json<ShutdownWatchResponse> {
    let shutting_down = SHUTTING_DOWN.load(Ordering::SeqCst);
    Json(ShutdownWatchResponse {
        shutting_down,
        message: if shutting_down {
            "Dashboard is shutting down. Program has stopped.".to_string()
        } else {
            String::new()
        },
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
    let api_router = Router::with_path("api")
        .push(Router::with_path("latest").get(latest_api))
        .push(Router::with_path("stats").get(stats_api))
        .push(Router::with_path("buffer").get(buffer_api))
        .push(Router::with_path("throughput").get(throughput_api))
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
                .push(Router::with_path("shutdown/status").get(shutdown_status_api))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_frame(frame_id: u64, window_end: u64) -> AggregatedFrame {
        AggregatedFrame {
            frame_id,
            window_start: 0,
            window_end,
            sensor_stats: HashMap::new(),
            anomalies: Vec::new(),
            sensor_internal_buffer_max: HashMap::new(),
        }
    }

    /// Reference: full sort + take, matching pre-optimization `newest_frames` behavior.
    fn newest_frames_naive(frames: &[AggregatedFrame]) -> Vec<AggregatedFrame> {
        let mut v = frames.to_vec();
        v.sort_by(|a, b| cmp_latest_desc(a, b));
        v.into_iter().take(MAX_LATEST_FRAMES).collect()
    }

    fn assert_same_latest(a: &[AggregatedFrame], b: &[AggregatedFrame]) {
        assert_eq!(a.len(), b.len(), "len mismatch");
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.frame_id, y.frame_id);
            assert_eq!(x.window_end, y.window_end);
        }
    }

    #[test]
    fn newest_frames_bounded_empty() {
        let frames: Vec<AggregatedFrame> = vec![];
        assert!(newest_frames_bounded(&frames).is_empty());
        assert!(newest_frames_naive(&frames).is_empty());
    }

    #[test]
    fn newest_frames_bounded_fewer_than_cap() {
        let frames = vec![make_frame(1, 100), make_frame(2, 200)];
        assert_same_latest(&newest_frames_bounded(&frames), &newest_frames_naive(&frames));
    }

    #[test]
    fn newest_frames_bounded_exactly_cap() {
        let frames: Vec<_> = (0..10_u64).map(|i| make_frame(i, i * 10)).collect();
        assert_same_latest(&newest_frames_bounded(&frames), &newest_frames_naive(&frames));
    }

    #[test]
    fn newest_frames_bounded_more_than_cap_ties_window_end() {
        let mut frames = vec![];
        for id in 0..25_u64 {
            frames.push(make_frame(id, 500));
        }
        assert_same_latest(&newest_frames_bounded(&frames), &newest_frames_naive(&frames));
    }

    #[test]
    fn newest_frames_bounded_mixed_keys() {
        let frames = vec![
            make_frame(0, 10),
            make_frame(5, 100),
            make_frame(3, 100),
            make_frame(9, 50),
            make_frame(1, 200),
            make_frame(2, 200),
            make_frame(7, 150),
            make_frame(4, 150),
            make_frame(8, 150),
            make_frame(6, 150),
            make_frame(10, 149),
            make_frame(11, 151),
        ];
        assert_same_latest(&newest_frames_bounded(&frames), &newest_frames_naive(&frames));
    }

    #[test]
    fn newest_frames_bounded_pseudo_random() {
        let mut frames = Vec::new();
        let mut we: u64 = 1;
        let mut fid: u64 = 0;
        for _ in 0..200 {
            we = we.wrapping_mul(6364136223846793005).wrapping_add(1);
            fid = fid.wrapping_add(we % 7 + 1);
            frames.push(make_frame(fid, we % 10_000));
        }
        assert_same_latest(&newest_frames_bounded(&frames), &newest_frames_naive(&frames));
    }
}