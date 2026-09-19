use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ReportRow {
    pub name: String,
    pub iqfeed_symbol: String,
    pub timeframe: String,
    pub status: String,
    pub rows: usize,
    pub path: String,
    pub error: String,
    pub start_time: String,
    pub end_time: String,
}

pub fn write_report(dir: &Path, date: NaiveDate, rows: &[ReportRow]) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(format!("download_{}.csv", date.format("%Y-%m-%d")));
    let mut wtr = csv::Writer::from_path(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    for row in rows {
        wtr.serialize(row)?;
    }
    wtr.flush()?;
    Ok(path)
}
