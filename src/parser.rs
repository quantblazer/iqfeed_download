//! Turns IQFeed response lines into [`Bar`]s. Field order is fixed per timeframe:
//!
//! - daily (`HDT`):   `timestamp, High, Low, Open, Close, PeriodVolume, OpenInterest`
//! - hourly (`HIT`):  `timestamp, High, Low, Open, Close, TotalVolume, PeriodVolume, NumTrades`
//!
//! Note IQFeed sends High/Low *before* Open/Close.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};

use crate::model::{Bar, Timeframe};

pub const END_MARKER: &str = "!ENDMSG!";

#[derive(Debug, PartialEq)]
pub enum Line<'a> {
    /// A data row with the request id stripped.
    Data(&'a str),
    /// End-of-response marker.
    End,
    /// `E,<message>` from IQFeed.
    Error(String),
    /// Anything else (system/keep-alive messages, other request ids).
    Other,
}

pub fn classify<'a>(line: &'a str, req_id: &str) -> Line<'a> {
    // IQFeed terminates every message with a trailing comma ("DL1,!ENDMSG!,").
    let line = line.trim_end_matches(',');
    let rest = line
        .strip_prefix(req_id)
        .and_then(|r| r.strip_prefix(','));
    match rest {
        Some(END_MARKER) => Line::End,
        Some(r) => match r.strip_prefix("E,") {
            Some(msg) => Line::Error(msg.to_string()),
            // History rows are tagged "LH," after the request id.
            None => Line::Data(r.strip_prefix("LH,").unwrap_or(r)),
        },
        None => match line.strip_prefix("E,") {
            Some(msg) => Line::Error(msg.to_string()),
            None => Line::Other,
        },
    }
}

pub struct ParseOutcome {
    pub bars: Vec<Bar>,
    pub malformed: usize,
    pub first_error: Option<String>,
}

pub fn parse_lines<S: AsRef<str>>(tf: Timeframe, lines: &[S]) -> ParseOutcome {
    let mut out = ParseOutcome {
        bars: Vec::with_capacity(lines.len()),
        malformed: 0,
        first_error: None,
    };
    for line in lines {
        match parse_row(tf, line.as_ref()) {
            Ok(bar) => out.bars.push(bar),
            Err(e) => {
                out.malformed += 1;
                out.first_error
                    .get_or_insert_with(|| format!("{e:#} in row '{}'", line.as_ref()));
            }
        }
    }
    out
}

pub fn parse_row(tf: Timeframe, row: &str) -> Result<Bar> {
    let f: Vec<&str> = row.split(',').map(str::trim).collect();
    let (min_fields, volume_idx) = match tf {
        Timeframe::Daily => (7, 5),
        Timeframe::Hourly => (7, 6),
    };
    if f.len() < min_fields {
        bail!("expected at least {min_fields} fields, got {}", f.len());
    }
    Ok(Bar {
        ts: parse_timestamp(f[0])?,
        high: price(f[1], "high")?,
        low: price(f[2], "low")?,
        open: price(f[3], "open")?,
        close: price(f[4], "close")?,
        volume: count(f[volume_idx], "volume")?,
        open_interest: match tf {
            Timeframe::Daily => Some(count(f[6], "open interest")?),
            Timeframe::Hourly => None,
        },
    })
}

fn parse_timestamp(s: &str) -> Result<NaiveDateTime> {
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(dt);
        }
    }
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| anyhow!("bad timestamp '{s}'"))?;
    Ok(date.and_hms_opt(0, 0, 0).expect("midnight is valid"))
}

fn price(s: &str, what: &str) -> Result<f64> {
    s.parse::<f64>().with_context(|| format!("bad {what} '{s}'"))
}

