//! The filter bar's choices and the short labels shown on its buttons.

use crate::store::Filters;

/// Minimum IMDb rating steps. `None` is "Any".
pub const RATINGS: [(Option<f32>, &str); 7] = [
    (None, "Any"),
    (Some(5.0), "5+"),
    (Some(6.0), "6+"),
    (Some(6.5), "6.5+"),
    (Some(7.0), "7+"),
    (Some(7.5), "7.5+"),
    (Some(8.0), "8+"),
];

/// Minimum IMDb vote steps.
pub const VOTES: [(Option<u32>, &str); 6] = [
    (None, "Any"),
    (Some(100), "100+"),
    (Some(1_000), "1K+"),
    (Some(5_000), "5K+"),
    (Some(10_000), "10K+"),
    (Some(50_000), "50K+"),
];

/// Maximum movie runtime steps, in minutes.
pub const RUNTIMES: [(Option<u16>, &str); 6] = [
    (None, "Any"),
    (Some(90), "90 min"),
    (Some(100), "100 min"),
    (Some(120), "2 h"),
    (Some(150), "2½ h"),
    (Some(180), "3 h"),
];

/// Earliest and latest years accepted in the year boxes.
pub const YEAR_RANGE: std::ops::RangeInclusive<u16> = 1870..=2100;

