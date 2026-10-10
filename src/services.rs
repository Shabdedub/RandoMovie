//! Streaming services.
//!
//! TMDB lists one provider per product variant ("Netflix", "Netflix Standard with Ads",
//! "Netflix Kids", "Paramount+ Amazon Channel", ...). The app shows one tile per service by
//! grouping variants on a normalised name, and expands the user's selection back to every
//! variant id when it asks TMDB for titles.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::tmdb::model::ProviderEntry;

/// Word sequences that mark a resold or ad-supported variant of another service.
const CHANNEL_SUFFIXES: [&[&str]; 4] = [
    &["with", "ads"],
    &["amazon", "channel"],
    &["apple", "tv", "channel"],
    &["roku", "premium", "channel"],
];

/// Plan or profile words that don't make a separate service.
const TIER_WORDS: [&str; 4] = ["basic", "standard", "premium", "kids"];

/// Services whose real name ends in a tier word.
const BRANDS_WITH_TIER_WORDS: [&str; 1] = ["youtube premium"];

/// Sorts last when a provider has no priority for the region.
const NO_PRIORITY: i32 = i32::MAX / 2;

/// One TMDB provider in a region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub id: u32,
    pub name: String,
    pub logo_path: Option<String>,
    /// The region's display priority; lower is more prominent.
    pub priority: i32,
}

/// One service tile: a family of provider variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceGroup {
    pub key: String,
    pub name: String,
    pub logo_path: Option<String>,
    pub priority: i32,
    /// Variants in priority order.
    pub variants: Vec<Provider>,
}

impl ServiceGroup {
    pub fn variant_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.variants.iter().map(|v| v.id)
    }
}

/// The base service name with variant suffixes removed:
/// "Netflix basic with Ads" -> "Netflix", "Paramount+ Amazon Channel" -> "Paramount+".
pub fn base_name(name: &str) -> String {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    loop {
        if let Some(suffix) = CHANNEL_SUFFIXES
            .iter()
            .find(|suffix| ends_with_words(&words, suffix))
        {
            words.truncate(words.len() - suffix.len());
            continue;
        }
        let current = words.join(" ").to_lowercase();
        if BRANDS_WITH_TIER_WORDS.contains(&current.as_str()) {
            break;
        }
        let ends_in_tier = words
            .last()
            .is_some_and(|last| TIER_WORDS.iter().any(|t| last.eq_ignore_ascii_case(t)));
        if words.len() > 1 && ends_in_tier {
            words.pop();
            continue;
        }
        break;
    }
    words.join(" ")
}

/// The grouping key: the base name lowercased with "+" read as "plus" and only letters and
/// digits kept, so "Paramount+", "Paramount Plus" and "AcornTV"/"Acorn TV" meet.
pub fn group_key(name: &str) -> String {
    base_name(name)
        .to_lowercase()
        .replace('+', "plus")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn ends_with_words(words: &[&str], suffix: &[&str]) -> bool {
    words.len() > suffix.len()
        && words[words.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(w, s)| w.eq_ignore_ascii_case(s))
}

/// Merges TMDB's movie and TV provider lists by id and orders them by the region's display
/// priority.
pub fn merge_providers(lists: &[&[ProviderEntry]], region: &str) -> Vec<Provider> {
    let mut by_id: HashMap<u32, Provider> = HashMap::new();
    for list in lists {
        for entry in *list {
            let priority = entry
                .display_priorities
                .get(region)
                .copied()
                .unwrap_or(NO_PRIORITY);
            by_id
                .entry(entry.provider_id)
                .and_modify(|p| p.priority = p.priority.min(priority))
                .or_insert_with(|| Provider {
                    id: entry.provider_id,
                    name: entry.provider_name.trim().to_owned(),
                    logo_path: entry.logo_path.clone(),
                    priority,
                });
        }
    }
    let mut providers: Vec<Provider> = by_id.into_values().collect();
    providers.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(&b.name)));
    providers
}

/// Groups providers (already in priority order) into service tiles.
pub fn group_providers(providers: &[Provider]) -> Vec<ServiceGroup> {
    let mut groups: Vec<ServiceGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for provider in providers {
        let key = group_key(&provider.name);
        match index.get(&key) {
            Some(&i) => groups[i].variants.push(provider.clone()),
            None => {
                index.insert(key.clone(), groups.len());
                groups.push(ServiceGroup {
                    key,
                    name: String::new(),
                    logo_path: None,
                    priority: provider.priority,
                    variants: vec![provider.clone()],
                });
            }
        }
    }
    for group in &mut groups {
        name_group(group);
    }
    groups.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(&b.name)));
    groups
}