fn count(s: &str, what: &str) -> Result<u64> {
    if s.is_empty() {
        return Ok(0);
    }
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let f = s.parse::<f64>().with_context(|| format!("bad {what} '{s}'"))?;
    if f < 0.0 || !f.is_finite() {
        bail!("bad {what} '{s}'");
    }
    Ok(f.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_lines() {
        assert_eq!(classify("DL1,2024-01-02 00:00:00,1,2", "DL1"), Line::Data("2024-01-02 00:00:00,1,2"));
        assert_eq!(classify("DL1,!ENDMSG!", "DL1"), Line::End);
        assert_eq!(classify("DL1,!ENDMSG!,", "DL1"), Line::End);
        assert_eq!(
            classify("DL1,E,Unknown Server Error code 0.,", "DL1"),
            Line::Error("Unknown Server Error code 0.".into())
        );
        assert_eq!(classify("DL1,E,!NO_DATA!", "DL1"), Line::Error("!NO_DATA!".into()));
        assert_eq!(classify("E,!SYNTAX_ERROR!", "DL1"), Line::Error("!SYNTAX_ERROR!".into()));
        // A different request id (or one that merely shares a prefix) is not ours.
        assert_eq!(classify("DL10,1,2,3", "DL1"), Line::Other);
        assert_eq!(classify("T,20240102 093000", "DL1"), Line::Other);
    }

    /// Captured from a live IQFeed 6.2 session (`HIX,@ES#,3600,3,1,R2`, 2026-09-19).
    #[test]
    fn parses_real_hourly_lines_from_iqfeed() {
        let raw = [
            "R2,LH,2026-09-18 14:00:00,7715.75,7693.00,7694.75,7710.00,984267,109656,0,",
            "R2,LH,2026-09-18 15:00:00,7718.25,7704.25,7709.75,7712.25,1212774,228383,0,",
            "R2,LH,2026-09-18 16:00:00,7729.25,7711.50,7712.25,7725.00,1288568,75783,0,",
        ];
        let rows: Vec<&str> = raw
            .iter()
            .map(|l| match classify(l, "R2") {
                Line::Data(d) => d,
                other => panic!("expected data, got {other:?}"),
            })
            .collect();
        let out = parse_lines(Timeframe::Hourly, &rows);
        assert_eq!(out.malformed, 0);
        let last = &out.bars[2];
        assert_eq!(last.ts.to_string(), "2026-09-18 16:00:00");
        assert_eq!((last.open, last.high, last.low, last.close), (7712.25, 7729.25, 7711.50, 7725.00));
        assert_eq!(last.volume, 75783);
    }

    /// Two days of `@MES#C` hourly rows saved with `probe --save-raw` from a live session.
    #[test]
    fn parses_live_mes_hourly_fixture() {
        let raw = include_str!("../tests/fixtures/mes_hourly_raw.txt");
        let lines: Vec<&str> = raw.lines().collect();
        let out = parse_lines(Timeframe::Hourly, &lines);

        assert_eq!(lines.len(), 46);
        assert_eq!(out.malformed, 0, "first error: {:?}", out.first_error);
        assert_eq!(out.bars.len(), 46);

        let first = &out.bars[0];
        assert_eq!(first.ts.to_string(), "2026-09-16 00:00:00");
        assert_eq!((first.open, first.high, first.low, first.close), (7672.5, 7674.25, 7667.0, 7668.0));
        assert_eq!(first.volume, 3641); // period volume, not the running total (49938)

        assert!(out.bars.windows(2).all(|w| w[0].ts < w[1].ts), "bars must be ascending");
        assert!(out.bars.iter().all(|b| b.high >= b.low && b.high >= b.open.max(b.close) && b.low <= b.open.min(b.close)));
        assert!(out.bars.iter().all(|b| b.open_interest.is_none()));
    }

    #[test]
    fn parses_daily_row_with_high_low_first() {
        let bar = parse_row(
            Timeframe::Daily,
            "2024-01-02 00:00:00,4792.50,4782.00,4785.25,4789.75,1245300,2100000",
        )
        .unwrap();
        assert_eq!(bar.open, 4785.25);
        assert_eq!(bar.high, 4792.50);
        assert_eq!(bar.low, 4782.00);
        assert_eq!(bar.close, 4789.75);
        assert_eq!(bar.volume, 1_245_300);
        assert_eq!(bar.open_interest, Some(2_100_000));
    }

    #[test]
    fn parses_hourly_row_using_period_volume() {
        let bar = parse_row(
            Timeframe::Hourly,
            "2024-01-02 09:00:00,4792.50,4782.00,4785.25,4789.75,99999,12453,3100",
        )
        .unwrap();
        assert_eq!(bar.open, 4785.25);
        assert_eq!(bar.volume, 12453);
        assert_eq!(bar.open_interest, None);
        assert_eq!(bar.ts.to_string(), "2024-01-02 09:00:00");
    }

    #[test]
    fn accepts_date_only_and_fractional_timestamps() {
        assert!(parse_row(Timeframe::Daily, "2024-01-02,2,1,1.5,1.8,10,5").is_ok());
        assert!(parse_row(Timeframe::Hourly, "2024-01-02 09:00:00.000000,2,1,1.5,1.8,10,5,1").is_ok());
    }

    #[test]
    fn rejects_malformed_rows_and_counts_them() {
        assert!(parse_row(Timeframe::Daily, "2024-01-02 00:00:00,1,2").is_err());
        assert!(parse_row(Timeframe::Daily, "not-a-date,2,1,1.5,1.8,10,5").is_err());
        assert!(parse_row(Timeframe::Daily, "2024-01-02,x,1,1.5,1.8,10,5").is_err());

        let out = parse_lines(
            Timeframe::Daily,
            &["2024-01-02,2,1,1.5,1.8,10,5", "garbage", "2024-01-03,2,1,1.5,1.8,10,5"],
        );
        assert_eq!(out.bars.len(), 2);
        assert_eq!(out.malformed, 1);
        assert!(out.first_error.unwrap().contains("garbage"));
    }
}
