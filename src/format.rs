//! Display strings shared by the UI and the smoke example.

use crate::imdb::ImdbStatus;
use crate::picker::Verdict;
use crate::util::time::{DAY, civil_from_days};

/// 900 -> "900 bytes", 12_345_678 -> "11.8 MB".
pub fn bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let v = n as f64;
    if v < KB {
        format!("{n} bytes")
    } else if v < MB {
        format!("{:.0} KB", v / KB)
    } else if v < GB {
        format!("{:.1} MB", v / MB)
    } else {
        format!("{:.2} GB", v / GB)
    }
}

/// Unix seconds to "9 Oct 2026" (UTC).
pub fn day(ts: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (year, month, day) = civil_from_days(ts.div_euclid(DAY));
    format!("{day} {} {year}", MONTHS[(month as usize - 1).min(11)])
}

/// How long ago something happened, from the seconds since: "just now", "5 minutes ago",
/// "1 day ago", "4 months ago". Elapsed time needs no time zone, unlike a date.
pub fn ago(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    if seconds < MINUTE {
        return "just now".to_owned();
    }
    let (count, unit) = match seconds {
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < 30 * DAY => (s / DAY, "day"),
        s if s < 365 * DAY => (s / (30 * DAY), "month"),
        s => (s / (365 * DAY), "year"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

pub const IMDB_UNAVAILABLE: &str = "IMDb rating unavailable";
pub const IMDB_LOADING: &str = "Loading IMDb ratings…";

/// 2_200_000 -> "2.2M", 45_200 -> "45K", 1_234 -> "1.2K", 980 -> "980".
pub fn votes_compact(votes: u32) -> String {
    let v = f64::from(votes);
    if votes >= 10_000_000 {
        format!("{:.0}M", v / 1e6)
    } else if votes >= 1_000_000 {
        trim_zero(format!("{:.1}", v / 1e6)) + "M"
    } else if votes >= 10_000 {
        format!("{:.0}K", (v / 1e3).floor())
    } else if votes >= 1_000 {
        trim_zero(format!("{:.1}", (v / 100.0).floor() / 10.0)) + "K"
    } else {
        votes.to_string()
    }
}

fn trim_zero(s: String) -> String {
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

/// 78 -> "7.8"
pub fn rating(rating_x10: u8) -> String {
    format!("{}.{}", rating_x10 / 10, rating_x10 % 10)
}

/// "★ 7.8 IMDb · 1.2M votes", "IMDb rating unavailable" or "Loading IMDb ratings…".
pub fn imdb_badge(status: &ImdbStatus) -> String {
    match status {
        ImdbStatus::Rated(r) => {
            let noun = if r.votes == 1 { "vote" } else { "votes" };
            format!("★ {} IMDb · {} {noun}", rating(r.rating_x10), votes_compact(r.votes))
        }
        ImdbStatus::Unavailable => IMDB_UNAVAILABLE.to_owned(),
        ImdbStatus::Loading => IMDB_LOADING.to_owned(),
    }
}

/// 136 -> "2h 16m", 45 -> "45m", 120 -> "2h"
pub fn runtime(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

pub fn seasons(count: u32) -> String {
    if count == 1 {
        "1 season".to_owned()
    } else {
        format!("{count} seasons")
    }
}

/// "Netflix", "Netflix and Stan", "Netflix, Stan and 2 more".
pub fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// The verdict line: "Included with Netflix", "Free on SBS On Demand", ...
pub fn verdict(verdict: &Verdict, region_name: &str) -> String {
    match verdict {
        Verdict::Included(names) => format!("Included with {}", join_names(names)),
        Verdict::Free(names) => format!("Free on {}", join_names(names)),
        Verdict::NotOnServices => "Not on your services".to_owned(),
        Verdict::Unavailable => format!("Not available to stream in {region_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imdb::Rating;

    #[test]
    fn compact_votes() {
        assert_eq!(votes_compact(0), "0");
        assert_eq!(votes_compact(980), "980");
        assert_eq!(votes_compact(1_000), "1K");
        assert_eq!(votes_compact(1_234), "1.2K");
        assert_eq!(votes_compact(9_999), "9.9K");
        assert_eq!(votes_compact(45_200), "45K");
        assert_eq!(votes_compact(999_999), "999K");
        assert_eq!(votes_compact(1_200_000), "1.2M");
        assert_eq!(votes_compact(2_000_000), "2M");
        assert_eq!(votes_compact(12_400_000), "12M");
    }

    #[test]
    fn badges_and_labels() {
        let rated = ImdbStatus::Rated(Rating {
            rating_x10: 78,
            votes: 1_200_000,
        });
        assert_eq!(imdb_badge(&rated), "★ 7.8 IMDb · 1.2M votes");
        assert_eq!(imdb_badge(&ImdbStatus::Unavailable), "IMDb rating unavailable");
        assert_eq!(imdb_badge(&ImdbStatus::Loading), "Loading IMDb ratings…");
        assert_eq!(rating(100), "10.0");
        assert_eq!(runtime(136), "2h 16m");
        assert_eq!(runtime(45), "45m");
        assert_eq!(runtime(120), "2h");
        assert_eq!(seasons(1), "1 season");
        assert_eq!(seasons(4), "4 seasons");
    }

    #[test]
    fn sizes_and_days() {
        assert_eq!(bytes(900), "900 bytes");
        assert_eq!(bytes(2048), "2 KB");
        assert_eq!(bytes(12_345_678), "11.8 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
        let ts = crate::util::time::parse_http_date("Fri, 09 Oct 2026 12:51:19 GMT").unwrap();
        assert_eq!(day(ts), "9 Oct 2026");
    }

    #[test]
    fn time_ago_uses_the_largest_whole_unit() {
        assert_eq!(ago(-5), "just now", "a clock that moved back");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3_599), "59 minutes ago");
        assert_eq!(ago(2 * 3_600), "2 hours ago");
        assert_eq!(ago(DAY), "1 day ago");
        assert_eq!(ago(29 * DAY), "29 days ago");
        assert_eq!(ago(45 * DAY), "1 month ago");
        assert_eq!(ago(364 * DAY), "12 months ago");
        assert_eq!(ago(400 * DAY), "1 year ago");
        assert_eq!(ago(800 * DAY), "2 years ago");
    }

    #[test]
    fn verdict_lines() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            verdict(&Verdict::Included(names(&["Netflix"])), "Australia"),
            "Included with Netflix"
        );
        assert_eq!(
            verdict(&Verdict::Free(names(&["SBS On Demand", "7plus", "9Now", "10"])), "Australia"),
            "Free on SBS On Demand, 7plus and 2 more"
        );
        assert_eq!(verdict(&Verdict::NotOnServices, "Australia"), "Not on your services");
        assert_eq!(
            verdict(&Verdict::Unavailable, "Australia"),
            "Not available to stream in Australia"
        );
    }
}
