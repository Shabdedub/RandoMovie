//! The Search screen on the UI thread: the query, sent 300 ms after typing stops; result pages
//! with "Load more"; each row's rating and availability; thumbnails; and the card for an
//! opened result (a `CardSlot`, as on Pick).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use randomovie::card::TitleCard;
use randomovie::error::ApiError;
use randomovie::media::TitleRef;
use randomovie::search::{HitInfo, RowAvailability, SearchHit, normalize_query};
use randomovie::log_info;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, VecModel};

use super::Controller;
use super::card::{CardSlot, Surface, imdb_state};
use super::pick::PLACEHOLDER_DELAY;
use crate::worker::{Command, ImageSpec};
use crate::{AppWindow, SearchMetrics, SearchRow, SearchState, SearchStatus};

/// The spec's debounce: a search goes out once typing pauses this long.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

pub(super) struct SearchView {
    /// The opened result.
    pub(super) card: CardSlot,
    /// The text in the box, as typed.
    text: String,
    /// The query the results are for, its request id and how far it has loaded.
    query: Option<String>,
    id: u64,
    page: u32,
    total_pages: u32,
    loading_more: bool,
    hits: Vec<SearchHit>,
    rows: Rc<VecModel<SearchRow>>,
    /// Decoded thumbnails by size and path; dropped whenever Search hides.
    thumbs: HashMap<String, Image>,
    pending_thumbs: HashSet<String>,
    /// The result being opened, until its card arrives.
    opening: Option<u64>,
    /// When the query was sent, for the log's timings.
    sent_at: Option<Instant>,
    info_logged: bool,
    /// Services, region or ratings changed since the rows and the card were filled.
    stale: bool,
    debounce: Timer,
    placeholder_timer: Timer,
}

impl SearchView {
    pub(super) fn new() -> SearchView {
        SearchView {
            card: CardSlot::new(),
            text: String::new(),
            query: None,
            id: 0,
            page: 0,
            total_pages: 0,
            loading_more: false,
            hits: Vec::new(),
            rows: Rc::new(VecModel::default()),
            thumbs: HashMap::new(),
            pending_thumbs: HashSet::new(),
            opening: None,
            sent_at: None,
            info_logged: false,
            stale: false,
            debounce: Timer::default(),
            placeholder_timer: Timer::default(),
        }
    }
}

fn thumb_key(spec: &ImageSpec) -> String {
    format!("{}x{}:{}", spec.width, spec.height, spec.path)
}

fn new_row(hit: &SearchHit, thumb: Image) -> SearchRow {
    SearchRow {
        name: hit.name.as_str().into(),
        meta: hit.meta().into(),
        thumb,
        imdb: SharedString::new(),
        imdb_state: 2,
        verdict: SharedString::new(),
        verdict_state: 4,
    }
}

fn apply_info(row: &mut SearchRow, info: Option<&HitInfo>) {
    match info {
        Some(info) => {
            row.imdb = info.imdb.trim_start_matches('★').trim_start().into();
            row.imdb_state = imdb_state(info.imdb_badge);
            row.verdict = info.verdict.as_str().into();
            row.verdict_state = match info.availability {
                RowAvailability::OnYourServices => 0,
                RowAvailability::Free => 1,
                RowAvailability::NotOnServices => 2,
                RowAvailability::NotStreaming => 3,
            };
        }
        None => {
            row.imdb = SharedString::new();
            row.imdb_state = 2;
            row.verdict = "Couldn't load where it streams".into();
            row.verdict_state = 5;
        }
    }
}

fn error_message(error: &ApiError) -> String {
    match error {
        ApiError::Unauthorized => {
            "TMDB didn't accept the token. Add a new one in Settings, then try again.".to_owned()
        }
        e if e.is_connectivity() => format!("{e}. Check your connection, then try again."),
        e => format!("{e}."),
    }
}

impl Controller {
    pub(super) fn init_search(&mut self, ui: &AppWindow) {
        let state = ui.global::<SearchState>();
        state.set_rows(ModelRc::from(self.search.rows.clone()));
        state.set_groups(self.search.card.groups_model());
        state.set_status(SearchStatus::Idle);
        self.refresh_search_texts(ui);
    }

    /// The intro and heading name the region, so they follow a region change.
    pub(super) fn refresh_search_texts(&self, ui: &AppWindow) {
        let region = self.region_name(&self.effective_region());
        let state = ui.global::<SearchState>();
        state.set_intro(
            format!("See its IMDb rating and where it streams in {region}, with your services marked.").into(),
        );
        if let Some(query) = &self.search.query
            && !self.search.hits.is_empty()
        {
            state.set_heading(format!("Results for “{query}” in {region}").into());
        }
    }

