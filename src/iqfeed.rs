//! IQFeed lookup-socket client. This is the only module that knows the wire format.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;
use thiserror::Error;
use tracing::debug;

use crate::model::Timeframe;
use crate::parser::{classify, Line};

const PROTOCOL: &str = "6.2";
const HOURLY_INTERVAL_SECONDS: u32 = 3600;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("no data returned")]
    NoData,
    #[error("IQFeed error: {0}")]
    Server(String),
    #[error("connection closed before end of response ({0} rows received)")]
    Partial(usize),
    #[error("timed out waiting for IQFeed")]
    Timeout,
    #[error("socket error: {0}")]
    Io(#[from] io::Error),
}

impl FetchError {
    /// After these the socket may hold stale bytes, so the caller must reconnect.
    pub fn connection_unusable(&self) -> bool {
        matches!(self, FetchError::Partial(_) | FetchError::Timeout | FetchError::Io(_))
    }
}

pub fn build_request(
    symbol: &str,
    tf: Timeframe,
    begin: NaiveDate,
    end: NaiveDate,
    req_id: &str,
) -> String {
    match tf {
        // HDT,symbol,begindate,enddate,maxdatapoints,direction(1=oldest first),reqid
        // IQFeed rejects a time part here ("BeginDate must be ... YYYYMMDD").
        Timeframe::Daily => {
            let (b, e) = (begin.format("%Y%m%d"), end.format("%Y%m%d"));
            format!("HDT,{symbol},{b},{e},,1,{req_id}\r\n")
        }
        // HIT,symbol,interval,begin,end,maxdatapoints,beginfilter,endfilter,direction,reqid
        Timeframe::Hourly => {
            let (b, e) = (begin.format("%Y%m%d 000000"), end.format("%Y%m%d 235959"));
            format!("HIT,{symbol},{HOURLY_INTERVAL_SECONDS},{b},{e},,,,1,{req_id}\r\n")
        }
    }
}

pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    seq: u32,
}

impl Client {
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        let addr = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("cannot resolve {host}:{port}"))?
            .next()
            .ok_or_else(|| anyhow!("no address for {host}:{port}"))?;
        let stream = TcpStream::connect_timeout(&addr, timeout).with_context(|| {
            format!("cannot connect to IQFeed lookup port {host}:{port} - is IQConnect running and logged in?")
        })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        let writer = stream.try_clone()?;
        let mut client = Client { reader: BufReader::new(stream), writer, seq: 0 };
        client.set_protocol()?;
        Ok(client)
    }

    fn set_protocol(&mut self) -> Result<()> {
        self.writer
            .write_all(format!("S,SET PROTOCOL,{PROTOCOL}\r\n").as_bytes())?;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self
                .reader
                .read_line(&mut buf)
                .context("waiting for IQFeed protocol reply")?;
            if n == 0 {
                bail!("IQFeed closed the connection during the protocol handshake");
            }
            let line = buf.trim();
            debug!("handshake: {line}");
            if line.starts_with("S,CURRENT PROTOCOL") {
                if !line.ends_with(PROTOCOL) {
                    bail!("IQFeed replied '{line}', expected protocol {PROTOCOL}");
                }
                return Ok(());
            }
            if line.starts_with("E,") {
                bail!("IQFeed rejected protocol {PROTOCOL}: {line}");
            }
        }
    }

    /// Request bars and return the raw data rows (request id stripped, no end marker).
    pub fn fetch(
        &mut self,
        symbol: &str,
        tf: Timeframe,
        begin: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<String>, FetchError> {
        self.seq += 1;
        let req_id = format!("DL{}", self.seq);
        let request = build_request(symbol, tf, begin, end, &req_id);
        debug!("-> {}", request.trim_end());
        self.writer.write_all(request.as_bytes())?;

        let mut rows = Vec::new();
        let mut server_error: Option<String> = None;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = match self.reader.read_line(&mut buf) {
                Ok(n) => n,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Err(FetchError::Timeout)
                }
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                return Err(match server_error {
                    Some(m) => server_err(m),
                    None => FetchError::Partial(rows.len()),
                });
            }
            let line = buf.trim_end_matches(['\r', '\n']);
            match classify(line, &req_id) {
                Line::Data(row) => rows.push(row.to_string()),
                Line::End => break,
                // Keep reading to the end marker so the socket stays in sync.
                Line::Error(m) => {
                    server_error.get_or_insert(m);
                }
                Line::Other => debug!("ignored: {line}"),
            }
        }
        if let Some(m) = server_error {
            return Err(server_err(m));
        }
        if rows.is_empty() {
            return Err(FetchError::NoData);
        }
        Ok(rows)
    }
}

