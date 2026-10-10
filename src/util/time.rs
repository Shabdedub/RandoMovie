//! Unix time helpers and the two date formats the app needs, without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

pub const HOUR: i64 = 3600;
pub const DAY: i64 = 24 * HOUR;

/// Seconds since 1970-01-01 UTC.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 to (year, month, day), using Howard Hinnant's `civil_from_days`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

/// (year, month, day) to days since 1970-01-01, the inverse of [`civil_from_days`].
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-09T18:00:01Z`
pub fn format_utc(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(DAY));
    let secs = ts.rem_euclid(DAY);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// `2026-10-09`
pub fn format_date(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parses an HTTP date such as `Fri, 09 Oct 2026 12:51:19 GMT` into unix seconds.
pub fn parse_http_date(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }
    let day: u32 = parts[1].parse().ok()?;
    let month = MONTHS.iter().position(|m| *m == parts[2])? as u32 + 1;
    let year: i64 = parts[3].parse().ok()?;
    let mut hms = parts[4].split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, se) = (hms.next()??, hms.next()??, hms.next()??);
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * DAY + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-1, 0, 1, 59, 365, 10_957, 20_735, 50_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "day {days}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_735), (2026, 10, 9));
    }

    #[test]
    fn formats_and_parses_http_dates() {
        let ts = parse_http_date("Fri, 09 Oct 2026 12:51:19 GMT").unwrap();
        assert_eq!(format_utc(ts), "2026-10-09T12:51:19Z");
        assert_eq!(format_date(ts), "2026-10-09");
        assert_eq!(parse_http_date("Fri, 09 Foo 2026 12:51:19 GMT"), None);
        assert_eq!(parse_http_date("yesterday"), None);
    }
}