    // ---- Queries -----------------------------------------------------------------------

    /// Typing: the search goes out once typing pauses.
    pub fn search_edited(&mut self, _ui: &AppWindow, text: &str) {
        self.search.text = text.to_owned();
        Self::later(&self.search.debounce, &self.weak, SEARCH_DEBOUNCE, |c, ui| {
            c.run_search(ui, false);
        });
    }

    /// Enter in the box: search now, or open the selected result when the list is current.
    pub fn search_submitted(&mut self, ui: &AppWindow) {
        let query = normalize_query(&self.search.text);
        if query.is_some() && query == self.search.query && !self.search.hits.is_empty() {
            let selected = ui.global::<SearchState>().get_selected().max(0);
            self.open_result(ui, selected);
        } else {
            self.search.debounce.stop();
            self.run_search(ui, false);
        }
    }

    pub fn search_retry(&mut self, ui: &AppWindow) {
        self.run_search(ui, true);
    }

    /// Back online: a search that failed runs again by itself.
    pub(super) fn retry_failed_search(&mut self, ui: &AppWindow) {
        if ui.global::<SearchState>().get_status() == SearchStatus::Failed && self.search.query.is_some() {
            self.run_search(ui, true);
        }
    }

    fn run_search(&mut self, ui: &AppWindow, force: bool) {
        let query = normalize_query(&self.search.text);
        if !force && query == self.search.query {
            return;
        }
        let state = ui.global::<SearchState>();
        if state.get_card_open() || self.search.opening.is_some() {
            self.close_card(ui);
        }
        // A new id makes the worker drop the old search and the UI ignore its answers.
        let id = self.next_request_id();
        self.search.id = id;
        self.search.page = 0;
        self.search.total_pages = 0;
        self.search.loading_more = false;
        self.search.stale = false;
        state.set_has_more(false);
        state.set_loading_more(false);
        match query {
            None => {
                self.search.query = None;
                self.search.hits.clear();
                self.search.rows.set_vec(Vec::new());
                self.release_thumbs();
                state.set_selected(-1);
                state.set_searching(false);
                state.set_heading(SharedString::new());
                state.set_status(SearchStatus::Idle);
            }
            Some(query) => {
                self.search.query = Some(query.clone());
                self.search.sent_at = Some(Instant::now());
                self.search.info_logged = false;
                // Earlier results stay until the new ones arrive, so the list doesn't blink.
                if self.search.hits.is_empty() {
                    state.set_status(SearchStatus::Searching);
                } else {
                    state.set_searching(true);
                }
                self.send(Command::Search { id, query, page: 1 });
            }
        }
    }

    pub fn search_load_more(&mut self, ui: &AppWindow) {
        if self.search.loading_more || self.search.page >= self.search.total_pages {
            return;
        }
        let Some(query) = self.search.query.clone() else { return };
        self.search.loading_more = true;
        ui.global::<SearchState>().set_loading_more(true);
        self.send(Command::Search {
            id: self.search.id,
            query,
            page: self.search.page + 1,
        });
    }

    pub(super) fn on_search_results(
        &mut self,
        ui: &AppWindow,
        id: u64,
        page: u32,
        total_pages: u32,
        hits: Vec<SearchHit>,
    ) {
        if id != self.search.id {
            return; // an older query
        }
        if page == 1
            && let Some(sent) = self.search.sent_at
        {
            log_info!("search results on screen {} ms after the search was sent", sent.elapsed().as_millis());
        }
        self.search.page = page;
        self.search.total_pages = total_pages;
        self.search.loading_more = false;
        let rows: Vec<SearchRow> = hits.iter().map(|hit| new_row(hit, Image::default())).collect();
        let state = ui.global::<SearchState>();
        if page == 1 {
            self.search.hits = hits;
            self.search.rows.set_vec(rows);
            state.set_selected(if self.search.hits.is_empty() { -1 } else { 0 });
            state.set_results_serial(state.get_results_serial().wrapping_add(1));
        } else {
            self.search.hits.extend(hits);
            for row in rows {
                self.search.rows.push(row);
            }
        }
        let query = self.search.query.clone().unwrap_or_default();
        let count = self.search.hits.len();
        state.set_searching(false);
        state.set_loading_more(false);
        state.set_has_more(page < total_pages);
        if count == 0 {
            state.set_message(
                format!("No movies or series match “{query}”. Check the spelling, or try fewer words.")
                    .into(),
            );
            state.set_heading(SharedString::new());
            state.set_status(SearchStatus::Empty);
        } else {
            state.set_status(SearchStatus::Results);
        }
        self.refresh_search_texts(ui);
        let noun = if count == 1 { "result" } else { "results" };
        state.set_announcement(format!("{count} {noun} for {query}").into());
        self.request_thumbs(ui);
    }

