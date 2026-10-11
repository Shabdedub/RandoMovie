//! The Library's filters: Movies, Series or both, and genres, over the titles marked "Seen it"
//! or "Not interested". Genres use the unified names of the Pick screen's genre filter.

use crate::media::MediaType;
use crate::picker::genres::unified_names;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryFilter {
    /// `None` shows movies and series.
    pub media: Option<MediaType>,
    /// Unified genre names; a title in any of them matches. Empty shows every genre.
    pub genres: Vec<String>,
}

impl LibraryFilter {
    pub fn is_active(&self) -> bool {
        self.media.is_some() || !self.genres.is_empty()
    }

    /// Whether a title shows. `genres` are its TMDB genre names, `None` while they're
    /// unknown: such a title shows unless genres are chosen.
    pub fn matches(&self, media: MediaType, genres: Option<&[String]>) -> bool {
        if self.media.is_some_and(|chosen| chosen != media) {
            return false;
        }
        if self.genres.is_empty() {
            return true;
        }
        genres.is_some_and(|genres| {
            unified_names(genres)
                .iter()
                .any(|name| self.genres.iter().any(|chosen| chosen.eq_ignore_ascii_case(name)))
        })
    }

    /// Chooses a genre, or unchooses it if it was chosen.
    pub fn toggle_genre(&mut self, name: &str) {
        match self.genres.iter().position(|chosen| chosen.eq_ignore_ascii_case(name)) {
            Some(index) => {
                self.genres.remove(index);
            }
            None => self.genres.push(name.to_owned()),
        }
    }
}

/// The genre choices: every unified genre the titles have, plus any chosen one no title has
/// any more, sorted by name.
pub fn genre_options<'a>(titles: impl IntoIterator<Item = &'a [String]>, chosen: &[String]) -> Vec<String> {
    let mut options: Vec<String> = Vec::new();
    let names = titles.into_iter().flat_map(unified_names).chain(chosen.iter().cloned());
    for name in names {
        if !options.iter().any(|option| option.eq_ignore_ascii_case(&name)) {
            options.push(name);
        }
    }
    options.sort();
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn type_and_genre_filters_combine() {
        let thriller = names(&["Horror", "Thriller"]);
        let scifi_series = names(&["Sci-Fi & Fantasy", "Drama"]);
        let any = LibraryFilter::default();
        assert!(!any.is_active());
        assert!(any.matches(MediaType::Movie, Some(&thriller)));
        assert!(any.matches(MediaType::Tv, None), "unknown genres show without a genre filter");

        let series = LibraryFilter {
            media: Some(MediaType::Tv),
            genres: Vec::new(),
        };
        assert!(series.is_active());
        assert!(!series.matches(MediaType::Movie, Some(&thriller)));
        assert!(series.matches(MediaType::Tv, Some(&scifi_series)));

        let mut genres = LibraryFilter::default();
        genres.toggle_genre("Science Fiction");
        assert!(genres.matches(MediaType::Tv, Some(&scifi_series)), "Sci-Fi & Fantasy counts");
        assert!(!genres.matches(MediaType::Movie, Some(&thriller)));
        assert!(!genres.matches(MediaType::Tv, None), "unknown genres don't match a choice");
        genres.toggle_genre("thriller");
        assert!(genres.matches(MediaType::Movie, Some(&thriller)), "any chosen genre, any case");
        genres.toggle_genre("Science Fiction");
        assert_eq!(genres.genres, ["thriller"], "choosing again unchooses");
    }

    #[test]
    fn options_come_from_the_titles() {
        let titles = [names(&["Horror", "Thriller"]), names(&["Sci-Fi & Fantasy"]), names(&["Thriller"])];
        let options = genre_options(titles.iter().map(Vec::as_slice), &names(&["Western"]));
        assert_eq!(options, names(&["Fantasy", "Horror", "Science Fiction", "Thriller", "Western"]));
        assert!(genre_options(std::iter::empty::<&[String]>(), &[]).is_empty());
    }
}
