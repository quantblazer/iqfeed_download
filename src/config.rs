use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, NaiveTime};
use serde::Deserialize;

use crate::model::Timeframe;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub iqfeed: IqFeedCfg,
    pub output: OutputCfg,
    pub history: HistoryCfg,
    pub symbols: Vec<SymbolCfg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IqFeedCfg {
    pub host: String,
    pub lookup_port: u16,
    pub timeout_seconds: u64,
}

impl Default for IqFeedCfg {
    fn default() -> Self {
        IqFeedCfg { host: "127.0.0.1".into(), lookup_port: 9100, timeout_seconds: 60 }
    }
}

#[derive(Debug, Deserialize)]
pub struct OutputCfg {
    pub directory: PathBuf,
    #[serde(default = "default_reports_dir")]
    pub reports_dir: PathBuf,
    #[serde(default = "default_logs_dir")]
    pub logs_dir: PathBuf,
}

fn default_reports_dir() -> PathBuf {
    "./reports".into()
}

fn default_logs_dir() -> PathBuf {
    "./logs".into()
}

#[derive(Debug, Deserialize)]
pub struct HistoryCfg {
    pub start_date: String,
    #[serde(default = "default_end")]
    pub end_date: String,
    pub timeframes: Vec<Timeframe>,
    /// Where daily bars come from: built from hourly bars (default) or IQFeed's own daily history.
    #[serde(default)]
    pub daily_source: DailySource,
    /// Eastern time the trade day starts, used when `daily_source = "hourly"` (CME Globex: 18:00).
    #[serde(default = "default_session_start")]
    pub session_start: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DailySource {
    #[default]
    Hourly,
    Iqfeed,
}

fn default_end() -> String {
    "today".into()
}

fn default_session_start() -> String {
    "18:00".into()
}

impl HistoryCfg {
    pub fn session_start(&self) -> Result<NaiveTime> {
        NaiveTime::parse_from_str(&self.session_start, "%H:%M")
            .with_context(|| format!("history.session_start must be HH:MM, got '{}'", self.session_start))
    }

    pub fn start(&self) -> Result<NaiveDate> {
        parse_date(&self.start_date, "history.start_date")
    }

    pub fn end(&self, today: NaiveDate) -> Result<NaiveDate> {
        if self.end_date.eq_ignore_ascii_case("today") {
            Ok(today)
        } else {
            parse_date(&self.end_date, "history.end_date")
        }
    }
}

fn parse_date(s: &str, field: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("{field} must be YYYY-MM-DD, got '{s}'"))
}

#[derive(Debug, Deserialize)]
pub struct SymbolCfg {
    pub name: String,
    pub iqfeed_symbol: String,
    /// Overrides the default file name prefix (`{name}_back_adjusted`).
    pub file_prefix: Option<String>,
}

impl SymbolCfg {
    pub fn prefix(&self) -> String {
        if let Some(p) = &self.file_prefix {
            p.clone()
        } else if self.iqfeed_symbol.ends_with("#C") {
            format!("{}_back_adjusted", self.name)
        } else if self.iqfeed_symbol.ends_with('#') {
            format!("{}_unadjusted", self.name)
        } else {
            self.name.clone()
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn output_path(&self, sym: &SymbolCfg, tf: Timeframe) -> PathBuf {
        self.output.directory.join(format!("{}_{}.csv", sym.prefix(), tf))
    }

    fn validate(&self) -> Result<()> {
        if self.symbols.is_empty() {
            bail!("no [[symbols]] configured");
        }
        if self.history.timeframes.is_empty() {
            bail!("history.timeframes is empty");
        }
        if self.iqfeed.timeout_seconds == 0 {
            bail!("iqfeed.timeout_seconds must be > 0");
        }
        self.history.session_start()?;
        let today = chrono::Local::now().date_naive();
        if self.history.start()? > self.history.end(today)? {
            bail!("history.start_date is after history.end_date");
        }
        let mut names = HashSet::new();
        let mut prefixes = HashSet::new();
        for s in &self.symbols {
            if s.name.trim().is_empty() || s.iqfeed_symbol.trim().is_empty() {
                bail!("every symbol needs a non-empty name and iqfeed_symbol");
            }
            let prefix = s.prefix();
            if !prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                bail!("symbol '{}': file prefix '{prefix}' may only use letters, digits, _ and -", s.name);
            }
            if !names.insert(s.name.clone()) {
                bail!("duplicate symbol name '{}'", s.name);
            }
            if !prefixes.insert(prefix.clone()) {
                bail!("symbol '{}' would write to the same files as another symbol ('{prefix}')", s.name);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
        [output]
        directory = "./data/continuous"
        [history]
        start_date = "2010-01-01"
        timeframes = ["1d", "1h"]
        [[symbols]]
        name = "ES"
        iqfeed_symbol = "@ES#C"
        [[symbols]]
        name = "ES"
        iqfeed_symbol = "@ES#"
    "#;

    #[test]
    fn derives_file_names_from_symbol_kind() {
        let text = GOOD.replace("name = \"ES\"\n        iqfeed_symbol = \"@ES#\"", "name = \"ESU\"\n        iqfeed_symbol = \"@ES#\"");
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.iqfeed.lookup_port, 9100);
        assert_eq!(
            cfg.output_path(&cfg.symbols[0], Timeframe::Daily),
            PathBuf::from("./data/continuous/ES_back_adjusted_1d.csv")
        );
        assert_eq!(
            cfg.output_path(&cfg.symbols[1], Timeframe::Hourly),
            PathBuf::from("./data/continuous/ESU_unadjusted_1h.csv")
        );
    }

    #[test]
    fn rejects_duplicate_names() {
        assert!(Config::parse(GOOD).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_bad_timeframe_date_and_empty_symbols() {
        let bad_tf = GOOD.replace("\"1h\"", "\"5m\"");
        assert!(Config::parse(&bad_tf).is_err());
        let bad_date = GOOD.replace("2010-01-01", "01/01/2010");
        assert!(Config::parse(&bad_date).is_err());
        let no_syms = "[output]\ndirectory=\"d\"\n[history]\nstart_date=\"2010-01-01\"\ntimeframes=[\"1d\"]\nsymbols=[]";
        assert!(Config::parse(no_syms).is_err());
    }

    #[test]
    fn rejects_unsafe_file_prefix() {
        let text = GOOD.replace("name = \"ES\"\n        iqfeed_symbol = \"@ES#\"", "name = \"X\"\n        iqfeed_symbol = \"@X#\"\n        file_prefix = \"../evil\"");
        assert!(Config::parse(&text).unwrap_err().to_string().contains("file prefix"));
    }
}