pub fn labels<T>(steps: &[(Option<T>, &'static str)]) -> Vec<&'static str> {
    steps.iter().map(|(_, label)| *label).collect()
}

/// The step for a stored rating: the highest step not above it, so a value typed into
/// settings.json between steps shows as the next lower step.
pub fn rating_index(value: Option<f32>) -> usize {
    match value {
        None => 0,
        Some(v) => RATINGS
            .iter()
            .rposition(|(step, _)| step.is_some_and(|s| s <= v + 0.001))
            .unwrap_or(1),
    }
}

pub fn votes_index(value: Option<u32>) -> usize {
    match value {
        None => 0,
        Some(v) => VOTES
            .iter()
            .rposition(|(step, _)| step.is_some_and(|s| s <= v))
            .unwrap_or(1),
    }
}

/// The step for a stored maximum runtime: the smallest step at or above it, so the filter is
/// never stricter than what was saved.
pub fn runtime_index(value: Option<u16>) -> usize {
    match value {
        None => 0,
        Some(v) => RUNTIMES
            .iter()
            .position(|(step, _)| step.is_some_and(|s| s >= v))
            .unwrap_or(RUNTIMES.len() - 1),
    }
}

/// Moves minimums and maximums typed into settings.json onto the filter bar's steps, so the bar
/// shows exactly the filter that applies.
pub fn snap_to_steps(filters: &mut Filters) {
    filters.min_imdb_rating = RATINGS[rating_index(filters.min_imdb_rating)].0;
    filters.min_imdb_votes = VOTES[votes_index(filters.min_imdb_votes)].0;
    filters.max_runtime = RUNTIMES[runtime_index(filters.max_runtime)].0;
}

/// A year typed into a year box: empty means "Any"; anything else must be a plausible year.
pub fn parse_year(text: &str) -> Option<u16> {
    let year: u16 = text.trim().parse().ok()?;
    YEAR_RANGE.contains(&year).then_some(year)
}

/// The year presets, in the order the year panel shows them.
pub const DECADES: [YearRange; 6] = [
    (Some(2020), Some(2029)),
    (Some(2010), Some(2019)),
    (Some(2000), Some(2009)),
    (Some(1990), Some(1999)),
    (Some(1980), Some(1989)),
    (None, Some(1979)),
];

/// The preset matching a year range, if any.
pub fn decade_index(from: Option<u16>, to: Option<u16>) -> Option<usize> {
    DECADES.iter().position(|&decade| decade == (from, to))
}

pub const YEAR_HINT: &str = "Enter a year between 1870 and 2100.";
pub const YEAR_ORDER_HINT: &str = "The first year can't be after the second.";

/// First and last year; `None` is open-ended.
pub type YearRange = (Option<u16>, Option<u16>);

/// The two year boxes as typed so far. `Ok(Some(range))` is a range to apply, `Ok(None)`
/// means a box holds a year that is still being typed, and `Err` explains what's wrong.
pub fn parse_year_range(from: &str, to: &str) -> Result<Option<YearRange>, &'static str> {
    enum Field {
        Empty,
        Typing,
        Year(u16),
    }
    let field = |text: &str| -> Result<Field, &'static str> {
        let text = text.trim();
        if text.is_empty() {
            Ok(Field::Empty)
        } else if !text.bytes().all(|b| b.is_ascii_digit()) {
            Err(YEAR_HINT)
        } else if text.len() < 4 {
            Ok(Field::Typing)
        } else {
            parse_year(text).map(Field::Year).ok_or(YEAR_HINT)
        }
    };
    let year = |field: &Field| match field {
        Field::Year(year) => Some(*year),
        _ => None,
    };
    let (from, to) = (field(from)?, field(to)?);
    if matches!(from, Field::Typing) || matches!(to, Field::Typing) {
        return Ok(None);
    }
    match (year(&from), year(&to)) {
        (Some(from), Some(to)) if from > to => Err(YEAR_ORDER_HINT),
        range => Ok(Some(range)),
    }
}

pub fn genre_summary(genres: &[String]) -> String {
    match genres {
        [] => "Any genre".to_owned(),
        [one] => one.clone(),
        [a, b] => format!("{a}, {b}"),
        more => format!("{} genres", more.len()),
    }
}

/// "IMDb 6+ · 1K+ votes", "IMDb 7+", "IMDb 1K+ votes" or "Any IMDb rating".
pub fn imdb_summary(filters: &Filters) -> String {
    let rating = RATINGS[rating_index(filters.min_imdb_rating)].1;
    let votes = VOTES[votes_index(filters.min_imdb_votes)].1;
    match (filters.min_imdb_rating.is_some(), filters.min_imdb_votes.is_some()) {
        (false, false) => "Any IMDb rating".to_owned(),
        (true, false) => format!("IMDb {rating}"),
        (false, true) => format!("IMDb {votes} votes"),
        (true, true) => format!("IMDb {rating} · {votes} votes"),
    }
}

/// "Any year", "1990–1999", "1990 or later", "1999 or earlier" or "1999".
pub fn year_summary(from: Option<u16>, to: Option<u16>) -> String {
    match (from, to) {
        (None, None) => "Any year".to_owned(),
        (Some(from), None) => format!("{from} or later"),
        (None, Some(to)) => format!("{to} or earlier"),
        (Some(from), Some(to)) if from == to => from.to_string(),
        (Some(from), Some(to)) => format!("{from}–{to}"),
    }
}

/// "Any length" or "Up to 2 h".
pub fn runtime_summary(max: Option<u16>) -> String {
    match max {
        None => "Any length".to_owned(),
        Some(_) => format!("Up to {}", RUNTIMES[runtime_index(max)].1),
    }
}

/// True when any filter differs from its default, so "Reset" is worth showing.
pub fn differs_from_default(filters: &Filters) -> bool {
    *filters != Filters::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_follow_steps() {
        assert_eq!(rating_index(None), 0);
        assert_eq!(rating_index(Some(6.0)), 2);
        assert_eq!(rating_index(Some(6.7)), 3, "between steps rounds down");
        assert_eq!(rating_index(Some(9.5)), 6);
        assert_eq!(rating_index(Some(1.0)), 1);
        assert_eq!(votes_index(Some(1_000)), 2);
        assert_eq!(votes_index(Some(7_000)), 3);
        assert_eq!(votes_index(Some(10)), 1);
        assert_eq!(runtime_index(None), 0);
        assert_eq!(runtime_index(Some(120)), 3);
        assert_eq!(runtime_index(Some(110)), 3, "never stricter than saved");
        assert_eq!(runtime_index(Some(240)), 5);
    }

    #[test]
    fn hand_edited_values_snap_to_the_steps_shown() {
        let mut f = Filters {
            min_imdb_rating: Some(6.7),
            min_imdb_votes: Some(7_000),
            max_runtime: Some(110),
            ..Filters::default()
        };
        snap_to_steps(&mut f);
        assert_eq!((f.min_imdb_rating, f.min_imdb_votes, f.max_runtime), (Some(6.5), Some(5_000), Some(120)));
        let mut any = Filters {
            min_imdb_rating: None,
            min_imdb_votes: None,
            ..Filters::default()
        };
        snap_to_steps(&mut any);
        assert_eq!((any.min_imdb_rating, any.min_imdb_votes, any.max_runtime), (None, None, None));
    }

    #[test]
    fn years_parse_and_summarise() {
        assert_eq!(parse_year(" 1999 "), Some(1999));
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("99999"), None);
        assert_eq!(parse_year("abc"), None);
        assert_eq!(parse_year("1500"), None);
        assert_eq!(year_summary(None, None), "Any year");
        assert_eq!(year_summary(Some(1990), Some(1999)), "1990–1999");
        assert_eq!(year_summary(Some(1990), None), "1990 or later");
        assert_eq!(year_summary(None, Some(1999)), "1999 or earlier");
        assert_eq!(year_summary(Some(2001), Some(2001)), "2001");
    }

    #[test]
    fn year_boxes_apply_only_complete_years() {
        assert_eq!(parse_year_range("", ""), Ok(Some((None, None))));
        assert_eq!(parse_year_range("1990", ""), Ok(Some((Some(1990), None))));
        assert_eq!(parse_year_range(" 1990 ", "1999"), Ok(Some((Some(1990), Some(1999)))));
        assert_eq!(parse_year_range("199", "1999"), Ok(None), "still typing");
        assert_eq!(parse_year_range("", "20"), Ok(None));
        assert_eq!(parse_year_range("1500", ""), Err(YEAR_HINT));
        assert_eq!(parse_year_range("19x0", ""), Err(YEAR_HINT));
        assert_eq!(parse_year_range("20000", ""), Err(YEAR_HINT));
        assert_eq!(parse_year_range("2000", "1990"), Err(YEAR_ORDER_HINT));
    }

    #[test]
    fn decades_match_their_ranges() {
        assert_eq!(decade_index(Some(1990), Some(1999)), Some(3));
        assert_eq!(decade_index(None, Some(1979)), Some(5));
        assert_eq!(decade_index(Some(1990), Some(1998)), None);
        assert_eq!(decade_index(None, None), None);
        for (from, to) in DECADES {
            assert!(from.is_none_or(|f| to.is_some_and(|t| f < t)));
        }
    }

    #[test]
    fn summaries() {
        let defaults = Filters::default();
        assert_eq!(imdb_summary(&defaults), "IMDb 6+ · 1K+ votes");
        assert!(!differs_from_default(&defaults));
        let any = Filters {
            min_imdb_rating: None,
            min_imdb_votes: None,
            ..Filters::default()
        };
        assert_eq!(imdb_summary(&any), "Any IMDb rating");
        assert!(differs_from_default(&any));
        let rating_only = Filters {
            min_imdb_votes: None,
            ..Filters::default()
        };
        assert_eq!(imdb_summary(&rating_only), "IMDb 6+");
        assert_eq!(genre_summary(&[]), "Any genre");
        assert_eq!(genre_summary(&["Comedy".into(), "Drama".into()]), "Comedy, Drama");
        assert_eq!(
            genre_summary(&["Comedy".into(), "Drama".into(), "Horror".into()]),
            "3 genres"
        );
        assert_eq!(runtime_summary(Some(120)), "Up to 2 h");
        assert_eq!(runtime_summary(None), "Any length");
        assert_eq!(labels(&RATINGS)[2], "6+");
    }
}
