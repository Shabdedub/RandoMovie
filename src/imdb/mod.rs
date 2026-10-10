//! IMDb ratings from IMDb's non-commercial dataset `title.ratings.tsv.gz`.

pub mod dataset;
pub mod index;

pub use dataset::{DatasetMeta, ImdbError, ImdbRatings, ImdbStatus, RefreshOutcome};
pub use index::Rating;