/// The plain service ("Netflix") names the tile. A lone variant keeps its full name
/// ("MGM Amazon Channel"); otherwise the stripped name of the most prominent variant is used.
fn name_group(group: &mut ServiceGroup) {
    group.priority = group
        .variants
        .iter()
        .map(|v| v.priority)
        .min()
        .unwrap_or(NO_PRIORITY);
    let plain = group
        .variants
        .iter()
        .find(|v| base_name(&v.name) == v.name.trim());
    let chosen = plain.or_else(|| {
        if group.variants.len() == 1 {
            group.variants.first()
        } else {
            None
        }
    });
    match chosen {
        Some(variant) => {
            group.name = variant.name.clone();
            group.logo_path = variant.logo_path.clone();
        }
        None => {
            let best = &group.variants[0];
            group.name = base_name(&best.name);
            group.logo_path = best.logo_path.clone();
        }
    }
}

/// All services in a region, with a lookup from provider id to its tile.
#[derive(Debug, Clone, Default)]
pub struct ServiceCatalog {
    pub region: String,
    pub groups: Vec<ServiceGroup>,
    by_provider: HashMap<u32, usize>,
    by_key: HashMap<String, usize>,
}

impl ServiceCatalog {
    pub fn new(region: &str, providers: &[Provider]) -> ServiceCatalog {
        let groups = group_providers(providers);
        let mut by_provider = HashMap::new();
        let mut by_key = HashMap::new();
        for (i, group) in groups.iter().enumerate() {
            by_key.insert(group.key.clone(), i);
            for id in group.variant_ids() {
                by_provider.insert(id, i);
            }
        }
        ServiceCatalog {
            region: region.to_owned(),
            groups,
            by_provider,
            by_key,
        }
    }

    /// Builds the catalog from TMDB's movie and TV provider lists for `region`.
    pub fn from_lists(region: &str, movie: &[ProviderEntry], tv: &[ProviderEntry]) -> ServiceCatalog {
        ServiceCatalog::new(region, &merge_providers(&[movie, tv], region))
    }

    pub fn group_of(&self, provider_id: u32) -> Option<&ServiceGroup> {
        self.by_provider.get(&provider_id).map(|&i| &self.groups[i])
    }

    pub fn group(&self, key: &str) -> Option<&ServiceGroup> {
        self.by_key.get(key).map(|&i| &self.groups[i])
    }

    /// Finds a tile by any spelling of its name ("Paramount+", "paramount plus").
    pub fn find(&self, name: &str) -> Option<&ServiceGroup> {
        self.group(&group_key(name))
    }
}

/// On/off state of a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionState {
    Off,
    Partial,
    On,
}

/// The user's services: selected tiles by key, minus variants switched off individually.
/// Keys are region-independent, so "Netflix" stays selected after a region change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceSelection {
    pub groups: BTreeSet<String>,
    pub disabled_variants: BTreeSet<u32>,
}

impl ServiceSelection {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub fn variant_selected(&self, group: &ServiceGroup, variant_id: u32) -> bool {
        self.groups.contains(&group.key) && !self.disabled_variants.contains(&variant_id)
    }

    pub fn state(&self, group: &ServiceGroup) -> SelectionState {
        let on = group
            .variant_ids()
            .filter(|&id| self.variant_selected(group, id))
            .count();
        match on {
            0 => SelectionState::Off,
            n if n == group.variants.len() => SelectionState::On,
            _ => SelectionState::Partial,
        }
    }

    /// Turns a whole tile on or off.
    pub fn set_group(&mut self, group: &ServiceGroup, on: bool) {
        for id in group.variant_ids() {
            self.disabled_variants.remove(&id);
        }
        if on {
            self.groups.insert(group.key.clone());
        } else {
            self.groups.remove(&group.key);
        }
    }

    /// Turns one variant on or off. Turning on a variant of an unselected tile selects only
    /// that variant; turning off the last variant deselects the tile.
    pub fn set_variant(&mut self, group: &ServiceGroup, variant_id: u32, on: bool) {
        if on {
            if !self.groups.contains(&group.key) {
                self.groups.insert(group.key.clone());
                self.disabled_variants
                    .extend(group.variant_ids().filter(|&id| id != variant_id));
            }
            self.disabled_variants.remove(&variant_id);
        } else {
            self.disabled_variants.insert(variant_id);
            if self.state(group) == SelectionState::Off {
                self.set_group(group, false);
            }
        }
    }