    pub(super) fn on_search_failed(&mut self, ui: &AppWindow, id: u64, page: u32, error: ApiError) {
        if id != self.search.id {
            return;
        }
        let state = ui.global::<SearchState>();
        let message = error_message(&error);
        state.set_searching(false);
        state.set_loading_more(false);
        self.search.loading_more = false;
        if page > 1 {
            // The results so far stay; "Load more" can be tried again.
            state.set_heading(format!("More results didn't load. {message}").into());
            state.set_announcement("More results didn't load".into());
            return;
        }
        self.search.hits.clear();
        self.search.rows.set_vec(Vec::new());
        state.set_selected(-1);
        state.set_has_more(false);
        state.set_heading(SharedString::new());
        state.set_message(message.as_str().into());
        state.set_status(SearchStatus::Failed);
        state.set_announcement(format!("Search didn't work. {message}").into());
    }

    pub(super) fn on_search_hit_info(&mut self, _ui: &AppWindow, id: u64, title: TitleRef, info: Option<HitInfo>) {
        if id != self.search.id {
            return;
        }
        if !self.search.info_logged
            && let Some(sent) = self.search.sent_at
        {
            self.search.info_logged = true;
            log_info!("first rating and availability {} ms after the search was sent", sent.elapsed().as_millis());
        }
        for (index, hit) in self.search.hits.iter().enumerate() {
            if hit.title == title
                && let Some(mut row) = self.search.rows.row_data(index)
            {
                apply_info(&mut row, info.as_ref());
                self.search.rows.set_row_data(index, row);
            }
        }
    }

    /// Rows and the opened card again, after services, the region or the ratings changed.
    pub(super) fn search_availability_changed(&mut self, ui: &AppWindow) {
        self.search.stale = true;
        if Self::surface_visible(ui, Surface::Search) {
            self.refresh_stale_search(ui);
        }
    }

    pub(super) fn refresh_stale_search(&mut self, ui: &AppWindow) {
        if !self.search.stale {
            return;
        }
        self.search.stale = false;
        self.refresh_search_texts(ui);
        if !self.search.hits.is_empty() {
            let titles = self.search.hits.iter().map(|hit| hit.title).collect();
            self.send(Command::SearchInfo {
                id: self.search.id,
                titles,
            });
        }
        let state = ui.global::<SearchState>();
        if state.get_card_open()
            && state.get_card_error().is_empty()
            && let Some(title) = self.search.card.card().map(|card| card.title)
        {
            self.request_title(ui, title, false);
        }
    }

    // ---- Thumbnails --------------------------------------------------------------------

    fn thumb_spec(ui: &AppWindow, path: &str) -> ImageSpec {
        let metrics = ui.global::<SearchMetrics>();
        let scale = ui.window().scale_factor();
        let px = |logical: f32| (logical * scale).round().max(1.0) as u32;
        ImageSpec {
            path: path.to_owned(),
            width: px(metrics.get_thumb_width()),
            height: px(metrics.get_thumb_height()),
            radius: metrics.get_thumb_radius() * scale,
        }
    }

    /// Asks for every thumbnail on screen that isn't decoded or on its way.
    pub(super) fn request_thumbs(&mut self, ui: &AppWindow) {
        if !Self::surface_visible(ui, Surface::Search) {
            return;
        }
        let mut specs = Vec::new();
        for hit in &self.search.hits {
            let Some(path) = hit.poster_path.as_deref() else { continue };
            let spec = Self::thumb_spec(ui, path);
            let key = thumb_key(&spec);
            if !self.search.thumbs.contains_key(&key) && self.search.pending_thumbs.insert(key) {
                specs.push(spec);
            }
        }
        // Rows made before their thumbnail was decoded pick it up here.
        for (index, hit) in self.search.hits.iter().enumerate() {
            let Some(path) = hit.poster_path.as_deref() else { continue };
            let key = thumb_key(&Self::thumb_spec(ui, path));
            if let Some(image) = self.search.thumbs.get(&key)
                && let Some(mut row) = self.search.rows.row_data(index)
                && row.thumb.size().width == 0
            {
                row.thumb = image.clone();
                self.search.rows.set_row_data(index, row);
            }
        }
        if !specs.is_empty() {
            self.send(Command::LoadThumbs(specs));
        }
    }