fn server_err(msg: String) -> FetchError {
    if msg.contains("NO_DATA") {
        FetchError::NoData
    } else {
        FetchError::Server(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn builds_daily_request() {
        let r = build_request("@ES#C", Timeframe::Daily, d(2015, 1, 1), d(2026, 12, 31), "DL1");
        assert_eq!(r, "HDT,@ES#C,20150101,20261231,,1,DL1\r\n");
    }

    #[test]
    fn builds_hourly_request() {
        let r = build_request("@ES#C", Timeframe::Hourly, d(2015, 1, 1), d(2026, 12, 31), "DL2");
        assert_eq!(r, "HIT,@ES#C,3600,20150101 000000,20261231 235959,,,,1,DL2\r\n");
    }

    use std::net::TcpListener;
    use std::thread;

    /// Minimal fake IQConnect: answers the protocol handshake, then replies to each request
    /// with the next scripted reply (`{id}` is replaced by the request id). After the script
    /// it holds the socket open for `hold` and then closes it.
    fn fake_iqconnect(script: Vec<Vec<&'static str>>, hold: Duration) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writer.write_all(b"S,CURRENT PROTOCOL,6.2\r\n").unwrap();
            for reply in script {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                let id = line.trim().rsplit(',').next().unwrap().to_string();
                for l in reply {
                    writer.write_all(l.replace("{id}", &id).as_bytes()).unwrap();
                    writer.write_all(b"\r\n").unwrap();
                }
            }
            thread::sleep(hold);
        });
        port
    }

    fn connect(port: u16, timeout_ms: u64) -> Client {
        Client::connect("127.0.0.1", port, Duration::from_millis(timeout_ms)).unwrap()
    }

    fn fetch(client: &mut Client) -> Result<Vec<String>, FetchError> {
        client.fetch("@ES#C", Timeframe::Hourly, d(2026, 9, 1), d(2026, 9, 18))
    }

    #[test]
    fn fetch_strips_id_lh_tag_and_trailing_commas() {
        let port = fake_iqconnect(
            vec![vec![
                "{id},LH,2026-09-18 15:00:00,7718.25,7704.25,7709.75,7712.25,1212774,228383,0,",
                "{id},LH,2026-09-18 16:00:00,7729.25,7711.50,7712.25,7725.00,1288568,75783,0,",
                "{id},!ENDMSG!,",
            ]],
            Duration::ZERO,
        );
        let rows = fetch(&mut connect(port, 2000)).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], "2026-09-18 16:00:00,7729.25,7711.50,7712.25,7725.00,1288568,75783,0");
    }

    #[test]
    fn server_error_is_reported_and_connection_stays_usable() {
        let port = fake_iqconnect(
            vec![
                vec!["{id},E,Unknown Server Error code 0.,", "{id},!ENDMSG!,"],
                vec!["{id},E,!NO_DATA!,", "{id},!ENDMSG!,"],
                vec!["{id},LH,2026-09-18 16:00:00,2,1,1.5,1.8,10,5,0,", "{id},!ENDMSG!,"],
            ],
            Duration::from_millis(500),
        );
        let mut client = connect(port, 2000);
        let err = fetch(&mut client).unwrap_err();
        assert!(matches!(&err, FetchError::Server(m) if m.contains("Unknown Server Error")), "{err:?}");
        assert!(!err.connection_unusable());
        assert!(matches!(fetch(&mut client), Err(FetchError::NoData)));
        assert_eq!(fetch(&mut client).unwrap().len(), 1);
    }

    #[test]
    fn closed_connection_mid_response_is_a_partial_error() {
        let port = fake_iqconnect(
            vec![vec!["{id},LH,2026-09-18 16:00:00,2,1,1.5,1.8,10,5,0,"]],
            Duration::ZERO,
        );
        let err = fetch(&mut connect(port, 2000)).unwrap_err();
        assert!(matches!(err, FetchError::Partial(1)), "{err:?}");
        assert!(err.connection_unusable());
    }

    #[test]
    fn stalled_response_times_out() {
        let port = fake_iqconnect(
            vec![vec!["{id},LH,2026-09-18 16:00:00,2,1,1.5,1.8,10,5,0,"]],
            Duration::from_millis(1500),
        );
        let err = fetch(&mut connect(port, 300)).unwrap_err();
        assert!(matches!(err, FetchError::Timeout), "{err:?}");
        assert!(err.connection_unusable());
    }

    #[test]
    fn maps_no_data_error() {
        assert!(matches!(server_err("!NO_DATA!".into()), FetchError::NoData));
        assert!(matches!(server_err("Invalid symbol".into()), FetchError::Server(_)));
    }
}