    /// S in the spec: provider ids of every selected service in this region, variants
    /// included, sorted.
    pub fn expand(&self, catalog: &ServiceCatalog) -> Vec<u32> {
        let mut ids: Vec<u32> = catalog
            .groups
            .iter()
            .filter(|g| self.groups.contains(&g.key))
            .flat_map(|g| g.variant_ids())
            .filter(|id| !self.disabled_variants.contains(id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn expand_set(&self, catalog: &ServiceCatalog) -> HashSet<u32> {
        self.expand(catalog).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmdb::model::ProviderList;

    fn au_catalog() -> ServiceCatalog {
        let movie: ProviderList =
            serde_json::from_str(include_str!("../tests/fixtures/providers_movie_au.json")).unwrap();
        let tv: ProviderList =
            serde_json::from_str(include_str!("../tests/fixtures/providers_tv_au.json")).unwrap();
        ServiceCatalog::from_lists("AU", &movie.results, &tv.results)
    }

    fn ids(group: &ServiceGroup) -> Vec<u32> {
        let mut ids: Vec<u32> = group.variant_ids().collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn strips_variant_suffixes() {
        assert_eq!(base_name("Netflix basic with Ads"), "Netflix");
        assert_eq!(base_name("Netflix Standard with Ads"), "Netflix");
        assert_eq!(base_name("Netflix Kids"), "Netflix");
        assert_eq!(base_name("Paramount+ Amazon Channel"), "Paramount+");
        assert_eq!(base_name("Paramount Plus Apple TV channel"), "Paramount Plus");
        assert_eq!(base_name("Paramount Plus Basic with Ads"), "Paramount Plus");
        assert_eq!(base_name("Paramount Plus Premium"), "Paramount Plus");
        assert_eq!(base_name("Apple TV Amazon Channel"), "Apple TV");
        assert_eq!(base_name("Apple TV Store"), "Apple TV Store");
        assert_eq!(base_name("YouTube Premium"), "YouTube Premium");
        assert_eq!(base_name("Premium"), "Premium");
        assert_eq!(group_key("Paramount+"), group_key("Paramount Plus"));
        assert_eq!(group_key("AcornTV Amazon Channel"), group_key("Acorn TV"));
        assert_ne!(group_key("Apple TV"), group_key("Apple TV Store"));
        assert_ne!(group_key("YouTube Premium"), group_key("YouTube"));
    }

    #[test]
    fn groups_real_au_providers() {
        let catalog = au_catalog();

        let netflix = catalog.group("netflix").unwrap();
        assert_eq!(netflix.name, "Netflix");
        assert_eq!(ids(netflix), [8, 175, 1796]);

        let paramount = catalog.find("Paramount+").unwrap();
        assert_eq!(paramount.name, "Paramount Plus");
        assert_eq!(ids(paramount), [531, 582, 1853, 2303, 2304]);

        let apple = catalog.group("appletv").unwrap();
        assert_eq!(ids(apple), [350, 2243]);
        assert_eq!(catalog.group_of(2).unwrap().name, "Apple TV Store");

        let acorn = catalog.group("acorntv").unwrap();
        assert_eq!(ids(acorn), [87, 196, 2034]);

        let mgm = catalog.group_of(588).unwrap();
        assert_eq!(mgm.name, "MGM Amazon Channel", "a lone variant keeps its full name");

        assert_ne!(
            catalog.group_of(188).unwrap().key,
            catalog.group_of(192).unwrap().key,
            "YouTube Premium is its own service"
        );

        // Most popular first, and every provider lands in exactly one tile.
        assert_eq!(catalog.groups[0].name, "Netflix");
        assert_eq!(catalog.groups[1].name, "Amazon Prime Video");
        let total: usize = catalog.groups.iter().map(|g| g.variants.len()).sum();
        assert_eq!(total, 98);
    }

    #[test]
    fn selection_expands_to_variants() {
        let catalog = au_catalog();
        let netflix = catalog.group("netflix").unwrap().clone();
        let stan = catalog.find("Stan").unwrap().clone();

        let mut sel = ServiceSelection::default();
        assert!(sel.expand(&catalog).is_empty());

        sel.set_group(&netflix, true);
        sel.set_group(&stan, true);
        assert_eq!(sel.expand(&catalog), [8, 21, 175, 1796]);
        assert_eq!(sel.state(&netflix), SelectionState::On);

        sel.set_variant(&netflix, 1796, false);
        assert_eq!(sel.state(&netflix), SelectionState::Partial);
        assert_eq!(sel.expand(&catalog), [8, 21, 175]);

        sel.set_variant(&netflix, 8, false);
        sel.set_variant(&netflix, 175, false);
        assert_eq!(sel.state(&netflix), SelectionState::Off);
        assert!(!sel.groups.contains("netflix"));
        assert!(sel.disabled_variants.is_empty(), "deselecting cleans up variants");

        // Turning on one variant of an unselected tile selects only that variant.
        sel.set_variant(&netflix, 1796, true);
        assert_eq!(sel.expand(&catalog), [21, 1796]);
        sel.set_group(&netflix, true);
        assert_eq!(sel.expand(&catalog), [8, 21, 175, 1796]);
    }
}
