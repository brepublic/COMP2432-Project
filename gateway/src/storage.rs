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

    /// Where the web server should read files from.
    pub fn path(&self) -> PathBuf {
        self.base_path.clone()
    }

    /// Equivalent of flush: each `write()` syncs to disk before rename.
    pub fn flush(&self) {
        // Intentionally empty: `write()` calls `sync_all()` on the temp file.
    }

    /// Write one aggregated frame as an atomic file update.
    ///
    /// We write to a temp file, call `sync_all()`, then atomically `rename()` to the final name.
    /// This guarantees the web server never reads partially written content.
    pub fn write(&self, frame: AggregatedFrame) {
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
    }

}