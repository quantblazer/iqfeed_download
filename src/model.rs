use std::fmt;
use std::str::FromStr;

use chrono::NaiveDateTime;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Timeframe {
    Daily,
    Hourly,
}

impl Timeframe {
    /// Short label used in file names, config and the CLI ("1d" / "1h").
    pub fn label(self) -> &'static str {
        match self {
            Timeframe::Daily => "1d",
            Timeframe::Hourly => "1h",
        }
    }

    /// Format used for the `timestamp` column in the CSV.
    pub fn timestamp_format(self) -> &'static str {
        match self {
            Timeframe::Daily => "%Y-%m-%d",
            Timeframe::Hourly => "%Y-%m-%d %H:%M:%S",
        }
    }

    /// IQFeed only supplies open interest on daily bars.
    pub fn has_open_interest(self) -> bool {
        matches!(self, Timeframe::Daily)
    }

    pub fn columns(self) -> &'static [&'static str] {
        match self {
            Timeframe::Daily => &[
                "timestamp",
                "open",
                "high",
                "low",
                "close",
                "volume",
                "open_interest",
                "iqfeed_symbol",
                "name",
            ],
            Timeframe::Hourly => &[
                "timestamp",
                "open",
                "high",
                "low",
                "close",
                "volume",
                "iqfeed_symbol",
                "name",
            ],
        }
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Timeframe {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1d" | "d" | "daily" => Ok(Timeframe::Daily),
            "1h" | "h" | "hourly" => Ok(Timeframe::Hourly),
            other => Err(format!("unknown timeframe '{other}' (expected 1d or 1h)")),
        }
    }
}

impl TryFrom<String> for Timeframe {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

/// One OHLCV bar. Timestamps are kept exactly as IQFeed reports them (Eastern time).
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub ts: NaiveDateTime,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
    pub open_interest: Option<u64>,
}

impl Bar {
    /// Compact single-line form for `probe` output.
    pub fn summary(&self, tf: Timeframe) -> String {
        let mut s = format!(
            "{}  O={} H={} L={} C={} V={}",
            self.ts.format(tf.timestamp_format()),
            self.open,
            self.high,
            self.low,
            self.close,
            self.volume
        );
        if let Some(oi) = self.open_interest {
            s.push_str(&format!(" OI={oi}"));
        }
        s
    }
}
