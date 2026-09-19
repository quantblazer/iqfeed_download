use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};

use crate::model::{Bar, Timeframe};

/// Sort ascending by timestamp and drop duplicate timestamps (last one wins).
pub fn normalize(bars: Vec<Bar>) -> Vec<Bar> {
    let map: BTreeMap<NaiveDateTime, Bar> = bars.into_iter().map(|b| (b.ts, b)).collect();
    map.into_values().collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub added: usize,
    /// Timestamps present in both sets whose values differ (the new row wins).
    pub changed: usize,
}

pub fn merge(existing: Vec<Bar>, fresh: Vec<Bar>) -> (Vec<Bar>, MergeStats) {
    let mut map: BTreeMap<NaiveDateTime, Bar> = existing.into_iter().map(|b| (b.ts, b)).collect();
    let mut stats = MergeStats::default();
    for bar in fresh {
        match map.insert(bar.ts, bar.clone()) {
            None => stats.added += 1,
            Some(old) if old != bar => stats.changed += 1,
            Some(_) => {}
        }
    }
    (map.into_values().collect(), stats)
}

/// A refresh must keep at least this fraction of the existing rows, otherwise the file is
/// left alone (guards against a partial IQFeed response overwriting good history).
pub const MIN_KEEP_RATIO: f64 = 0.9;

pub fn check_not_shrunk(existing_rows: usize, new_rows: usize) -> Result<()> {
    if existing_rows > 0 && (new_rows as f64) < existing_rows as f64 * MIN_KEEP_RATIO {
        bail!(
            "refusing to replace the existing file: the new download has {new_rows} rows but the file has {existing_rows} \
             (less than {:.0}% kept); existing file left untouched, rerun with --allow-shrink if this is expected",
            MIN_KEEP_RATIO * 100.0
        );
    }
    Ok(())
}

/// True when the fresh bars at or before `last` match the existing ones, i.e. no
/// back-adjustment happened since the file was written. Needs at least one shared bar.
pub fn overlap_consistent(existing: &[Bar], fresh: &[Bar], last: NaiveDateTime) -> bool {
    let index: HashMap<NaiveDateTime, &Bar> = existing.iter().map(|b| (b.ts, b)).collect();
    let mut compared = 0;
    for f in fresh.iter().filter(|b| b.ts <= last) {
        if let Some(e) = index.get(&f.ts) {
            compared += 1;
            if !prices_close(e, f) {
                return false;
            }
        }
    }
    compared > 0
}

fn prices_close(a: &Bar, b: &Bar) -> bool {
    let close = |x: f64, y: f64| (x - y).abs() <= 1e-9 * x.abs().max(1.0);
    close(a.open, b.open) && close(a.high, b.high) && close(a.low, b.low) && close(a.close, b.close)
}

pub fn read_bars(path: &Path, tf: Timeframe) -> Result<Vec<Bar>> {
    let mut rdr = csv::ReaderBuilder::new()
        .from_path(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let expected = tf.columns();
    let header = rdr.headers()?.clone();
    if header.iter().ne(expected.iter().copied()) {
        bail!(
            "{} has unexpected columns [{}], expected [{}]",
            path.display(),
            header.iter().collect::<Vec<_>>().join(","),
            expected.join(",")
        );
    }
    let mut bars = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec?;
        let bar = parse_record(&rec, tf)
            .with_context(|| format!("{} row {}", path.display(), i + 2))?;
        bars.push(bar);
    }
    Ok(bars)
}

fn parse_record(rec: &csv::StringRecord, tf: Timeframe) -> Result<Bar> {
    let get = |i: usize| rec.get(i).context("missing column");
    let ts = match tf {
        Timeframe::Daily => NaiveDate::parse_from_str(get(0)?, tf.timestamp_format())?
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid"),
        Timeframe::Hourly => NaiveDateTime::parse_from_str(get(0)?, tf.timestamp_format())?,
    };
    Ok(Bar {
        ts,
        open: get(1)?.parse()?,
        high: get(2)?.parse()?,
        low: get(3)?.parse()?,
        close: get(4)?.parse()?,
        volume: get(5)?.parse()?,
        // Empty for daily bars derived from hourly data (no open interest available).
        open_interest: match (tf.has_open_interest(), get(6)?) {
            (true, s) if !s.is_empty() => Some(s.parse()?),
            _ => None,
        },
    })
}

