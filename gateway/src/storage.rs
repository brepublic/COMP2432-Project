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
    /// Ensures `base_path` exists on disk, then returns a handle for frame writes.
    ///
    /// # Arguments
    ///
    /// * `base_path` — Directory for `frame_*.json` files (created if missing).
    ///
    /// # Returns
    ///
    /// New [`DataStorage`]. Panics if the directory cannot be created.
    pub fn new(base_path: PathBuf) -> Self {
        std::fs::create_dir_all(&base_path).expect("Failed to create data directory");
        Self { base_path }
    }

    /// Persists one aggregated frame as JSON via write-to-temp, `sync_all`, then `rename`.
    ///
    /// # Arguments
    ///
    /// * `self` — Storage handle.
    /// * `frame` — Aggregated window to serialize; a clone is sent to the dashboard cache.
    ///
    /// # Returns
    ///
    /// `()`. Panics on I/O or serialization errors.
    ///
    /// This guarantees readers never observe partial JSON.
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

        dashboard::record_aggregated_frame(for_dashboard);
    }

}