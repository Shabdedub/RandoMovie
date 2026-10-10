//! The user's country, from Windows settings.
//!
//! Windows' "Country or region" (`GetUserDefaultGeoName`) comes first, then the country of
//! the regional format (`GetUserDefaultLocaleName`, for example en-AU), then AU. Each is
//! checked against TMDB's streaming regions once that list is known, because "Country or
//! region" is sometimes left on a place TMDB has no data for.

use windows::Win32::Globalization::{GetUserDefaultGeoName, GetUserDefaultLocaleName};

/// Used when nothing better is known.
pub const FALLBACK_REGION: &str = "AU";

/// TMDB's streaming regions as of October 2026 (`/watch/providers/regions`). Used to choose a
/// region before the live list has loaded, for example on a first run without a connection.
pub const KNOWN_TMDB_REGIONS: [&str; 139] = [
    "AD", "AE", "AG", "AL", "AO", "AR", "AT", "AU", "AZ", "BA", "BB", "BE", "BF", "BG", "BH", "BM",
    "BO", "BR", "BS", "BY", "BZ", "CA", "CD", "CH", "CI", "CL", "CM", "CO", "CR", "CU", "CV", "CY",
    "CZ", "DE", "DK", "DO", "DZ", "EC", "EE", "EG", "ES", "FI", "FJ", "FR", "GB", "GF", "GH", "GI",
    "GP", "GQ", "GR", "GT", "GY", "HK", "HN", "HR", "HU", "ID", "IE", "IL", "IN", "IQ", "IS", "IT",
    "JM", "JO", "JP", "KE", "KR", "KW", "LB", "LC", "LI", "LT", "LU", "LV", "LY", "MA", "MC", "MD",
    "ME", "MG", "MK", "ML", "MT", "MU", "MW", "MX", "MY", "MZ", "NE", "NG", "NI", "NL", "NO", "NZ",
    "OM", "PA", "PE", "PF", "PG", "PH", "PK", "PL", "PS", "PT", "PY", "QA", "RO", "RS", "RU", "SA",
    "SC", "SE", "SG", "SI", "SK", "SM", "SN", "SV", "TC", "TD", "TH", "TN", "TR", "TT", "TW", "TZ",
    "UA", "UG", "US", "UY", "VA", "VE", "XK", "YE", "ZA", "ZM", "ZW",
];

/// `KNOWN_TMDB_REGIONS` as owned strings, for `choose`.
pub fn known_tmdb_regions() -> Vec<String> {
    KNOWN_TMDB_REGIONS.iter().map(|c| (*c).to_owned()).collect()
}

fn is_country_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase())
}

/// Windows' "Country or region" as an ISO 3166-1 code. `None` when Windows reports a UN
/// M.49 area code (such as "001" for World) or nothing.
pub fn geo_country() -> Option<String> {
    let mut buffer = [0u16; 16];
    let written = unsafe { GetUserDefaultGeoName(&mut buffer) };
    if written <= 1 {
        return None;
    }
    let len = (written as usize - 1).min(buffer.len());
    let code = String::from_utf16_lossy(&buffer[..len]);
    is_country_code(&code).then_some(code)
}

/// The country of the regional format, such as AU for en-AU.
pub fn locale_country() -> Option<String> {
    let mut buffer = [0u16; 85];
    let written = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    if written <= 1 {
        return None;
    }
    let len = (written as usize - 1).min(buffer.len());
    country_from_locale(&String::from_utf16_lossy(&buffer[..len]))
}

/// The country part of a locale name: "en-AU" gives AU, "zh-Hans-CN" gives CN, "en" none.
pub fn country_from_locale(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('-').collect();
    parts
        .iter()
        .skip(1)
        .rev()
        .find(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(|part| part.to_ascii_uppercase())
}

/// Region candidates in order of preference, without duplicates, ending with AU.
pub fn candidates() -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for code in [geo_country(), locale_country(), Some(FALLBACK_REGION.to_owned())]
        .into_iter()
        .flatten()
    {
        if !list.contains(&code) {
            list.push(code);
        }
    }
    list
}

/// The first candidate TMDB has streaming data for. With no list yet, the first candidate.
pub fn choose(candidates: &[String], supported: &[String]) -> String {
    candidates
        .iter()
        .find(|c| supported.is_empty() || supported.contains(c))
        .or(candidates.first())
        .cloned()
        .unwrap_or_else(|| FALLBACK_REGION.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_names_give_countries() {
        assert_eq!(country_from_locale("en-AU").as_deref(), Some("AU"));
        assert_eq!(country_from_locale("zh-Hans-CN").as_deref(), Some("CN"));
        assert_eq!(country_from_locale("sr-Latn-rs").as_deref(), Some("RS"));
        assert_eq!(country_from_locale("en"), None);
        assert_eq!(country_from_locale("es-419"), None);
    }

    #[test]
    fn chooses_the_first_supported_candidate() {
        let candidates = vec!["SX".to_owned(), "AU".to_owned()];
        let supported = vec!["AU".to_owned(), "NZ".to_owned()];
        assert_eq!(choose(&candidates, &supported), "AU");
        assert_eq!(choose(&candidates, &[]), "SX", "no list yet: trust Windows");
        assert_eq!(choose(&candidates, &["GB".to_owned()]), "SX", "none supported");
        assert_eq!(choose(&[], &supported), "AU");
    }

    #[test]
    fn built_in_list_matches_the_recorded_tmdb_list() {
        let recorded: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/regions.json")).unwrap();
        let mut codes: Vec<&str> = recorded["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["iso_3166_1"].as_str().unwrap())
            .collect();
        codes.sort_unstable();
        assert_eq!(codes, KNOWN_TMDB_REGIONS);
        assert!(KNOWN_TMDB_REGIONS.contains(&FALLBACK_REGION));
        assert_eq!(
            choose(&["SX".to_owned(), "AU".to_owned()], &known_tmdb_regions()),
            "AU"
        );
    }

    #[test]
    fn candidates_end_with_the_fallback() {
        let list = candidates();
        assert!(!list.is_empty());
        assert!(list.iter().all(|c| is_country_code(c)));
        assert_eq!(list.iter().filter(|c| *c == FALLBACK_REGION).count(), 1);
    }
}
