# IQFeed Continuous Futures CSV Exporter Plan

## Goal

Build an application that downloads **hourly continuous back-adjusted futures data directly from IQFeed** and saves each market to CSV.

The app will use IQFeed's already-created continuous/back-adjusted futures symbols, so it does **not** need to calculate rollovers or perform its own back-adjustment in the first version.

Example output:

```text
data/
  continuous/
    ES_back_adjusted_1h.csv
    NQ_back_adjusted_1h.csv
    CL_back_adjusted_1h.csv
    GC_back_adjusted_1h.csv
```

---

## Main Decision

Use IQFeed's provided continuous back-adjusted futures symbols directly.

Conceptual flow:

```text
IQFeed continuous back-adjusted symbol
    -> hourly historical request
    -> response parser
    -> CSV writer
    -> back-adjusted continuous CSV file
```

This replaces the more complex custom pipeline:

```text
raw futures contracts
    -> rollover schedule
    -> back-adjustment calculation
    -> continuous contract CSV
```

For MVP, the app should avoid custom rollover/back-adjustment logic and rely on IQFeed's adjusted continuous data.

---

## Assumptions

The app will:

- Run on a machine where IQFeed Client is installed and running.
- Connect to IQFeed locally through its lookup/history socket.
- Download hourly historical bars.
- Save one CSV per configured continuous symbol.
- Be config-driven.
- Support re-running without duplicating rows.
- Log failed symbols without stopping the full run.
- Preserve IQFeed timestamps as received unless timezone normalization is added later.

---

## Recommended Technology Stack

```text
Language: Python
Data handling: pandas
Config: YAML
CLI: argparse or typer
Logging: Python logging
Output format: CSV
Testing: pytest
```

Recommended project structure:

```text
iqfeed-futures-exporter/
  README.md
  requirements.txt
  config.yaml
  src/
    iqfeed_exporter/
      __init__.py
      main.py
      config.py
      iqfeed_client.py
      parser.py
      csv_writer.py
      logging_config.py
  tests/
    test_config.py
    test_parser.py
    test_csv_writer.py
  data/
    continuous/
  logs/
  reports/
```

---

## Application Architecture

```text
config.yaml
    |
    v
Configured Symbol List
    |
    v
IQFeed Lookup Socket
    |
    v
Hourly Historical Requests
    |
    v
IQFeed Response Parser
    |
    v
CSV Normalizer
    |
    v
Atomic CSV Writer
    |
    v
data/continuous/*.csv
```

---

## Data Source

The app should use IQFeed continuous back-adjusted futures symbols.

Possible examples may look like:

```text
@ES#C
@NQ#C
@CL#C
@GC#C
```

However, the exact IQFeed symbol format should be verified in IQFeed Symbol Lookup or IQFeed documentation. The application should therefore avoid hardcoding assumptions and make each IQFeed symbol explicit in `config.yaml`.

---

## Example Configuration

```yaml
iqfeed:
  host: "127.0.0.1"
  lookup_port: 9100
  timeout_seconds: 60

output:
  directory: "./data/continuous"
  overwrite: false

history:
  interval_seconds: 3600
  start_date: "2015-01-01"
  end_date: "2026-12-31"

symbols:
  - name: "ES"
    iqfeed_symbol: "@ES#C"
    output_file: "ES_back_adjusted_1h.csv"

  - name: "NQ"
    iqfeed_symbol: "@NQ#C"
    output_file: "NQ_back_adjusted_1h.csv"

  - name: "CL"
    iqfeed_symbol: "@CL#C"
    output_file: "CL_back_adjusted_1h.csv"

  - name: "GC"
    iqfeed_symbol: "@GC#C"
    output_file: "GC_back_adjusted_1h.csv"
```

---

## CSV Output Format

Recommended columns:

```csv
timestamp,open,high,low,close,volume,open_interest,iqfeed_symbol,name
```

Example:

```csv
2024-01-02 09:00:00,4785.25,4792.50,4782.00,4789.75,12453,0,@ES#C,ES
2024-01-02 10:00:00,4789.75,4798.00,4788.25,4795.50,18341,0,@ES#C,ES
```

Recommended output paths:

```text
data/continuous/ES_back_adjusted_1h.csv
data/continuous/NQ_back_adjusted_1h.csv
data/continuous/CL_back_adjusted_1h.csv
data/continuous/GC_back_adjusted_1h.csv
```

---

## CLI Commands

### Check IQFeed connection

```bash
python -m iqfeed_exporter.main check-connection
```

### List configured symbols

```bash
python -m iqfeed_exporter.main list-symbols --config config.yaml
```

Example output:

