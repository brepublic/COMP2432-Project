use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use chrono::Local;
use serde_json;

use crate::aggregation::AggregatedFrame;

pub struct DataStorage {
    base_path: PathBuf,
}

impl DataStorage {
    pub fn new(base_path: PathBuf) -> Self {
        std::fs::create_dir_all(&base_path).expect("dir error");
        Self {base_path,}
    }

    pub fn write(&self, frame: AggregatedFrame) {
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

    pub fn read(&self) -> Vec<AggregatedFrame>{
        Vec::new()
    }
}
