// gateway/src/storage.rs

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::aggregation::AggregatedFrame;

/// Data storage: writes aggregated frames to disk and provides a read API.
pub struct DataStorage {
    base_path: PathBuf,
}

impl DataStorage {
    pub fn new(base_path: PathBuf) -> Self {
        std::fs::create_dir_all(&base_path).expect("Failed to create data directory");
        Self { base_path }
    }

    /// Write one aggregated frame as an atomic file update.
    ///
    /// We write to a temp file, call `sync_all()`, then atomically `rename()` to the final name.
    /// This guarantees the web server never reads partially written content.
    ///
    /// After the disk write completes, the frame is also pushed into the dashboard's in-memory
    /// cache via [`dashboard::record_aggregated_frame`].  This is the key fix for the dashboard
    /// update latency: rather than scanning all JSON files on every `/api/latest` request
    /// (an O(N-files) disk scan that grows to 3-4 s as data accumulates), the cache is kept
    /// up to date in real time.  Because the cache's `refreshed_at_ms` timestamp is bumped on
    /// every push, the `/api/latest` handler always finds a fresh cache and returns the latest
    /// ten frames from memory without any disk I/O.
    pub fn write(&self, frame: AggregatedFrame) {
        let for_dashboard = frame.clone();
        let final_name = format!("frame_{}_{}.json", frame.window_end, frame.frame_id);
        let final_path = self.base_path.join(&final_name);

        let tmp_name = format!(".{}.tmp", final_name);
        let tmp_path = self.base_path.join(tmp_name);

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .expect("Failed to create temp data file");

        let json = serde_json::to_string(&frame).expect("Failed to serialize frame");
        writeln!(file, "{}", json).expect("Failed to write data file");
        file.sync_all().expect("Failed to sync temp file");

        drop(file);

        // Atomic on POSIX when src and dst are on the same filesystem.
        fs::rename(&tmp_path, &final_path).expect("Failed to atomically rename data file");

        // Push the frame into the dashboard's in-memory cache so that the next `/api/latest`
        // request can be served entirely from memory instead of re-scanning the data directory.
        dashboard::record_aggregated_frame(for_dashboard);
    }

}