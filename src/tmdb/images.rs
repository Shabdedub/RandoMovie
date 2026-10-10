//! TMDB image sizes. The app always asks for the smallest size that covers the pixels it
//! will draw, so nothing larger than needed is downloaded or decoded.
//!
//! Sizes come from TMDB's `/configuration` (recorded in `tests/fixtures/configuration.json`).

/// TMDB's image CDN; image requests carry no credentials.
pub const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Poster,
    Backdrop,
    Logo,
}

const POSTER_SIZES: [(u32, &str); 6] = [
    (92, "w92"),
    (154, "w154"),
    (185, "w185"),
    (342, "w342"),
    (500, "w500"),
    (780, "w780"),
];
const BACKDROP_SIZES: [(u32, &str); 3] = [(300, "w300"), (780, "w780"), (1280, "w1280")];
const LOGO_SIZES: [(u32, &str); 6] = [
    (45, "w45"),
    (92, "w92"),
    (154, "w154"),
    (185, "w185"),
    (300, "w300"),
    (500, "w500"),
];

/// The smallest size at least `physical_width` pixels wide, or `original` if none is.
pub fn size_for(kind: ImageKind, physical_width: u32) -> &'static str {
    let sizes: &[(u32, &str)] = match kind {
        ImageKind::Poster => &POSTER_SIZES,
        ImageKind::Backdrop => &BACKDROP_SIZES,
        ImageKind::Logo => &LOGO_SIZES,
    };
    sizes
        .iter()
        .find(|(width, _)| *width >= physical_width)
        .map_or("original", |(_, name)| name)
}

/// Full image URL for a TMDB path such as `/dXNAPwY7VrqMAo51EKhhCJfaGb5.jpg`.
pub fn image_url(size: &str, path: &str) -> String {
    format!("{IMAGE_BASE}{size}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_smallest_covering_size() {
        assert_eq!(size_for(ImageKind::Poster, 40), "w92");
        assert_eq!(size_for(ImageKind::Poster, 92), "w92");
        assert_eq!(size_for(ImageKind::Poster, 93), "w154");
        // A 240 px poster at 150 % scaling needs 360 physical pixels.
        assert_eq!(size_for(ImageKind::Poster, 360), "w500");
        assert_eq!(size_for(ImageKind::Poster, 2000), "original");
        assert_eq!(size_for(ImageKind::Backdrop, 160), "w300");
        assert_eq!(size_for(ImageKind::Logo, 48), "w92");
        assert_eq!(size_for(ImageKind::Logo, 32), "w45");
    }

    #[test]
    fn sizes_match_tmdb_configuration() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/configuration.json")).unwrap();
        let listed = |key: &str| -> Vec<String> {
            config["images"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        };
        for (_, name) in POSTER_SIZES {
            assert!(listed("poster_sizes").contains(&name.to_owned()), "{name}");
        }
        for (_, name) in BACKDROP_SIZES {
            assert!(listed("backdrop_sizes").contains(&name.to_owned()), "{name}");
        }
        for (_, name) in LOGO_SIZES {
            assert!(listed("logo_sizes").contains(&name.to_owned()), "{name}");
        }
        assert_eq!(
            config["images"]["secure_base_url"].as_str(),
            Some(IMAGE_BASE)
        );
    }
}
