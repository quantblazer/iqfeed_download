//! Build daily bars from hourly bars, used while IQFeed's native daily history is unavailable.
//!
//! A futures "trade day" starts at `session_start` (Eastern, like IQFeed's timestamps) on the
//! previous calendar day, e.g. the CME Globex day runs 18:00 -> 17:00 ET and is labelled with
//! the date it ends on. Sunday 18:00 therefore belongs to Monday's bar.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, NaiveTime, Timelike};

use crate::model::Bar;

pub fn hourly_to_daily(hourly: &[Bar], session_start: NaiveTime) -> Vec<Bar> {
    // Shifting by (24h - start) moves the session start to midnight of the trade date.
    let shift_secs = (86_400 - i64::from(session_start.num_seconds_from_midnight())) % 86_400;
    let shift = Duration::seconds(shift_secs);

    let mut days: BTreeMap<NaiveDate, Vec<&Bar>> = BTreeMap::new();
    for bar in hourly {
        days.entry((bar.ts + shift).date()).or_default().push(bar);
    }

    days.into_iter()
        .map(|(date, mut bars)| {
            bars.sort_by_key(|b| b.ts);
            Bar {
                ts: date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
                open: bars[0].open,
                high: bars.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max),
                low: bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min),
                close: bars[bars.len() - 1].close,
                volume: bars.iter().map(|b| b.volume).sum(),
                // IQFeed's interval bars carry no open interest.
                open_interest: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hb(day: u32, hour: u32, o: f64, h: f64, l: f64, c: f64, v: u64) -> Bar {
        Bar {
            ts: NaiveDate::from_ymd_opt(2026, 9, day).unwrap().and_hms_opt(hour, 0, 0).unwrap(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
            open_interest: None,
        }
    }

    fn t(h: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, 0, 0).unwrap()
    }

    #[test]
    fn globex_day_starts_previous_evening_and_is_labelled_by_end_date() {
        // Thu 17 Sep 18:00 .. Fri 18 Sep 16:00 is Friday's trade day (18 Sep).
        let bars = vec![
            hb(17, 18, 100.0, 105.0, 99.0, 104.0, 10), // evening open -> Fri
            hb(18, 9, 104.0, 110.0, 103.0, 108.0, 20),
            hb(18, 16, 108.0, 109.0, 95.0, 96.0, 30), // last bar of the day
            hb(20, 18, 96.0, 97.0, 90.0, 91.0, 5),    // Sunday 18:00 -> Monday 21 Sep
            hb(21, 10, 91.0, 99.0, 90.5, 98.0, 7),
        ];
        let daily = hourly_to_daily(&bars, t(18));
        assert_eq!(daily.len(), 2);

        let fri = &daily[0];
        assert_eq!(fri.ts.date(), NaiveDate::from_ymd_opt(2026, 9, 18).unwrap());
        assert_eq!((fri.open, fri.high, fri.low, fri.close, fri.volume), (100.0, 110.0, 95.0, 96.0, 60));
        assert_eq!(fri.open_interest, None);

        let mon = &daily[1];
        assert_eq!(mon.ts.date(), NaiveDate::from_ymd_opt(2026, 9, 21).unwrap());
        assert_eq!((mon.open, mon.high, mon.low, mon.close, mon.volume), (96.0, 99.0, 90.0, 98.0, 12));
    }

    #[test]
    fn midnight_session_start_groups_by_calendar_day() {
        let bars = vec![hb(18, 0, 1.0, 2.0, 0.5, 1.5, 1), hb(18, 23, 1.5, 3.0, 1.0, 2.0, 1), hb(19, 0, 2.0, 2.5, 1.5, 2.2, 1)];
        let daily = hourly_to_daily(&bars, t(0));
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[0].high, 3.0);
        assert_eq!(daily[1].open, 2.0);
    }

    #[test]
    fn empty_input_gives_no_bars() {
        assert!(hourly_to_daily(&[], t(18)).is_empty());
    }
}