    pub(super) fn on_thumb(&mut self, ui: &AppWindow, spec: ImageSpec, pixels: SharedPixelBuffer<Rgba8Pixel>) {
        let key = thumb_key(&spec);
        self.search.pending_thumbs.remove(&key);
        if !Self::surface_visible(ui, Surface::Search) || Self::thumb_spec(ui, &spec.path) != spec {
            return; // Search hid meanwhile, or the scale changed
        }
        let image = Image::from_rgba8(pixels);
        for (index, hit) in self.search.hits.iter().enumerate() {
            if hit.poster_path.as_deref() == Some(spec.path.as_str())
                && let Some(mut row) = self.search.rows.row_data(index)
            {
                row.thumb = image.clone();
                self.search.rows.set_row_data(index, row);
            }
        }
        self.search.thumbs.insert(key, image);
    }

    pub(super) fn on_thumb_failed(&mut self, spec: ImageSpec) {
        self.search.pending_thumbs.remove(&thumb_key(&spec));
    }

    /// Drops every decoded thumbnail (Search hides, or the scale changed).
    pub(super) fn release_thumbs(&mut self) {
        self.search.thumbs.clear();
        self.search.pending_thumbs.clear();
        for index in 0..self.search.rows.row_count() {
            if let Some(mut row) = self.search.rows.row_data(index)
                && row.thumb.size().width > 0
            {
                row.thumb = Image::default();
                self.search.rows.set_row_data(index, row);
            }
        }
    }

    // ---- The opened result -------------------------------------------------------------

    pub fn open_result(&mut self, ui: &AppWindow, index: i32) {
        let Some(title) = usize::try_from(index)
            .ok()
            .and_then(|i| self.search.hits.get(i))
            .map(|hit| hit.title)
        else {
            return;
        };
        ui.global::<SearchState>().set_selected(index);
        self.request_title(ui, title, true);
    }

    /// Asks for a title's card. With `placeholder`, a slow answer shows placeholders.
    fn request_title(&mut self, _ui: &AppWindow, title: TitleRef, placeholder: bool) {
        let id = self.next_request_id();
        self.search.opening = Some(id);
        if placeholder {
            Self::later(&self.search.placeholder_timer, &self.weak, PLACEHOLDER_DELAY, move |c, ui| {
                c.show_search_placeholder(ui, id);
            });
        }
        self.send(Command::OpenTitle { id, title });
    }

    fn show_search_placeholder(&mut self, ui: &AppWindow, id: u64) {
        if self.search.opening == Some(id) {
            self.release_card_images(ui, Surface::Search);
            let state = ui.global::<SearchState>();
            state.set_card_error(SharedString::new());
            state.set_card_loading(true);
            state.set_card_open(true);
        }
    }

    pub(super) fn on_title_card(&mut self, ui: &AppWindow, id: u64, card: TitleCard) {
        if self.search.opening != Some(id) {
            return;
        }
        self.search.opening = None;
        self.search.placeholder_timer.stop();
        self.present_card(ui, Surface::Search, id, card);
    }

    /// The opened card is on screen.
    pub(super) fn search_card_revealed(&mut self, ui: &AppWindow, _with_poster: bool) {
        let state = ui.global::<SearchState>();
        state.set_card_error(SharedString::new());
        state.set_card_loading(false);
        state.set_card_open(true);
        if let Some(card) = self.search.card.card() {
            let year = if card.year.is_empty() { String::new() } else { format!(" ({})", card.year) };
            state.set_announcement(format!("{}{year}. {}. {}.", card.name, card.imdb.trim_start_matches('★').trim_start(), card.verdict).into());
        }
    }

    pub(super) fn on_title_card_failed(&mut self, ui: &AppWindow, id: u64, error: ApiError) {
        if self.search.opening != Some(id) {
            return;
        }
        self.search.opening = None;
        self.search.placeholder_timer.stop();
        let message = error_message(&error);
        let state = ui.global::<SearchState>();
        state.set_card_loading(false);
        state.set_card_error(message.as_str().into());
        state.set_card_open(true);
        state.set_announcement(format!("This title didn't load. {message}").into());
    }

    /// "Back to results" and Esc.
    pub fn close_card(&mut self, ui: &AppWindow) {
        self.search.opening = None;
        self.search.placeholder_timer.stop();
        // Freed first: a card still waiting for its poster would otherwise open after this.
        self.release_card_images(ui, Surface::Search);
        let state = ui.global::<SearchState>();
        state.set_card_open(false);
        state.set_card_loading(false);
        state.set_card_error(SharedString::new());
    }

    pub fn search_open_link(&mut self, _ui: &AppWindow) {
        self.open_card_link(Surface::Search);
    }

    pub fn search_card_resized(&mut self, ui: &AppWindow, width: f32, height: f32, poster_width: f32, poster_height: f32) {
        self.card_resized(ui, Surface::Search, width, height, poster_width, poster_height);
    }
}
