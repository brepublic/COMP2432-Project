use std::fs::{OpenOptions, File};
use std::io::{Write, BufRead, BufReader};
use std::sync::RwLock;
use std::path::PathBuf;
use chrono::Local;
use serde_json;

use crate::aggregation::AggregatedFrame;

pub struct DataStorage {
    base_path: PathBuf,
    file_lock: RwLock<()>, 
}

impl DataStorage {
    pub fn new(base_path: PathBuf) -> Self {
        std::fs::create_dir_all(&base_path).expect("dir error");
        Self {
            base_path,
            file_lock: RwLock::new(()),
        }
    }

    pub fn write(&self, frame: AggregatedFrame) {
        let _lock = self.file_lock.write().unwrap();

        let date_str = Local::now().format("%Y-%m-%d").to_string();
        let file_path = self.base_path.join(format!("{}.json", date_str));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .expect("open error");

        let json = serde_json::to_string(&frame).expect("serde error");
        writeln!(file, "{}", json).expect("write error");
    }

    pub fn read_all(&self) -> Vec<AggregatedFrame> {
        let _lock = self.file_lock.read().unwrap();

        let mut frames = Vec::new();
        let entries = std::fs::read_dir(&self.base_path).expect("read dir error");

        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let file = File::open(&path).expect("open error");
                let reader = BufReader::new(file);
                for line in reader.lines() {
                    let line = line.unwrap();
                    if let Ok(frame) = serde_json::from_str::<AggregatedFrame>(&line) {
                        frames.push(frame);
                    }
                }
            }
        }
        frames
    }
}