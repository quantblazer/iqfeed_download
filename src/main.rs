mod aggregate;
mod config;
mod csv_store;
mod iqfeed;
mod model;
mod parser;
mod report;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{Duration as Days, Local, NaiveDate};
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};

use config::{Config, DailySource, IqFeedCfg, SymbolCfg};
use iqfeed::Client;
use model::{Bar, Timeframe};
use report::ReportRow;

const DEFAULT_CONFIG: &str = "config.toml";
/// How far before the last stored bar an incremental request starts, to detect re-adjustment.
const OVERLAP_DAYS: i64 = 7;

#[derive(Parser)]
#[command(name = "iqfeed-dl", version, about = "Download IQFeed continuous futures (daily + hourly) to CSV")]
struct Cli {
    /// Verbose (debug) logging, including the raw IQFeed requests.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check that IQFeed is reachable and answers a simple request.
    CheckConnection {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Print the configured symbols and their output files.
    ListSymbols {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Fetch one symbol and print the first/last bars without writing a CSV.
    Probe {
        symbol: String,
        #[arg(long, default_value = "1d")]
        timeframe: Timeframe,
        /// YYYY-MM-DD (default: one year ago)
        #[arg(long)]
        start: Option<String>,
        /// YYYY-MM-DD (default: today)
        #[arg(long)]
        end: Option<String>,
        /// Number of first and last bars to print.
        #[arg(long, default_value_t = 5)]
        rows: usize,
        /// Save the raw IQFeed data rows here (useful for test fixtures).
        #[arg(long)]
        save_raw: Option<PathBuf>,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Download all configured symbols to CSV.
    Download {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        /// Only this timeframe (1d or 1h).
        #[arg(long)]
        timeframe: Option<Timeframe>,
        /// Only these symbols (by config name); repeatable.
        #[arg(long = "symbol")]
        symbols: Vec<String>,
        /// Append only new bars instead of replacing the file. Falls back to a full refresh
        /// if the recent overlap no longer matches (i.e. a roll re-adjusted the history).
        #[arg(long)]
        incremental: bool,
        /// Keep retrying the IQFeed connection for up to this many minutes before giving up
        /// (for scheduled runs that start while IQFeed is still logging in).
        #[arg(long, value_name = "MINUTES", default_value_t = 0)]
        wait_for_iqfeed: u64,
        /// Allow a refresh to replace a file even if the new download has far fewer rows.
        #[arg(long)]
        allow_shrink: bool,
    },
}

#[derive(Clone, Copy)]
struct RunOpts {
    incremental: bool,
    allow_shrink: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::CheckConnection { config } => {
            init_logging(cli.verbose, None)?;
            check_connection(&connection_settings(&config)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ListSymbols { config } => {
            let cfg = Config::load(&config)?;
            list_symbols(&cfg);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Probe { symbol, timeframe, start, end, rows, save_raw, config } => {
            init_logging(cli.verbose, None)?;
            let settings = connection_settings(&config)?;
            probe(&settings, &symbol, timeframe, start, end, rows, save_raw.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Download { config, timeframe, symbols, incremental, wait_for_iqfeed, allow_shrink } => {
            let cfg = Config::load(&config)?;
            init_logging(cli.verbose, Some(&cfg.output.logs_dir))?;
            if wait_for_iqfeed > 0 {
                wait_for_iqfeed_ready(&cfg.iqfeed, wait_for_iqfeed)?;
            }
            let opts = RunOpts { incremental, allow_shrink };
            let failed = download(&cfg, timeframe, &symbols, opts)?;
            Ok(if failed == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
    }
}

// ---------------------------------------------------------------- logging

struct Tee {
    file: Arc<Mutex<File>>,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stderr().write_all(buf)?;
        self.file.lock().unwrap_or_else(|e| e.into_inner()).write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()?;
        self.file.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

fn init_logging(verbose: bool, log_dir: Option<&Path>) -> Result<()> {
    let level = if verbose { tracing::Level::DEBUG } else { tracing::Level::INFO };
    let builder = tracing_subscriber::fmt().with_max_level(level).with_ansi(false);
    match log_dir {
        Some(dir) => {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
            let path = dir.join(format!("export_{}.log", Local::now().format("%Y-%m-%d")));
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("cannot open log file {}", path.display()))?;
            let file = Arc::new(Mutex::new(file));
            builder.with_writer(move || Tee { file: file.clone() }).init();
        }
        None => builder.with_writer(io::stderr).init(),
    }
    Ok(())
}

// ---------------------------------------------------------------- sessions

fn connection_settings(config: &Path) -> Result<IqFeedCfg> {
    if config.exists() {
        Ok(Config::load(config)?.iqfeed)
    } else {
        Ok(IqFeedCfg::default())
    }
}

/// A lazily (re)connected IQFeed client. Any error that leaves the socket in an unknown
/// state drops the connection so the next request starts clean.
struct Session {
    cfg: IqFeedCfg,
    client: Option<Client>,
}

impl Session {
    fn new(cfg: &IqFeedCfg) -> Self {
        Session { cfg: cfg.clone(), client: None }
    }

    fn client(&mut self) -> Result<&mut Client> {
        if self.client.is_none() {
            let timeout = Duration::from_secs(self.cfg.timeout_seconds);
            self.client = Some(Client::connect(&self.cfg.host, self.cfg.lookup_port, timeout)?);
        }
        Ok(self.client.as_mut().expect("just connected"))
    }

    fn fetch_bars(
        &mut self,
        symbol: &str,
        tf: Timeframe,
        begin: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Bar>> {
        let lines = match self.client()?.fetch(symbol, tf, begin, end) {
            Ok(lines) => lines,
            Err(e) => {
                if e.connection_unusable() {
                    self.client = None;
                }
                return Err(e.into());
            }
        };
        let parsed = parser::parse_lines(tf, &lines);
        if parsed.malformed > 0 {
            warn!(
                "{symbol} {tf}: skipped {} malformed row(s); first: {}",
                parsed.malformed,
                parsed.first_error.as_deref().unwrap_or("?")
            );
        }
        if parsed.bars.is_empty() {
            bail!("no parseable rows; first error: {}", parsed.first_error.unwrap_or_default());
        }
        Ok(csv_store::normalize(parsed.bars))
    }
}

// ---------------------------------------------------------------- commands

fn check_connection(settings: &IqFeedCfg) -> Result<()> {
    let mut session = Session::new(settings);
    session.client()?;
    println!("OK: connected to IQFeed at {}:{}", settings.host, settings.lookup_port);

    let today = Local::now().date_naive();
    let bars = session.fetch_bars("@ES#C", Timeframe::Daily, today - Days::days(14), today)?;
    let last = bars.last().expect("fetch_bars never returns empty");
    println!(
        "OK: daily request for @ES#C returned {} bars, latest {}",
        bars.len(),
        last.summary(Timeframe::Daily)
    );
    Ok(())
}

fn list_symbols(cfg: &Config) {
    for s in &cfg.symbols {
        let files: Vec<String> = cfg
            .history
            .timeframes
            .iter()
            .map(|&tf| cfg.output_path(s, tf).display().to_string())
            .collect();
        println!("{:<6} {:<10} {}", s.name, s.iqfeed_symbol, files.join("  "));
    }
}

fn probe(
    settings: &IqFeedCfg,
    symbol: &str,
    tf: Timeframe,
    start: Option<String>,
    end: Option<String>,
    rows: usize,
    save_raw: Option<&Path>,
) -> Result<()> {
    let today = Local::now().date_naive();
    let end = match end {
        Some(s) => NaiveDate::parse_from_str(&s, "%Y-%m-%d").context("--end must be YYYY-MM-DD")?,
        None => today,
    };
    let start = match start {
        Some(s) => NaiveDate::parse_from_str(&s, "%Y-%m-%d").context("--start must be YYYY-MM-DD")?,
        None => today - Days::days(365),
    };

    let timeout = Duration::from_secs(settings.timeout_seconds);
    let mut client = Client::connect(&settings.host, settings.lookup_port, timeout)?;
    let lines = client.fetch(symbol, tf, start, end)?;
    println!("{} raw rows for {symbol} ({tf}) {start}..{end}", lines.len());
    if let Some(path) = save_raw {
        fs::write(path, lines.join("\n") + "\n")
            .with_context(|| format!("cannot write {}", path.display()))?;
        println!("raw rows saved to {}", path.display());
    }

    let parsed = parser::parse_lines(tf, &lines);
    if parsed.malformed > 0 {
        println!(
            "WARNING: {} malformed row(s); first: {}",
            parsed.malformed,
            parsed.first_error.unwrap_or_default()
        );
    }
    let bars = csv_store::normalize(parsed.bars);
    if let (Some(first), Some(last)) = (bars.first(), bars.last()) {
        println!("{} parsed bars, {} .. {}", bars.len(), first.ts, last.ts);
    }
    println!("-- first {rows}");
    for b in bars.iter().take(rows) {
        println!("{}", b.summary(tf));
    }
    println!("-- last {rows}");
    for b in bars.iter().skip(bars.len().saturating_sub(rows)) {
        println!("{}", b.summary(tf));
    }
    Ok(())
}

/// Poll the lookup port until IQFeed accepts a connection, or fail after `minutes`.
fn wait_for_iqfeed_ready(cfg: &IqFeedCfg, minutes: u64) -> Result<()> {
    const RETRY_EVERY: Duration = Duration::from_secs(10);
    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    let timeout = Duration::from_secs(cfg.timeout_seconds);
    let mut attempt = 0u32;
    loop {
        match Client::connect(&cfg.host, cfg.lookup_port, timeout) {
            Ok(_) => {
                if attempt > 0 {
                    info!("IQFeed is ready (after {attempt} retries)");
                }
                return Ok(());
            }
            Err(e) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(e.context(format!("IQFeed did not become ready within {minutes} minute(s)")));
                }
                if attempt % 6 == 0 {
                    warn!("IQFeed not ready ({e:#}); retrying every 10s for up to {minutes} minute(s)");
                }
                attempt += 1;
                thread::sleep(RETRY_EVERY.min(remaining));
            }
        }
    }
}

/// Returns the number of failed symbol/timeframe pairs.
fn download(
    cfg: &Config,
    only_tf: Option<Timeframe>,
    only_symbols: &[String],
    opts: RunOpts,
) -> Result<usize> {
    let today = Local::now().date_naive();
    let start = cfg.history.start()?;
    let end = cfg.history.end(today)?;

    let timeframes: Vec<Timeframe> = match only_tf {
        Some(tf) if !cfg.history.timeframes.contains(&tf) => {
            bail!("timeframe {tf} is not listed in history.timeframes")
        }
        Some(tf) => vec![tf],
        None => cfg.history.timeframes.clone(),
    };
    for name in only_symbols {
        if !cfg.symbols.iter().any(|s| &s.name == name) {
            bail!("symbol '{name}' is not in the config");
        }
    }
    let symbols: Vec<&SymbolCfg> = cfg
        .symbols
        .iter()
        .filter(|s| only_symbols.is_empty() || only_symbols.contains(&s.name))
        .collect();

    let mut session = Session::new(&cfg.iqfeed);
    let mut report_rows = Vec::new();
    for sym in symbols {
        // Hourly first, so derived daily bars can reuse the hourly bars just downloaded.
        let mut ordered = timeframes.clone();
        ordered.sort_by_key(|tf| matches!(tf, Timeframe::Daily));
        let mut hourly_cache: Option<Vec<Bar>> = None;
        for &tf in &ordered {
            let started = Local::now();
            info!("{} {tf}: downloading {}", sym.name, sym.iqfeed_symbol);
            let outcome =
                download_one(&mut session, cfg, sym, tf, start, end, opts, &mut hourly_cache);
            let path = cfg.output_path(sym, tf);
            let (status, rows, err) = match outcome {
                Ok(n) => {
                    info!("{} {tf}: {n} rows -> {}", sym.name, path.display());
                    ("success", n, String::new())
                }
                Err(e) => {
                    error!("{} {tf}: FAILED: {e:#}", sym.name);
                    ("failed", 0, format!("{e:#}"))
                }
            };
            report_rows.push(ReportRow {
                name: sym.name.clone(),
                iqfeed_symbol: sym.iqfeed_symbol.clone(),
                timeframe: tf.label().to_string(),
                status: status.to_string(),
                rows,
                path: if status == "success" { path.display().to_string() } else { String::new() },
                error: err,
                start_time: started.format("%Y-%m-%d %H:%M:%S").to_string(),
                end_time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            });
        }
    }

    let report_path = report::write_report(&cfg.output.reports_dir, today, &report_rows)?;
    let failed = report_rows.iter().filter(|r| r.status == "failed").count();
    info!(
        "done: {} succeeded, {failed} failed; report {}",
        report_rows.len() - failed,
        report_path.display()
    );
    Ok(failed)
}

fn download_one(
    session: &mut Session,
    cfg: &Config,
    sym: &SymbolCfg,
    tf: Timeframe,
    start: NaiveDate,
    end: NaiveDate,
    opts: RunOpts,
    hourly_cache: &mut Option<Vec<Bar>>,
) -> Result<usize> {
    let path = cfg.output_path(sym, tf);

    let existing = if path.exists() {
        match csv_store::read_bars(&path, tf) {
            Ok(bars) => Some(bars),
            Err(e) => {
                warn!("{} {tf}: existing file unusable, it will be replaced: {e:#}", sym.name);
                None
            }
        }
    } else {
        None
    };
    let existing_rows = existing.as_ref().map_or(0, Vec::len);
    let guard = |new_rows: usize| -> Result<()> {
        if opts.allow_shrink {
            Ok(())
        } else {
            csv_store::check_not_shrunk(existing_rows, new_rows)
        }
    };

    if tf == Timeframe::Daily && cfg.history.daily_source == DailySource::Hourly {
        if hourly_cache.is_none() {
            *hourly_cache = Some(session.fetch_bars(&sym.iqfeed_symbol, Timeframe::Hourly, start, end)?);
        }
        let hourly = hourly_cache.as_deref().expect("just filled");
        let bars = aggregate::hourly_to_daily(hourly, cfg.history.session_start()?);
        info!(
            "{} {tf}: built {} daily bars from {} hourly bars (trade day starts {} ET)",
            sym.name,
            bars.len(),
            hourly.len(),
            cfg.history.session_start
        );
        guard(bars.len())?;
        csv_store::write_bars(&path, tf, &sym.name, &sym.iqfeed_symbol, &bars)?;
        return Ok(bars.len());
    }

    let bars = match existing.filter(|e| opts.incremental && !e.is_empty()) {
        Some(existing) => incremental_update(session, sym, tf, start, end, existing)?,
        None => {
            let bars = session.fetch_bars(&sym.iqfeed_symbol, tf, start, end)?;
            warn_if_history_shrank(&path, tf, sym, &bars);
            bars
        }
    };
    guard(bars.len())?;
    csv_store::write_bars(&path, tf, &sym.name, &sym.iqfeed_symbol, &bars)?;
    if tf == Timeframe::Hourly {
        *hourly_cache = Some(bars.clone());
    }
    Ok(bars.len())
}

fn incremental_update(
    session: &mut Session,
    sym: &SymbolCfg,
    tf: Timeframe,
    start: NaiveDate,
    end: NaiveDate,
    existing: Vec<Bar>,
) -> Result<Vec<Bar>> {
    let last = existing.last().expect("caller checked non-empty").ts;
    let begin = (last.date() - Days::days(OVERLAP_DAYS)).max(start);
    let fresh = session.fetch_bars(&sym.iqfeed_symbol, tf, begin, end)?;

    if !csv_store::overlap_consistent(&existing, &fresh, last) {
        warn!(
            "{} {tf}: overlap with the existing file does not match (history was re-adjusted?); doing a full refresh",
            sym.name
        );
        return session.fetch_bars(&sym.iqfeed_symbol, tf, start, end);
    }
    let (merged, stats) = csv_store::merge(existing, fresh);
    info!("{} {tf}: incremental, {} new bars, {} changed", sym.name, stats.added, stats.changed);
    Ok(merged)
}

fn warn_if_history_shrank(path: &Path, tf: Timeframe, sym: &SymbolCfg, new: &[Bar]) {
    let Ok(old) = csv_store::read_bars(path, tf) else { return };
    if let (Some(old_first), Some(new_first)) = (old.first(), new.first()) {
        if new_first.ts > old_first.ts {
            warn!(
                "{} {tf}: new download starts at {} but the existing file started at {}; older history will be lost",
                sym.name, new_first.ts, old_first.ts
            );
        }
    }
}
