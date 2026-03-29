// gateway/src/storage.rs

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::RwLock;
use std::path::PathBuf;
use chrono::Local;
use serde_json;

use crate::aggregation::AggregatedFrame;

/// Data storage: writes aggregated frames to disk and provides a read API.
pub struct DataStorage {
    base_path: PathBuf,
    file_lock: RwLock<()>, // Serialize writes; allow concurrent reads.
}

impl DataStorage {
    pub fn new(base_path: PathBuf) -> Self {
        std::fs::create_dir_all(&base_path).expect("Failed to create data directory");
        Self {
            base_path,
            file_lock: RwLock::new(()),
        }
    }

    /// Write one aggregated frame (appended to the current day's file)
    pub fn write(&self, frame: AggregatedFrame) {
        // Acquire a write lock so only one thread writes to the file at a time.
        let _lock = self.file_lock.write().unwrap();

        let date_str = Local::now().format("%Y-%m-%d").to_string();
        let file_path = self.base_path.join(format!("{}.json", date_str));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .expect("Failed to open data file");

        // Serialize the frame as JSON and write it as a single line.
        let json = serde_json::to_string(&frame).expect("Failed to serialize frame");
        writeln!(file, "{}", json).expect("Failed to write data file");
    }

}