```text
ES  @ES#C  ES_back_adjusted_1h.csv
NQ  @NQ#C  NQ_back_adjusted_1h.csv
CL  @CL#C  CL_back_adjusted_1h.csv
GC  @GC#C  GC_back_adjusted_1h.csv
```

### Download one symbol

```bash
python -m iqfeed_exporter.main download-symbol @ES#C --start 2015-01-01 --end 2026-12-31
```

### Download all configured symbols

```bash
python -m iqfeed_exporter.main download --config config.yaml
```

---

## Download Flow

```text
1. Load config.yaml.

2. Connect to IQFeed lookup socket.

3. For each configured symbol:
   - Build hourly historical request.
   - Send request to IQFeed.
   - Read response until end marker.
   - Parse rows.
   - Normalize columns.
   - Merge with existing CSV if present.
   - Remove duplicate timestamps.
   - Sort rows by timestamp ascending.
   - Save CSV atomically.
   - Record result in report.

4. Write summary report.

5. Write log file.
```

---

## Duplicate Handling

When the app is re-run, it should merge existing rows with newly downloaded rows.

Process:

```text
existing CSV + newly downloaded rows
    -> combine
    -> drop duplicate timestamps
    -> keep latest IQFeed row
    -> sort by timestamp
    -> write final CSV
```

If the same timestamp appears with different OHLCV values, keep the newly downloaded IQFeed row and log a warning.

---

## Atomic CSV Writing

CSV writing should be atomic to avoid corrupt files.

```text
1. Write to temporary file:
   ES_back_adjusted_1h.csv.tmp

2. Verify file was written.

3. Replace final file:
   ES_back_adjusted_1h.csv
```

---

## Error Handling

The app should handle:

- IQFeed Client not running
- Socket connection failure
- Socket timeout
- Invalid IQFeed symbol
- No data returned
- Malformed IQFeed response rows
- Partial response
- Disk write failure
- Existing CSV with invalid format

Symbol-level failure should not stop the whole run.

Example report:

```csv
name,iqfeed_symbol,status,rows,path,error
ES,@ES#C,success,54231,data/continuous/ES_back_adjusted_1h.csv,
NQ,@NQ#C,success,54192,data/continuous/NQ_back_adjusted_1h.csv,
CL,@CL#C,failed,0,,No data returned
```

---

## Logging and Reports

Recommended log output:

```text
logs/export_YYYY-MM-DD.log
```

Recommended report output:

```text
reports/download_YYYY-MM-DD.csv
```

Report columns:

```csv
name,iqfeed_symbol,status,rows,path,error,start_time,end_time
```

---

## Modules

### `config.py`

Responsibilities:

- Load YAML config.
- Validate required fields.
- Validate symbol entries.
- Return typed config object.

### `iqfeed_client.py`

Responsibilities:

- Connect to IQFeed lookup port.
- Send historical interval requests.
- Read response lines.
- Detect IQFeed end message.
- Handle socket timeouts.

### `parser.py`

Responsibilities:

- Parse IQFeed response rows.
- Ignore control/end messages.
- Convert timestamps.
- Convert OHLCV fields to numeric values.
- Return normalized row dictionaries.

Example normalized row:

```python
{
    "timestamp": "2024-01-02 09:00:00",
    "open": 4785.25,
    "high": 4792.50,
    "low": 4782.00,
    "close": 4789.75,
    "volume": 12453,
    "open_interest": 0,
    "iqfeed_symbol": "@ES#C",
    "name": "ES",
}
```

### `csv_writer.py`

Responsibilities:

- Create output directories.
- Load existing CSV if present.
- Merge old and new rows.
- Drop duplicate timestamps.
- Sort by timestamp.
- Write atomically.

### `main.py`

Responsibilities:

- Provide CLI commands.
- Coordinate config, IQFeed client, parser, and CSV writer.
- Print progress.
- Create reports.

---

## MVP Implementation Phases

### Phase 1 — Single-symbol prototype

Build a script that downloads one configured continuous symbol.

Example:

```text
@ES#C -> data/continuous/ES_back_adjusted_1h.csv
```

Success criteria:

```text
1. IQFeed Client is running.
2. App connects to 127.0.0.1:9100.
3. App requests hourly history for @ES#C.
4. CSV file is created.
5. CSV contains hourly OHLCV rows.
```

### Phase 2 — Parser and CSV writer

Build:

- IQFeed response parser
- Normalized row model
- CSV writer
- Duplicate removal
- Timestamp sorting
- Atomic writes

Success criteria:

```text
1. CSV has consistent columns.
2. Rows are sorted oldest to newest.
3. Re-running does not create duplicate rows.
4. Existing CSV files are not corrupted on failure.
```

