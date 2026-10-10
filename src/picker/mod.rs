//! The random picker: candidate pools, uniform sampling, the availability guarantee and
//! the user's filters.

pub mod eligibility;
pub mod engine;
pub mod filters;
pub mod genres;
pub mod sampling;

pub use eligibility::{Chip, ChipGroup, ChipGroupKind, Verdict, WhereToWatch, is_eligible, where_to_watch};
pub use engine::{
    Catalog, Pick, PickError, PickRequest, Picker, RECENT_PICKS_EXCLUDED, RatingSource, SkipList,
    passes_imdb_minimums, reverify,
};
pub use genres::UnifiedGenre;
