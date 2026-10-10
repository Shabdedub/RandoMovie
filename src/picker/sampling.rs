//! Uniform sampling over discover results.
//!
//! TMDB returns 20 results per page and refuses pages past 500, so at most the first 10 000
//! results of a query are reachable. A draw picks a pool with probability proportional to
//! its reachable size, then a uniform index inside it.

pub const PAGE_SIZE: u32 = 20;
pub const MAX_PAGE: u32 = 500;
pub const MAX_REACHABLE: u32 = PAGE_SIZE * MAX_PAGE;

/// How many results of a query can actually be fetched.
pub fn reachable(total_results: u32) -> u32 {
    total_results.min(MAX_REACHABLE)
}

/// 0-based result index to (1-based page, offset within the page).
pub fn page_and_offset(index: u32) -> (u32, usize) {
    (index / PAGE_SIZE + 1, (index % PAGE_SIZE) as usize)
}

/// Draws a pool, weighted by size, and a uniform index below that pool's size.
/// Returns `None` when every pool is empty.
pub fn draw(sizes: &[u32], rng: &mut fastrand::Rng) -> Option<(usize, u32)> {
    let total: u64 = sizes.iter().map(|&s| u64::from(s)).sum();
    if total == 0 {
        return None;
    }
    let mut x = rng.u64(0..total);
    for (pool, &size) in sizes.iter().enumerate() {
        let size = u64::from(size);
        if x < size {
            return Some((pool, x as u32));
        }
        x -= size;
    }
    None
}

/// The order to try a fetched page: the drawn offset first (clamped to a short page), then
/// the rest shuffled.
pub fn try_order(drawn: usize, page_len: usize, rng: &mut fastrand::Rng) -> Vec<usize> {
    if page_len == 0 {
        return Vec::new();
    }
    let first = drawn.min(page_len - 1);
    let mut rest: Vec<usize> = (0..page_len).filter(|&i| i != first).collect();
    rng.shuffle(&mut rest);
    let mut order = Vec::with_capacity(page_len);
    order.push(first);
    order.extend(rest);
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_maps_to_page_and_offset() {
        assert_eq!(page_and_offset(0), (1, 0));
        assert_eq!(page_and_offset(19), (1, 19));
        assert_eq!(page_and_offset(20), (2, 0));
        assert_eq!(page_and_offset(9_999), (500, 19));
    }

    #[test]
    fn caps_at_500_pages() {
        assert_eq!(reachable(45), 45);
        assert_eq!(reachable(10_000), 10_000);
        assert_eq!(reachable(20_001), 10_000);
        let mut rng = fastrand::Rng::with_seed(1);
        for _ in 0..10_000 {
            let (_, index) = draw(&[reachable(20_001)], &mut rng).unwrap();
            let (page, offset) = page_and_offset(index);
            assert!((1..=MAX_PAGE).contains(&page));
            assert!(offset < PAGE_SIZE as usize);
        }
    }

    #[test]
    fn short_last_page() {
        // 45 results: pages of 20, 20 and 5.
        let mut rng = fastrand::Rng::with_seed(2);
        let mut seen_last_page = false;
        for _ in 0..2_000 {
            let (_, index) = draw(&[45], &mut rng).unwrap();
            let (page, offset) = page_and_offset(index);
            assert!(page <= 3);
            if page == 3 {
                seen_last_page = true;
                assert!(offset < 5, "offset {offset} past the 5 items on page 3");
            }
        }
        assert!(seen_last_page);
        // If the page came back shorter than expected, the drawn item is clamped.
        let order = try_order(17, 5, &mut rng);
        assert_eq!(order[0], 4);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4]);
        assert!(try_order(3, 0, &mut rng).is_empty());
    }

    #[test]
    fn pools_are_weighted_by_size() {
        let mut rng = fastrand::Rng::with_seed(3);
        let sizes = [100, 300, 0, 600];
        let mut counts = [0u32; 4];
        for _ in 0..100_000 {
            let (pool, index) = draw(&sizes, &mut rng).unwrap();
            assert!(index < sizes[pool]);
            counts[pool] += 1;
        }
        assert_eq!(counts[2], 0);
        let share = |n: u32| f64::from(n) / 100_000.0;
        assert!((share(counts[0]) - 0.1).abs() < 0.01);
        assert!((share(counts[1]) - 0.3).abs() < 0.01);
        assert!((share(counts[3]) - 0.6).abs() < 0.01);
        assert_eq!(draw(&[0, 0], &mut rng), None);
        assert_eq!(draw(&[], &mut rng), None);
    }

    #[test]
    fn every_index_is_reachable_uniformly() {
        let mut rng = fastrand::Rng::with_seed(4);
        let mut counts = [0u32; 45];
        for _ in 0..90_000 {
            let (_, index) = draw(&[45], &mut rng).unwrap();
            counts[index as usize] += 1;
        }
        // Expect 2000 each; allow generous noise.
        assert!(counts.iter().all(|&c| (1700..2300).contains(&c)), "{counts:?}");
    }
}
