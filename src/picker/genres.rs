//! One genre list for both media types.
//!
//! TMDB's movie and TV genres differ ("Science Fiction" vs "Sci-Fi & Fantasy", no "Horror"
//! for TV). The filter bar shows one list; each entry maps to the matching ids per type.

use crate::media::MediaType;
use crate::tmdb::model::Genre;

/// TV genre name parts that mean a movie genre.
const ALIASES: [(&str, &str); 1] = [("Sci-Fi", "Science Fiction")];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedGenre {
    pub name: String,
    pub movie_ids: Vec<u32>,
    pub tv_ids: Vec<u32>,
}

/// Builds the unified list, sorted by name. TV genres such as "Action & Adventure" are
/// split on " & " so they join both "Action" and "Adventure".
pub fn unify(movie: &[Genre], tv: &[Genre]) -> Vec<UnifiedGenre> {
    let mut list: Vec<UnifiedGenre> = movie
        .iter()
        .map(|g| UnifiedGenre {
            name: g.name.clone(),
            movie_ids: vec![g.id],
            tv_ids: Vec::new(),
        })
        .collect();
    for genre in tv {
        for part in genre.name.split(" & ") {
            let part = part.trim();
            let name = ALIASES
                .iter()
                .find(|(alias, _)| alias.eq_ignore_ascii_case(part))
                .map_or(part, |(_, canonical)| canonical);
            match list.iter_mut().find(|u| u.name.eq_ignore_ascii_case(name)) {
                Some(unified) => {
                    if !unified.tv_ids.contains(&genre.id) {
                        unified.tv_ids.push(genre.id);
                    }
                }
                None => list.push(UnifiedGenre {
                    name: name.to_owned(),
                    movie_ids: Vec::new(),
                    tv_ids: vec![genre.id],
                }),
            }
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// Genre ids for `media` matching the chosen names.
/// `None`: no genre filter. `Some(empty)`: this media type can't match the choice.
/// Unknown names are ignored; if none are known, there is no filter.
pub fn ids_for(genres: &[UnifiedGenre], chosen: &[String], media: MediaType) -> Option<Vec<u32>> {
    let known: Vec<&UnifiedGenre> = chosen
        .iter()
        .filter_map(|name| genres.iter().find(|g| g.name.eq_ignore_ascii_case(name)))
        .collect();
    if known.is_empty() {
        return None;
    }
    let mut ids: Vec<u32> = known
        .iter()
        .flat_map(|g| match media {
            MediaType::Movie => g.movie_ids.iter().copied(),
            MediaType::Tv => g.tv_ids.iter().copied(),
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmdb::model::GenreList;

    fn unified() -> Vec<UnifiedGenre> {
        let movie: GenreList =
            serde_json::from_str(include_str!("../../tests/fixtures/genres_movie.json")).unwrap();
        let tv: GenreList =
            serde_json::from_str(include_str!("../../tests/fixtures/genres_tv.json")).unwrap();
        unify(&movie.genres, &tv.genres)
    }

    fn find<'a>(list: &'a [UnifiedGenre], name: &str) -> &'a UnifiedGenre {
        list.iter().find(|g| g.name == name).unwrap()
    }

    #[test]
    fn maps_movie_and_tv_genres() {
        let list = unified();
        assert_eq!(find(&list, "Action").movie_ids, [28]);
        assert_eq!(find(&list, "Action").tv_ids, [10759]);
        assert_eq!(find(&list, "Adventure").tv_ids, [10759]);
        assert_eq!(find(&list, "Science Fiction").movie_ids, [878]);
        assert_eq!(find(&list, "Science Fiction").tv_ids, [10765]);
        assert_eq!(find(&list, "Fantasy").tv_ids, [10765]);
        assert_eq!(find(&list, "Comedy").tv_ids, [35]);
        assert!(find(&list, "Horror").tv_ids.is_empty());
        assert!(find(&list, "Kids").movie_ids.is_empty());
        assert_eq!(find(&list, "Kids").tv_ids, [10762]);
        assert_eq!(find(&list, "Politics").tv_ids, [10768]);
        assert!(list.windows(2).all(|w| w[0].name < w[1].name), "sorted");
    }

    #[test]
    fn chosen_genres_map_per_type() {
        let list = unified();
        let chosen = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(ids_for(&list, &[], MediaType::Movie), None);
        assert_eq!(
            ids_for(&list, &chosen(&["Action", "Science Fiction"]), MediaType::Movie),
            Some(vec![28, 878])
        );
        assert_eq!(
            ids_for(&list, &chosen(&["Action", "Science Fiction"]), MediaType::Tv),
            Some(vec![10759, 10765])
        );
        assert_eq!(
            ids_for(&list, &chosen(&["Horror"]), MediaType::Tv),
            Some(vec![]),
            "series can't match Horror"
        );
        assert_eq!(ids_for(&list, &chosen(&["Not A Genre"]), MediaType::Tv), None);
    }
}
