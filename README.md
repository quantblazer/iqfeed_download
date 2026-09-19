# iqfeed-dl

Downloads IQFeed's continuous back-adjusted futures (`@ES#C` style symbols) and saves one CSV
per market per timeframe: **hourly** bars straight from IQFeed and **daily** bars built from
those hourly bars. Written in Rust, single binary, no rollover or back-adjustment logic of its
own; IQFeed's adjusted series is used as is.

```
IQFeed continuous symbol -> HIT hourly history -> parse -> CSV (1h)
                                             \-> aggregate by trade day -> CSV (1d)
```

## Prerequisites

- **IQFeed client installed, running and logged in.** Start it via the IQFeed launcher
  (`iqlink.exe`) and click Connect. The downloader talks to the local lookup socket
  `127.0.0.1:9100`, which only opens once IQConnect is connected to DTN. If port 9100 is
  closed, the downloader reports "cannot connect ... is IQConnect running and logged in?".
- A DTN subscription with futures history (hourly history is verified to work; see
  [IQFeed notes](#iqfeed-notes-and-gotchas) for daily).
- Rust (stable, MSVC toolchain) and Visual Studio Build Tools with the C++ workload to build.

## Build

```powershell
cargo build --release          # binary: target\release\iqfeed-dl.exe
cargo test                     # 30 tests, none need IQFeed
```

Below, `iqfeed-dl` means `target\release\iqfeed-dl.exe` (or `cargo run --release --`).

## Quick start

```powershell
iqfeed-dl check-connection                     # is IQFeed reachable and answering?
iqfeed-dl list-symbols                         # what config.toml will download
iqfeed-dl probe '@MES#C' --timeframe 1h        # look at a symbol before adding it
iqfeed-dl download                             # everything in config.toml
iqfeed-dl download --symbol ES --symbol NQ     # only some markets
iqfeed-dl download --timeframe 1h              # only one timeframe
```

Quote symbols containing `#` in PowerShell (`'@ES#C'`), or the rest of the line is treated as
a comment.

## Commands

| Command | What it does |
|---|---|
| `check-connection` | Connects, sets protocol 6.2, requests a few recent `@ES#C` daily bars. |
| `list-symbols` | Prints each configured symbol and its output files. |
| `probe <symbol>` | Fetches one symbol and prints the first/last bars. Writes no CSV. Options: `--timeframe 1d\|1h` (default `1d`), `--start`, `--end` (`YYYY-MM-DD`), `--rows N`, `--save-raw FILE` (raw IQFeed rows, handy for test fixtures). |
| `download` | Downloads all configured symbols. Options: `--timeframe`, `--symbol NAME` (repeatable), `--incremental`, `--wait-for-iqfeed <MINUTES>` (keep retrying the connection, for scheduled runs), `--allow-shrink` (see [shrink guard](#shrink-guard)). |

Every command except `probe`'s symbol argument accepts `--config <path>` (default
`config.toml`). `-v` turns on debug logging, including every raw IQFeed request.

`probe --timeframe 1d` uses IQFeed's *native* daily history, which currently fails on the
account this was built against (see below). Use `--timeframe 1h` to probe.

Exit codes: `0` all succeeded, `1` at least one symbol/timeframe failed (the rest still
completed), `2` fatal error such as bad config or IQFeed unreachable.

## Configuration (`config.toml`)

```toml
[iqfeed]
host = "127.0.0.1"
lookup_port = 9100          # IQFeed's lookup/history port
timeout_seconds = 60        # per socket read

[output]
directory = "./data/continuous"
reports_dir = "./reports"   # default
logs_dir = "./logs"         # default

[history]
start_date = "2010-01-01"   # YYYY-MM-DD
end_date = "today"          # or YYYY-MM-DD
timeframes = ["1d", "1h"]   # any subset
daily_source = "hourly"     # "hourly" (build from hourly) or "iqfeed" (native HDT)
session_start = "18:00"     # Eastern; trade-day start used when daily_source = "hourly"

[[symbols]]
name = "ES"
iqfeed_symbol = "@ES#C"
# file_prefix = "ES_custom"   # optional override
```

Included markets (all probed against a live IQFeed session):

| name | IQFeed symbol | | name | IQFeed symbol |
|---|---|---|---|---|
| ES | `@ES#C` | | CL | `QCL#C` |
| MES | `@MES#C` | | GC | `QGC#C` |
| NQ | `@NQ#C` | | ZN | `@TY#C` |
| YM | `@YM#C` | | ZB | `@US#C` |
| RTY | `@RTY#C` | | | |

Symbol notes: `#C` is back-adjusted continuous, `#` is the unadjusted front month. CME/CBOT
equity index products use an `@` prefix, NYMEX/COMEX use `Q`, and CBOT rates use IQFeed's
legacy roots (`@TY` = 10-year note, `@US` = 30-year bond). Symbols are explicit on purpose;
check new ones with `probe` before adding them.

Validation rejects: no symbols, no timeframes, bad dates, start after end, bad
`session_start`, duplicate names, two symbols writing the same files, and file prefixes with
characters other than letters, digits, `_`, `-`.

## Output

Files: `<prefix>_<1d|1h>.csv`, where the prefix is `<name>_back_adjusted` for `#C` symbols,
`<name>_unadjusted` for `#` symbols, otherwise `<name>` (or `file_prefix` if set). Example:
`data/continuous/ES_back_adjusted_1h.csv`.

Hourly, `YYYY-MM-DD HH:MM:SS`:

```csv
timestamp,open,high,low,close,volume,iqfeed_symbol,name
2026-09-18 16:00:00,7712.25,7729.25,7711.75,7725.5,33543,@MES#C,MES
```

Daily, `YYYY-MM-DD`:

```csv
timestamp,open,high,low,close,volume,open_interest,iqfeed_symbol,name
2026-09-18,7704.75,7739,7675,7725.5,869142,,@MES#C,MES
```

- Rows are sorted oldest to newest with unique timestamps.
- **Timestamps are exactly as IQFeed reports them: US Eastern time.** Hourly bars are stamped
  with the bar's **start** (the `16:00` bar covers 16:00 to 17:00).
- `volume` is the volume traded in that bar.
- `open_interest` is empty for derived daily bars (see below).
- Prices are written with the shortest exact decimal form (`4785` rather than `4785.00`).
- Files are written atomically: data goes to `<file>.tmp`, is re-read and verified, then
  renamed over the target. A crash or failure leaves the previous file intact.
- A re-run over the same data produces a byte-identical file.

### Daily bars built from hourly

IQFeed's own daily history (`HDX`/`HDT`) was failing on the development account, so by
default (`daily_source = "hourly"`) each daily bar is aggregated from the hourly bars of one
**trade day**:

- The trade day starts at `session_start` Eastern on the previous calendar day and is dated by
  the day it ends on. With the default `18:00`, Sunday 18:00 belongs to Monday, and Thursday
  18:00 through Friday 16:59 is Friday's bar (the CME Globex day).
- open = first hourly open, high = max, low = min, close = last hourly close, volume = sum.
- No open interest (IQFeed's interval bars don't carry it).
- The newest daily bar is partial if you download during a session; the next run replaces it.

**Matching MultiCharts (or any other platform):** daily bars match your platform's daily bars
built from minute data when both use the same trade-day definition. Set `session_start` to
the start of your platform's session template, in Eastern time. A regular-hours-only template
(for example 9:30 to 16:00 ET) cannot be reproduced this way. To check, compare one recent
day's OHLCV. Set `daily_source = "iqfeed"` to use IQFeed's native daily bars once that works.

## Full refresh vs incremental

By default every `download` re-downloads the whole history and replaces the file. That is
deliberate: **back-adjusted history changes whenever a contract rolls**, so appending new bars
to an old file would mix adjustment bases.

`--incremental` requests only the last 7 days before the newest stored bar plus everything
after, and merges (new rows win). It first compares the overlapping bars with the file. If
they differ (history was re-adjusted) or nothing overlaps, it falls back to a full refresh
automatically. Incremental applies to hourly files; derived daily bars are always rebuilt from
the hourly bars.

### Shrink guard

A refresh replaces a file only if the new download keeps at least **90%** of the existing
rows. Otherwise that symbol/timeframe fails (`exit code 1`, message in the log and report) and
the existing file is left untouched. This stops a partial or truncated IQFeed response from
overwriting good history. If you really want less history (for example you moved `start_date`
later), run with `--allow-shrink`.

## Scheduling (Windows Task Scheduler)

The recommended setup is one run per weekday at **17:15 ET**, right after the CME Globex day
ends (17:00 ET), so the last hourly bar and the derived daily bar are complete. A full refresh
of all markets takes under a minute, so there is no need for incremental runs. This machine
is on US Eastern time, so the task's local time is ET (Task Scheduler follows daylight saving).

```powershell
cargo build --release                       # the task runs target\release\iqfeed-dl.exe
.\register-task.ps1 -IQFeedAutostart        # register the task and start IQFeed at logon
.\register-task.ps1 -RunNow                 # optional: run it once now (see logs\scheduled.log)
.\register-task.ps1 -Time 17:30             # change the time (re-registers the task)
.\register-task.ps1 -Unregister             # remove the task
```

How it fits together:

- **`register-task.ps1`** creates the task "IQFeed Continuous Futures Download": Monday to
  Friday, runs **only while you are logged on** (IQFeed is a desktop app, and no password is
  stored anywhere), starts as soon as possible if the PC was off at the scheduled time, and
  cannot overlap itself or run longer than an hour.
- **`run-scheduled.ps1`** is what the task runs. It calls
  `iqfeed-dl download --wait-for-iqfeed 15`, and if that fails it waits 5 minutes and tries
  once more. Each attempt is recorded in `logs\scheduled.log`. Its exit code is the
  downloader's (0 ok, 1 some symbols failed, 2 fatal), which shows up as the task's
  "Last Run Result".
- **`--wait-for-iqfeed`** polls the lookup port every 10 seconds, so the task can start while
  IQFeed is still logging in. If it never comes up, the run fails with exit code 2.
- **IQFeed login at startup:** `-IQFeedAutostart` puts a shortcut to the IQFeed launcher
  (`iqlink.exe`) in your Startup folder. Open the launcher once and tick its option to save
  the username and password / connect automatically, so it can log in without you.

Checking a run: `logs\scheduled.log` (one line per attempt), then `logs\export_YYYY-MM-DD.log`
and `reports\download_YYYY-MM-DD.csv` for per-symbol detail. In Task Scheduler the task's
history shows the exit code.

Caveats: the PC must be on and you logged in at run time (or at the next opportunity), and
IQFeed must be able to log in. If IQFeed's servers are down, the run fails after the wait and
the retry, and the previous CSVs stay as they were.

## Logs and reports

- Log: `logs/export_YYYY-MM-DD.log` (appended), also printed to the console.
- Report: `reports/download_YYYY-MM-DD.csv`, one row per symbol and timeframe (overwritten by
  the next run on the same day):

```csv
name,iqfeed_symbol,timeframe,status,rows,path,error,start_time,end_time
```

Failures (invalid symbol, no data, timeout, partial response, malformed rows, disk errors) are
logged with IQFeed's own message, recorded as `failed` in the report, and do not stop the run.
After a timeout or dropped connection the client reconnects for the next request. Malformed
individual rows are skipped with a warning; a request with no parseable rows fails.

## Project layout

```
config.toml            what to download
run-scheduled.ps1      scheduled entry point (wait for IQFeed, retry once, log)
register-task.ps1      create/remove the Task Scheduler job
src/main.rs            CLI, download loop, incremental logic, shrink guard, logging
src/iqfeed.rs          IQFeed socket client: the only place that knows the wire format
src/parser.rs          response lines -> bars
src/aggregate.rs       hourly -> daily by trade day
src/csv_store.rs       read, merge, dedupe, atomic write
src/config.rs          config loading and validation
src/report.rs          per-run report
src/model.rs           Timeframe and Bar
tests/fixtures/        raw IQFeed rows captured from a live session
data/continuous/       output CSVs
```

## IQFeed notes and gotchas

Verified against a live IQFeed 6.2 session:

- Requests, sent to port 9100 after `S,SET PROTOCOL,6.2`:
  - hourly: `HIT,<sym>,3600,<yyyymmdd hhmmss>,<yyyymmdd hhmmss>,,,,1,<reqid>`
  - daily: `HDT,<sym>,<yyyymmdd>,<yyyymmdd>,,1,<reqid>`. The daily form **rejects a time part**.
- Response rows look like `<reqid>,LH,<timestamp>,High,Low,Open,Close,TotalVolume,PeriodVolume,NumTrades,`
  (note **High and Low come before Open and Close**). The parser uses period volume.
- **Every message ends with a trailing comma**, including the end marker `<reqid>,!ENDMSG!,`.
  Errors arrive as `<reqid>,E,<message>,` followed by the end marker.
- Intervals of 86400 seconds or more are rejected, so daily bars cannot come from `HIT`.
- **Native daily history (`HDX`, `HDT`) and weekly (`HWX`) returned
  `Unknown Server Error code 0` for every symbol** on the account used for development,
  including stocks. Intraday history worked. This is on the DTN side (entitlement or their
  daily-history service); it is why daily bars are derived.
- **Do not send `HMX`** (monthly bars): it made IQConnect drop the connection and exit.
- IQConnect exits or refuses connections until it is logged in. The admin port (9300) reports
  `Connected` / `Not Connected` in its `S,STATS` line.
- Historical depth depends on the subscription. The 2010-01-01 start returned data for all
  markets except MES, which launched in May 2019.

## Older plan

`iqfeed-continuous-futures-csv-exporter-plan.md` is the original Python/hourly design. It is
superseded by this Rust implementation (daily and hourly, the corrected request formats above,
and full-refresh handling of back-adjusted data).