### Phase 3 — Config-driven symbol list

Build:

- `config.yaml`
- symbol list loader
- `list-symbols` command

Success criteria:

```bash
python -m iqfeed_exporter.main list-symbols --config config.yaml
```

prints all configured symbols and output files.

### Phase 4 — Multi-symbol downloader

Build:

- loop through all configured symbols
- download each symbol
- save each symbol to its own CSV
- continue after errors
- log progress

Success criteria:

```bash
python -m iqfeed_exporter.main download --config config.yaml
```

downloads every configured continuous back-adjusted contract.

### Phase 5 — Reporting and resume support

Build:

- summary report CSV
- log file
- resume existing downloads
- failure reporting

Success criteria:

```text
1. Every run creates a report.
2. Failed symbols are captured.
3. Successful files are not corrupted.
4. App can be stopped and restarted safely.
```

---

## Features Not Needed in MVP

Because IQFeed already provides back-adjusted continuous contracts, the MVP does **not** need:

```text
roll calendar generation
manual roll schedule
raw individual contract download
contract chain builder
roll gap calculation
custom cumulative back-adjustment
continuous-contract stitching
```

These can be added later only if independent verification or custom rollover logic is required.

---

## Future Enhancements

Possible later features:

- Download raw individual contracts for audit.
- Compare IQFeed continuous data against custom-built continuous data.
- Add timezone normalization.
- Add Parquet output.
- Add database storage.
- Add scheduled daily updates.
- Add retry/backoff per symbol.
- Add data quality reports.
- Add missing-bar detection.
- Add market-session filtering.

---

## MVP Success Criteria

The first useful version is complete when:

```text
1. IQFeed Client is running and logged in.
2. App connects to 127.0.0.1:9100.
3. App downloads hourly data for at least one continuous back-adjusted symbol.
4. App saves CSV with:
   timestamp, open, high, low, close, volume, open_interest, iqfeed_symbol, name
5. App can download multiple configured symbols.
6. App can be re-run without duplicating rows.
7. Failed symbols are logged without stopping the full run.
8. Output files are written atomically.
```

---

# Separate IQFeed Session

This section is intentionally separate from the application design because IQFeed setup and operation are runtime prerequisites.

## IQFeed Runtime Requirements

Before running the exporter:

```text
1. IQFeed Client must be installed.
2. IQFeed Client must be running.
3. The IQFeed account must be logged in.
4. The machine must have access to historical futures data.
5. Continuous back-adjusted symbols must be verified in IQFeed Symbol Lookup.
```

## IQFeed Local Connection

IQFeed exposes local TCP services.

Typical ports:

```text
Lookup / Historical Data: 9100
Level 1:                  5009
Admin:                    9300
```

This app uses the lookup/history port:

```text
127.0.0.1:9100
```

## IQFeed Historical Request Concept

Hourly data uses an interval of:

```text
3600 seconds
```

Conceptual request:

```text
HIT,{symbol},3600,{begin_datetime},{end_datetime},,,1
```

Example:

```text
HIT,@ES#C,3600,20150101 000000,20261231 235959,,,1
```

The exact command format should be verified against the installed IQFeed API documentation. The implementation should isolate the IQFeed command format inside:

```text
src/iqfeed_exporter/iqfeed_client.py
```

That way, if the request format needs adjustment, only one module changes.

## IQFeed Symbol Verification

Before adding symbols to `config.yaml`, verify each symbol in IQFeed Symbol Lookup.

Examples to verify:

```text
@ES#C
@NQ#C
@CL#C
@GC#C
```

The app should treat these as configuration values, not assumptions.

## IQFeed Check Command

The exporter should include:

```bash
python -m iqfeed_exporter.main check-connection
```

This command should verify:

```text
1. Socket connection to 127.0.0.1:9100 succeeds.
2. IQFeed responds to a simple lookup/history request.
3. The response is not an error message.
```

## IQFeed Operational Notes

- IQFeed must stay running while the exporter runs.
- Long historical downloads may take time and should use socket timeouts.
- Some symbols may return no data depending on subscription permissions.
- Historical depth depends on IQFeed entitlement and symbol availability.
- Timestamps should initially be saved exactly as IQFeed returns them.
- The app should log the raw IQFeed error message when a symbol fails.

---

## Final Recommendation

Build a Python CLI exporter that reads a config file of IQFeed continuous back-adjusted futures symbols, requests hourly historical bars from IQFeed, and saves clean, deduplicated CSV files.

Final data flow:

```text
IQFeed continuous back-adjusted symbol
    -> hourly historical bars
    -> normalized rows
    -> deduplicated CSV
```
