//! The availability guarantee, and the "where to watch" grouping shown on the title card.

use std::collections::HashSet;

use crate::services::ServiceCatalog;
use crate::tmdb::model::{Offer, RegionAvailability};

/// The spec's rule for region R: `flatrate ∪ free ∪ ads` contains a selected provider, or
/// free titles are on and `free ∪ ads` isn't empty. Rent and buy never count.
pub fn is_eligible(av: &RegionAvailability, services: &HashSet<u32>, include_free: bool) -> bool {
    on_services(av, services).next().is_some() || (include_free && is_free(av))
}

/// Free or ad-supported somewhere in the region.
pub fn is_free(av: &RegionAvailability) -> bool {
    !av.free.is_empty() || !av.ads.is_empty()
}

fn on_services<'a>(
    av: &'a RegionAvailability,
    services: &'a HashSet<u32>,
) -> impl Iterator<Item = &'a Offer> + 'a {
    av.flatrate
        .iter()
        .chain(&av.free)
        .chain(&av.ads)
        .filter(move |offer| services.contains(&offer.provider_id))
}

/// The headline above the provider chips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// "Included with Netflix"
    Included(Vec<String>),
    /// "Free on SBS On Demand"
    Free(Vec<String>),
    /// Streams only on services the user doesn't have, or is rent/buy only.
    NotOnServices,
    /// No offers at all in the region.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipGroupKind {
    YourServices,
    Free,
    OtherSubscriptions,
    RentBuy,
}

impl ChipGroupKind {
    pub fn label(self) -> &'static str {
        match self {
            ChipGroupKind::YourServices => "On your services",
            ChipGroupKind::Free => "Free",
            ChipGroupKind::OtherSubscriptions => "Other subscriptions",
            ChipGroupKind::RentBuy => "Rent or buy",
        }
    }
}

