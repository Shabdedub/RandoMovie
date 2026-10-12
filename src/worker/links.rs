//! Service buttons on the title card: the title's ids on streaming services, looked up on
//! Wikidata when a card shows and cached for 30 days, and the link a button opens.

use std::rc::Rc;
use std::time::Duration;

use randomovie::platform::shell;
use randomovie::util::time::{DAY, now_unix};
use randomovie::watch_links::{self, ServiceIds};
use randomovie::{log_info, log_warn};

use super::{ServiceLinkRequest, Worker};

/// Ids on services rarely change; a title looked up is looked up again after this.
const IDS_TTL: Duration = Duration::from_secs(30 * DAY as u64);

impl Worker {
    /// A card is on screen: its title's ids, so a button press opens the exact page at once.
    pub(super) async fn prepare_service_links(self: &Rc<Self>, imdb_id: String) {
        let _ = self.service_ids(&imdb_id).await;
    }

    /// A service button: works out the link and opens it in the browser.
    pub(super) async fn open_service_link(self: &Rc<Self>, request: ServiceLinkRequest) {
        let ids = match &request.imdb_id {
            Some(imdb_id) if watch_links::uses_service_ids(request.provider_id) => self.service_ids(imdb_id).await,
            _ => None,
        };
        let region = self.effective_region();
        match watch_links::button_url(
            request.provider_id,
            request.title.media,
            ids.as_ref(),
            &request.name,
            &region,
            request.watch_page.as_deref(),
        ) {
            Some(url) => {
                let how = if url.starts_with("https://www.themoviedb.org/") {
                    "TMDB's watch page"
                } else if url.contains("search") || url.contains("/results?") {
                    "a search"
                } else {
                    "the title's page"
                };
                log_info!("service button for provider {} opens {how}", request.provider_id);
                if !shell::open_url(&url) {
                    log_warn!("refused to open a non-web link");
                }
            }
            None => log_warn!("no link for provider {} and no watch page", request.provider_id),
        }
    }

    /// The ids for one IMDb id, from memory, the database or Wikidata. A press during the
    /// lookup started when the card showed waits for that lookup instead of starting another.
    /// `None` when Wikidata couldn't be reached; the next press tries again.
    async fn service_ids(self: &Rc<Self>, imdb_id: &str) -> Option<ServiceIds> {
        if !watch_links::is_imdb_id(imdb_id) {
            return None;
        }
        let cell = self.service_id_lookups.borrow_mut().entry(imdb_id.to_owned()).or_default().clone();
        cell.get_or_try_init(|| self.fetch_service_ids(imdb_id)).await.ok().cloned()
    }

    async fn fetch_service_ids(&self, imdb_id: &str) -> Result<ServiceIds, String> {
        let key = format!("wikidata:{imdb_id}");
        let now = now_unix();
        if let Ok(Some(cached)) = self.store.http_get(&key)
            && cached.is_fresh(now, IDS_TTL)
            && let Ok(ids) = serde_json::from_slice(&cached.body)
        {
            return Ok(ids);
        }
        let http = self.http.clone().ok_or("no HTTP client")?;
        match watch_links::fetch_service_ids(&http, imdb_id).await {
            Ok(ids) => {
                if let Ok(body) = serde_json::to_vec(&ids)
                    && let Err(e) = self.store.http_put(&key, &body, now)
                {
                    log_warn!("service ids couldn't be cached: {e}");
                }
                Ok(ids)
            }
            Err(e) => {
                log_info!("Wikidata lookup for {imdb_id} failed: {e}");
                Err(e)
            }
        }
    }
}