/// Write via `<file>.tmp`, verify, then rename over the target so a crash never leaves a
/// half-written CSV.
pub fn write_bars(
    path: &Path,
    tf: Timeframe,
    name: &str,
    iqfeed_symbol: &str,
    bars: &[Bar],
) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let tmp = tmp_path(path);
    let result = write_tmp(&tmp, tf, name, iqfeed_symbol, bars).and_then(|()| {
        let written = read_bars(&tmp, tf).context("verifying temporary file")?;
        if written.len() != bars.len() {
            bail!("verification failed: wrote {} rows, read back {}", bars.len(), written.len());
        }
        fs::rename(&tmp, path)
            .with_context(|| format!("cannot replace {}", path.display()))
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn write_tmp(
    tmp: &Path,
    tf: Timeframe,
    name: &str,
    iqfeed_symbol: &str,
    bars: &[Bar],
) -> Result<()> {
    let file = File::create(tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
    let mut wtr = csv::Writer::from_writer(BufWriter::new(file));
    wtr.write_record(tf.columns())?;
    for b in bars {
        let mut rec = vec![
            b.ts.format(tf.timestamp_format()).to_string(),
            b.open.to_string(),
            b.high.to_string(),
            b.low.to_string(),
            b.close.to_string(),
            b.volume.to_string(),
        ];
        if tf.has_open_interest() {
            rec.push(b.open_interest.map(|v| v.to_string()).unwrap_or_default());
        }
        rec.push(iqfeed_symbol.to_string());
        rec.push(name.to_string());
        wtr.write_record(&rec)?;
    }
    let file = wtr
        .into_inner()
        .map_err(|e| anyhow::anyhow!("flush failed: {}", e.error()))?
        .into_inner()
        .map_err(|e| anyhow::anyhow!("flush failed: {}", e.error()))?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(day: u32, close: f64) -> Bar {
        Bar {
            ts: NaiveDate::from_ymd_opt(2024, 1, day).unwrap().and_hms_opt(0, 0, 0).unwrap(),
            open: close - 1.0,
            high: close + 1.0,
            low: close - 2.0,
            close,
            volume: 100,
            open_interest: Some(50),
        }
    }

    #[test]
    fn normalize_sorts_and_dedupes_last_wins() {
        let out = normalize(vec![bar(3, 30.0), bar(1, 10.0), bar(3, 33.0)]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].close, 10.0);
        assert_eq!(out[1].close, 33.0);
    }

    #[test]
    fn merge_new_rows_win_and_are_counted() {
        let (merged, stats) = merge(vec![bar(1, 10.0), bar(2, 20.0)], vec![bar(2, 21.0), bar(3, 30.0)]);
        assert_eq!(merged.iter().map(|b| b.close).collect::<Vec<_>>(), vec![10.0, 21.0, 30.0]);
        assert_eq!(stats, MergeStats { added: 1, changed: 1 });
    }

    #[test]
    fn shrink_guard_allows_growth_and_small_losses_but_not_big_ones() {
        assert!(check_not_shrunk(0, 0).is_ok()); // no existing file
        assert!(check_not_shrunk(100, 100).is_ok());
        assert!(check_not_shrunk(100, 250).is_ok());
        assert!(check_not_shrunk(100, 90).is_ok()); // exactly at the limit
        let err = check_not_shrunk(100, 89).unwrap_err().to_string();
        assert!(err.contains("89") && err.contains("--allow-shrink"), "{err}");
        assert!(check_not_shrunk(100, 0).is_err());
    }

    #[test]
    fn overlap_detects_readjustment() {
        let existing = vec![bar(1, 10.0), bar(2, 20.0)];
        let last = existing[1].ts;
        assert!(overlap_consistent(&existing, &[bar(2, 20.0), bar(3, 30.0)], last));
        // whole history shifted by a roll adjustment
        assert!(!overlap_consistent(&existing, &[bar(2, 25.0), bar(3, 30.0)], last));
        // nothing to compare against
        assert!(!overlap_consistent(&existing, &[bar(3, 30.0)], last));
    }

    #[test]
    fn write_then_read_roundtrip_daily_and_rerun_is_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out").join("ES_back_adjusted_1d.csv");
        let bars = vec![bar(1, 4785.25), bar(2, 4789.75)];
        write_bars(&path, Timeframe::Daily, "ES", "@ES#C", &bars).unwrap();
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.starts_with("timestamp,open,high,low,close,volume,open_interest,iqfeed_symbol,name\n"));
        assert!(first.contains("2024-01-01,4784.25,4786.25,4783.25,4785.25,100,50,@ES#C,ES"));
        assert_eq!(read_bars(&path, Timeframe::Daily).unwrap(), bars);

        write_bars(&path, Timeframe::Daily, "ES", "@ES#C", &bars).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), first);
        assert!(!tmp_path(&path).exists());
    }

    #[test]
    fn daily_without_open_interest_roundtrips_as_empty_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d_1d.csv");
        let mut b = bar(1, 10.0);
        b.open_interest = None;
        write_bars(&path, Timeframe::Daily, "X", "@X#C", &[b.clone()]).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("2024-01-01,9,11,8,10,100,,@X#C,X"));
        assert_eq!(read_bars(&path, Timeframe::Daily).unwrap(), vec![b]);
    }

    #[test]
    fn hourly_has_no_open_interest_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ES_back_adjusted_1h.csv");
        let mut b = bar(1, 10.0);
        b.open_interest = None;
        write_bars(&path, Timeframe::Hourly, "ES", "@ES#C", &[b.clone()]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("timestamp,open,high,low,close,volume,iqfeed_symbol,name\n"));
        assert!(text.contains("2024-01-01 00:00:00,9,11,8,10,100,@ES#C,ES"));
        assert_eq!(read_bars(&path, Timeframe::Hourly).unwrap(), vec![b]);
    }

    #[test]
    fn failed_write_leaves_existing_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x_1d.csv");
        write_bars(&path, Timeframe::Daily, "X", "@X#C", &[bar(1, 10.0)]).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        // A directory squatting on the temp path makes File::create fail.
        fs::create_dir(tmp_path(&path)).unwrap();
        assert!(write_bars(&path, Timeframe::Daily, "X", "@X#C", &[bar(2, 20.0)]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn rejects_files_with_wrong_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.csv");
        fs::write(&path, "a,b,c\n1,2,3\n").unwrap();
        assert!(read_bars(&path, Timeframe::Daily).is_err());
    }
}