/// One logo chip: a service tile name, so variants such as "Netflix Standard with Ads"
/// show as "Netflix".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub provider_id: u32,
    pub name: String,
    pub logo_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipGroup {
    pub kind: ChipGroupKind,
    pub chips: Vec<Chip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhereToWatch {
    pub verdict: Verdict,
    /// Non-empty groups in display order.
    pub groups: Vec<ChipGroup>,
    /// TMDB's watch page for this title and region.
    pub link: Option<String>,
}

impl WhereToWatch {
    pub fn group(&self, kind: ChipGroupKind) -> Option<&ChipGroup> {
        self.groups.iter().find(|g| g.kind == kind)
    }
}

/// Sorts the region's offers into chip groups. Each service appears once, in the first
/// group that applies: your services, then free, other subscriptions, rent/buy.
pub fn where_to_watch(
    av: &RegionAvailability,
    services: &HashSet<u32>,
    catalog: &ServiceCatalog,
) -> WhereToWatch {
    let mut seen: HashSet<String> = HashSet::new();
    let mut build = |kind: ChipGroupKind, offers: Vec<&Offer>| {
        let mut offers = offers;
        offers.sort_by_key(|o| o.display_priority);
        let mut chips = Vec::new();
        for offer in offers {
            let (key, name, logo) = match catalog.group_of(offer.provider_id) {
                Some(group) => (group.key.clone(), group.name.clone(), group.logo_path.clone()),
                None => (
                    format!("id:{}", offer.provider_id),
                    offer.provider_name.trim().to_owned(),
                    offer.logo_path.clone(),
                ),
            };
            if seen.insert(key) {
                chips.push(Chip {
                    provider_id: offer.provider_id,
                    name,
                    logo_path: logo,
                });
            }
        }
        ChipGroup { kind, chips }
    };

    let yours = build(ChipGroupKind::YourServices, on_services(av, services).collect());
    let free = build(ChipGroupKind::Free, av.free.iter().chain(&av.ads).collect());
    let other = build(ChipGroupKind::OtherSubscriptions, av.flatrate.iter().collect());
    let rent_buy = build(ChipGroupKind::RentBuy, av.rent.iter().chain(&av.buy).collect());

    let names = |group: &ChipGroup| group.chips.iter().map(|c| c.name.clone()).collect();
    let verdict = if !yours.chips.is_empty() {
        Verdict::Included(names(&yours))
    } else if !free.chips.is_empty() {
        Verdict::Free(names(&free))
    } else if av.has_offers() {
        Verdict::NotOnServices
    } else {
        Verdict::Unavailable
    };

    WhereToWatch {
        verdict,
        groups: [yours, free, other, rent_buy]
            .into_iter()
            .filter(|g| !g.chips.is_empty())
            .collect(),
        link: av.link.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaType;
    use crate::tmdb::model::{ProviderList, parse_details};

    fn catalog() -> ServiceCatalog {
        let movie: ProviderList =
            serde_json::from_str(include_str!("../../tests/fixtures/providers_movie_au.json")).unwrap();
        let tv: ProviderList =
            serde_json::from_str(include_str!("../../tests/fixtures/providers_tv_au.json")).unwrap();
        ServiceCatalog::from_lists("AU", &movie.results, &tv.results)
    }

    fn matrix() -> RegionAvailability {
        let body = include_str!("../../tests/fixtures/movie_603_details.json");
        parse_details(MediaType::Movie, body.as_bytes(), "AU", 0)
            .unwrap()
            .availability
    }

    fn bluey() -> RegionAvailability {
        let body = include_str!("../../tests/fixtures/tv_82728_details.json");
        parse_details(MediaType::Tv, body.as_bytes(), "AU", 0)
            .unwrap()
            .availability
    }

    fn set(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }

    fn offer(id: u32) -> Offer {
        Offer {
            provider_id: id,
            provider_name: format!("P{id}"),
            logo_path: None,
            display_priority: 1,
        }
    }

    #[test]
    fn subscription_on_a_selected_service_counts() {
        // The Matrix in AU streams on Stan (21) and Prime Video (119), with no free offer.
        assert!(is_eligible(&matrix(), &set(&[21]), false));
        assert!(is_eligible(&matrix(), &set(&[2100]), false), "a variant id counts");
        assert!(!is_eligible(&matrix(), &set(&[8]), false));
        assert!(!is_eligible(&matrix(), &set(&[8]), true), "not free anywhere");
        assert!(!is_eligible(&matrix(), &set(&[]), true));
    }

    #[test]
    fn free_titles_count_only_when_enabled() {
        // Bluey in AU is free on ABC iview (135) and otherwise buy-only.
        assert!(is_eligible(&bluey(), &set(&[]), true));
        assert!(!is_eligible(&bluey(), &set(&[]), false));
        assert!(is_eligible(&bluey(), &set(&[135]), false), "a selected free service counts");
        assert!(!is_eligible(&bluey(), &set(&[2, 10]), false), "buying never counts");
    }

    #[test]
    fn rent_and_buy_never_count() {
        let av = RegionAvailability {
            rent: vec![offer(2), offer(3)],
            buy: vec![offer(2)],
            ..Default::default()
        };
        assert!(!is_eligible(&av, &set(&[2, 3]), true));
        let ads_only = RegionAvailability {
            ads: vec![offer(132)],
            ..Default::default()
        };
        assert!(is_eligible(&ads_only, &set(&[]), true), "ad-supported is free");
    }

    #[test]
    fn groups_chips_and_picks_the_verdict() {
        let catalog = catalog();
        let stan_and_prime = where_to_watch(&matrix(), &set(&[21, 119, 2100]), &catalog);
        assert_eq!(
            stan_and_prime.verdict,
            Verdict::Included(vec!["Amazon Prime Video".into(), "Stan".into()])
        );
        let yours = stan_and_prime.group(ChipGroupKind::YourServices).unwrap();
        assert_eq!(yours.chips.len(), 2, "Prime Video and its ads tier are one chip");
        let other: Vec<&str> = stan_and_prime
            .group(ChipGroupKind::OtherSubscriptions)
            .unwrap()
            .chips
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(other, ["HBO Max"], "HBO Max and its Amazon Channel are one chip");
        assert!(stan_and_prime.group(ChipGroupKind::RentBuy).is_some());
        assert!(stan_and_prime.link.is_some());

        let none = where_to_watch(&matrix(), &set(&[8]), &catalog);
        assert_eq!(none.verdict, Verdict::NotOnServices);
        assert!(none.group(ChipGroupKind::YourServices).is_none());

        let free = where_to_watch(&bluey(), &set(&[]), &catalog);
        assert_eq!(free.verdict, Verdict::Free(vec!["ABC iview".into()]));
        let kinds: Vec<ChipGroupKind> = free.groups.iter().map(|g| g.kind).collect();
        assert_eq!(kinds, [ChipGroupKind::Free, ChipGroupKind::RentBuy]);

        let nothing = where_to_watch(&RegionAvailability::default(), &set(&[8]), &catalog);
        assert_eq!(nothing.verdict, Verdict::Unavailable);
        assert!(nothing.groups.is_empty());
    }
}
