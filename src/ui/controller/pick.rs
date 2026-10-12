//! The Pick screen on the UI thread: pick and "Previous" requests, the filter bar, and
//! scheduling the background pick that keeps "Pick again" instant. The card itself is a
//! `CardSlot` (see `card.rs`).

use std::rc::Rc;
use std::time::{Duration, Instant};

use randomovie::card::{ImdbBadge, TitleCard};
use randomovie::error::ApiError;
use randomovie::format;
use randomovie::media::{MediaChoice, TitleRef};
use randomovie::picker::PickError;
use randomovie::picker::filters::{self, DECADES, RATINGS, RUNTIMES, VOTES};
use randomovie::services::SelectionState;
use randomovie::store::{ExclusionReason, Filters};
use randomovie::log_info;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, VecModel};

use super::Controller;
use super::card::{CardSlot, Surface, card_data, write_card};
use crate::worker::Command;
use crate::{AppWindow, Exclusion, GenreOption, PickState, PickStatus};

/// An answer within this time replaces the card directly (a prepared pick is instant); a
/// slower one shows placeholders first.
pub(super) const PLACEHOLDER_DELAY: Duration = Duration::from_millis(150);
/// Changes to filters or services are collected this long before the next pick is prepared.
const PREFETCH_SETTLE: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Pick,
    Previous,
}

/// What the button under a failed pick does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorAction {
    ChooseServices,
    ResetFilters,
    AddToken,
    Retry,
}

pub(super) struct PickView {
    /// The card on screen (kept while an error shows, so "Previous" can return to it).
    pub(super) card: CardSlot,
    /// "Seen it" / "Not interested" for that card.
    excluded: Option<ExclusionReason>,
    /// The request waiting for an answer.
    pending: Option<(u64, Request)>,
    /// When the last request was made, for the log's click-to-card time.
    requested_at: Option<Instant>,
    /// The worker has an earlier pick before the card on screen.
    earlier: bool,
    /// Movies / Series / Both: starts at the default type, not saved.
    media: MediaChoice,
    /// Unified genre names from TMDB.
    genres: Vec<String>,
    genre_options: Rc<VecModel<GenreOption>>,
    /// The poster width last told to the worker, in physical pixels.
    poster_width_sent: Option<u32>,
    error_action: Option<ErrorAction>,
    placeholder_timer: Timer,
    prefetch_timer: Timer,
}

impl PickView {
    pub(super) fn new(media: MediaChoice) -> PickView {
        PickView {
            card: CardSlot::new(),
            excluded: None,
            pending: None,
            requested_at: None,
            earlier: false,
            media,
            genres: Vec::new(),
            genre_options: Rc::new(VecModel::default()),
            poster_width_sent: None,
            error_action: None,
            placeholder_timer: Timer::default(),
            prefetch_timer: Timer::default(),
        }
    }

    pub(super) fn media(&self) -> MediaChoice {
        self.media
    }

    pub(super) fn set_media(&mut self, media: MediaChoice) {
        self.media = media;
    }
}

fn string_model(labels: Vec<&'static str>) -> ModelRc<SharedString> {
    let labels: Vec<SharedString> = labels.into_iter().map(SharedString::from).collect();
    ModelRc::new(VecModel::from(labels))
}

fn media_index(media: MediaChoice) -> i32 {
    match media {
        MediaChoice::Movies => 0,
        MediaChoice::Series => 1,
        MediaChoice::Both => 2,
    }
}

pub(super) fn media_choice(index: i32) -> MediaChoice {
    match index {
        0 => MediaChoice::Movies,
        1 => MediaChoice::Series,
        _ => MediaChoice::Both,
    }
}

fn exclusion(reason: Option<ExclusionReason>) -> Exclusion {
    match reason {
        None => Exclusion::None,
        Some(ExclusionReason::Seen) => Exclusion::Seen,
        Some(ExclusionReason::NotInterested) => Exclusion::NotInterested,
    }
}

/// Title, message and way out for a failed pick.
fn describe(error: &PickError, filters_narrow: bool) -> (&'static str, String, ErrorAction) {
    match error {
        PickError::NeedServices => (
            "Choose your services",
            "Select the streaming services you have, or turn on free titles.".to_owned(),
            ErrorAction::ChooseServices,
        ),
        PickError::NothingMatches if filters_narrow => (
            "Nothing matches",
            "Try looser filters: fewer genres, a lower IMDb minimum or a wider year range.".to_owned(),
            ErrorAction::ResetFilters,
        ),
        PickError::NothingMatches => (
            "Nothing matches",
            "Nothing on your services qualified. Picks count subscription and free titles, not \
             stores that only rent or sell. Add services or turn on free titles."
                .to_owned(),
            ErrorAction::ChooseServices,
        ),
        PickError::Api(ApiError::Unauthorized) => (
            "TMDB token needed",
            "Add your TMDB token to pick titles.".to_owned(),
            ErrorAction::AddToken,
        ),
        PickError::Api(e) if e.is_connectivity() => (
            "Couldn't reach TMDB",
            format!("{e}. Check your connection, then try again."),
            ErrorAction::Retry,
        ),
        PickError::Api(e) => ("Something went wrong", format!("{e}."), ErrorAction::Retry),
    }
}

fn action_label(action: ErrorAction) -> &'static str {
    match action {
        ErrorAction::ChooseServices => "Choose services",
        ErrorAction::ResetFilters => "Reset filters",
        ErrorAction::AddToken => "Add token",
        ErrorAction::Retry => "Try again",
    }
}

impl Controller {
    pub(super) fn init_pick(&mut self, ui: &AppWindow) {
        let state = ui.global::<PickState>();
        state.set_groups(self.pick.card.groups_model());
        state.set_genres(ModelRc::from(self.pick.genre_options.clone()));
        state.set_rating_options(string_model(filters::labels(&RATINGS)));
        state.set_votes_options(string_model(filters::labels(&VOTES)));
        state.set_runtime_options(string_model(filters::labels(&RUNTIMES)));
        state.set_status(PickStatus::Idle);
        self.refresh_filter_bar(ui, true);
        self.refresh_idle_message(ui);
    }

    // ---- Requests ----------------------------------------------------------------------

    /// "Pick for me" / "Pick again", Space or Enter.
    pub fn pick(&mut self, ui: &AppWindow) {
        if matches!(self.pick.pending, Some((_, Request::Pick))) || !self.settings.setup_complete {
            return;
        }
        self.start_request(ui, Request::Pick);
    }

    pub fn previous(&mut self, ui: &AppWindow) {
        if self.pick.pending.is_some() {
            return;
        }
        // After a failed pick, "Previous" first returns to the card that was on screen.
        if ui.global::<PickState>().get_status() == PickStatus::Failed && self.pick.card.card().is_some() {
            self.show_pick_state(ui);
            self.show_slot(ui, Surface::Pick);
            return;
        }
        if self.pick.earlier {
            self.start_request(ui, Request::Previous);
        }
    }

    /// Sends a request; a newer one replaces any still waiting.
    fn start_request(&mut self, ui: &AppWindow, request: Request) {
        let id = self.next_request_id();
        self.pick.pending = Some((id, request));
        self.pick.requested_at = Some(Instant::now());
        let state = ui.global::<PickState>();
        state.set_busy(request == Request::Pick);
        state.set_can_go_back(false);
        Self::later(&self.pick.placeholder_timer, &self.weak, PLACEHOLDER_DELAY, move |c, ui| {
            c.show_pick_placeholder(ui, id);
        });
        self.send(match request {
            Request::Pick => Command::Pick {
                id,
                media: self.pick.media,
            },
            Request::Previous => Command::Previous { id },
        });
    }

    fn show_pick_placeholder(&mut self, ui: &AppWindow, id: u64) {
        if self.pick.pending.is_some_and(|(pending, _)| pending == id) {
            self.release_card_images(ui, Surface::Pick);
            ui.global::<PickState>().set_status(PickStatus::Picking);
        }
    }

    /// Takes the answer to `id` if it's the request waiting, else ignores it.
    fn answer(&mut self, id: u64) -> bool {
        if !self.pick.pending.is_some_and(|(pending, _)| pending == id) {
            return false;
        }
        self.pick.pending = None;
        self.pick.placeholder_timer.stop();
        true
    }

    pub(super) fn on_card(
        &mut self,
        ui: &AppWindow,
        id: u64,
        card: TitleCard,
        can_go_back: bool,
        excluded: Option<ExclusionReason>,
    ) {
        if !self.answer(id) {
            return;
        }
        self.pick.earlier = can_go_back;
        self.pick.excluded = excluded;
        self.present_card(ui, Surface::Pick, id, card);
    }

    /// The new card is on screen: the page around it follows.
    pub(super) fn pick_card_revealed(&mut self, ui: &AppWindow, with_poster: bool) {
        if self.pick.pending.is_none()
            && let Some(clicked) = self.pick.requested_at.take()
        {
            let poster = if with_poster { "with" } else { "without" };
            log_info!("card on screen {} ms after the click, {poster} its poster", clicked.elapsed().as_millis());
        }
        self.show_pick_state(ui);
        if let Some(card) = self.pick.card.card() {
            let year = if card.year.is_empty() { String::new() } else { format!(" ({})", card.year) };
            let announcement = format!("{}{year}. {}.", card.name, card.verdict);
            ui.global::<PickState>().set_announcement(announcement.into());
        }
    }

    /// Status and buttons for the card on screen.
    fn show_pick_state(&self, ui: &AppWindow) {
        let state = ui.global::<PickState>();
        state.set_busy(matches!(self.pick.pending, Some((_, Request::Pick))));
        state.set_excluded(exclusion(self.pick.excluded));
        state.set_has_picked(true);
        state.set_can_go_back(self.pick.pending.is_none() && self.pick.earlier);
        state.set_status(PickStatus::Shown);
    }

    pub(super) fn on_pick_failed(&mut self, ui: &AppWindow, id: u64, error: PickError) {
        if !self.answer(id) {
            return;
        }
        let narrow = filters::differs_from_default(&self.settings.filters);
        let (title, message, action) = describe(&error, narrow);
        self.pick.error_action = Some(action);
        self.release_card_images(ui, Surface::Pick);
        let state = ui.global::<PickState>();
        state.set_busy(false);
        state.set_error_title(title.into());
        state.set_error_message(message.as_str().into());
        state.set_error_action(action_label(action).into());
        state.set_can_go_back(self.pick.card.card().is_some() || self.pick.earlier);
        state.set_status(PickStatus::Failed);
        state.set_announcement(format!("{title}. {message}").into());
    }

    pub fn run_error_action(&mut self, ui: &AppWindow) {
        match self.pick.error_action {
            Some(ErrorAction::ChooseServices) => self.set_sheet_open(ui, true),
            Some(ErrorAction::ResetFilters) => {
                self.reset_filters(ui);
                self.pick(ui);
            }
            Some(ErrorAction::AddToken) => self.open_token_dialog(ui, ""),
            Some(ErrorAction::Retry) => self.pick(ui),
            None => {}
        }
    }

    pub(super) fn on_card_imdb(&mut self, ui: &AppWindow, id: u64, text: String, badge: ImdbBadge) {
        if self.pick.card.id() != Some(id) {
            return;
        }
        let Some(card) = self.pick.card.card_mut() else { return };
        card.imdb = text;
        card.imdb_badge = badge;
        let data = card_data(card);
        let serial = ui.global::<PickState>().get_card_serial();
        write_card(ui, Surface::Pick, data, serial);
    }

    pub fn exclude(&mut self, _ui: &AppWindow, which: Exclusion) {
        let Some(card) = self.pick.card.card() else { return };
        let title = card.title;
        let command = match which {
            Exclusion::Seen => Command::Exclude {
                title,
                reason: ExclusionReason::Seen,
            },
            Exclusion::NotInterested => Command::Exclude {
                title,
                reason: ExclusionReason::NotInterested,
            },
            _ => Command::Unexclude { title },
        };
        self.send(command);
    }

    pub(super) fn on_excluded(&mut self, ui: &AppWindow, title: TitleRef, reason: Option<ExclusionReason>) {
        if self.pick.card.card().is_some_and(|card| card.title == title) {
            self.pick.excluded = reason;
            ui.global::<PickState>().set_excluded(exclusion(reason));
        }
    }

    /// A service button on the Pick card.
    pub fn pick_open_service(&mut self, _ui: &AppWindow, group: i32, chip: i32) {
        self.open_service(Surface::Pick, group, chip);
    }

    /// Tells the worker the poster's physical width, so prepared picks download that size,
    /// including the first one, which is prepared before any card asks for images.
    pub(super) fn send_poster_width(&mut self, ui: &AppWindow) {
        let Some(card_box) = self.pick.card.card_box else { return };
        let width = (card_box.poster_width * ui.window().scale_factor()).round().max(1.0) as u32;
        if self.pick.poster_width_sent != Some(width) {
            self.pick.poster_width_sent = Some(width);
            self.send(Command::PosterWidth(width));
        }
    }

    // ---- Filters -----------------------------------------------------------------------

    pub(super) fn on_genres(&mut self, ui: &AppWindow, names: Vec<String>) {
        self.pick.genres = names;
        self.refresh_filter_bar(ui, false);
    }

    pub fn select_media(&mut self, ui: &AppWindow, index: i32) {
        self.pick.media = media_choice(index);
        self.refresh_filter_bar(ui, false);
        self.pick_inputs_changed(ui);
    }

    pub fn toggle_genre(&mut self, ui: &AppWindow, index: i32) {
        let Some(name) = usize::try_from(index).ok().and_then(|i| self.pick.genres.get(i)).cloned() else {
            return;
        };
        let chosen = &mut self.settings.filters.genres;
        match chosen.iter().position(|g| g.eq_ignore_ascii_case(&name)) {
            Some(position) => {
                chosen.remove(position);
            }
            None => chosen.push(name),
        }
        self.filters_changed(ui, false);
    }

    pub fn clear_genres(&mut self, ui: &AppWindow) {
        self.settings.filters.genres.clear();
        self.filters_changed(ui, false);
    }

    pub fn select_rating(&mut self, ui: &AppWindow, index: i32) {
        if let Some(&(value, _)) = usize::try_from(index).ok().and_then(|i| RATINGS.get(i)) {
            self.settings.filters.min_imdb_rating = value;
            self.filters_changed(ui, false);
        }
    }

    pub fn select_votes(&mut self, ui: &AppWindow, index: i32) {
        if let Some(&(value, _)) = usize::try_from(index).ok().and_then(|i| VOTES.get(i)) {
            self.settings.filters.min_imdb_votes = value;
            self.filters_changed(ui, false);
        }
    }

    pub fn select_runtime(&mut self, ui: &AppWindow, index: i32) {
        if let Some(&(value, _)) = usize::try_from(index).ok().and_then(|i| RUNTIMES.get(i)) {
            self.settings.filters.max_runtime = value;
            self.filters_changed(ui, false);
        }
    }

    /// Typing in the year boxes: complete years apply at once, partial ones wait.
    pub fn edit_years(&mut self, ui: &AppWindow, from: &str, to: &str) {
        let state = ui.global::<PickState>();
        match filters::parse_year_range(from, to) {
            Ok(Some((from, to))) => {
                state.set_year_error(SharedString::new());
                let f = &mut self.settings.filters;
                if (f.year_from, f.year_to) != (from, to) {
                    f.year_from = from;
                    f.year_to = to;
                    self.filters_changed(ui, false);
                }
            }
            Ok(None) => state.set_year_error(SharedString::new()),
            Err(hint) => state.set_year_error(hint.into()),
        }
    }

    /// A decade preset; -1 clears the years.
    pub fn choose_decade(&mut self, ui: &AppWindow, index: i32) {
        let (from, to) = usize::try_from(index)
            .ok()
            .and_then(|i| DECADES.get(i).copied())
            .unwrap_or((None, None));
        self.settings.filters.year_from = from;
        self.settings.filters.year_to = to;
        ui.global::<PickState>().set_year_error(SharedString::new());
        self.filters_changed(ui, true);
    }

    pub fn reset_filters(&mut self, ui: &AppWindow) {
        self.settings.filters = Filters::default();
        self.pick.media = self.settings.default_media;
        ui.global::<PickState>().set_year_error(SharedString::new());
        self.filters_changed(ui, true);
    }

    /// Saves the filters and updates the bar. `rewrite_years` replaces the year boxes'
    /// text, which typing must not do.
    fn filters_changed(&mut self, ui: &AppWindow, rewrite_years: bool) {
        self.settings.sanitize();
        self.save_settings();
        self.refresh_filter_bar(ui, rewrite_years);
        self.pick_inputs_changed(ui);
    }

    pub(super) fn refresh_filter_bar(&self, ui: &AppWindow, rewrite_years: bool) {
        let f = &self.settings.filters;
        let state = ui.global::<PickState>();
        state.set_media_index(media_index(self.pick.media));

        let options: Vec<GenreOption> = self
            .pick
            .genres
            .iter()
            .map(|name| GenreOption {
                name: name.as_str().into(),
                selected: f.genres.iter().any(|g| g.eq_ignore_ascii_case(name)),
            })
            .collect();
        let model = &self.pick.genre_options;
        if model.row_count() == options.len() {
            for (row, option) in options.into_iter().enumerate() {
                if model.row_data(row).as_ref() != Some(&option) {
                    model.set_row_data(row, option);
                }
            }
        } else {
            model.set_vec(options);
        }
        state.set_genre_summary(filters::genre_summary(&f.genres).into());
        state.set_genre_note(if self.pick.genres.is_empty() {
            "The genre list loads once TMDB is reachable.".into()
        } else {
            SharedString::new()
        });

        state.set_rating_index(filters::rating_index(f.min_imdb_rating) as i32);
        state.set_votes_index(filters::votes_index(f.min_imdb_votes) as i32);
        state.set_imdb_summary(filters::imdb_summary(f).into());
        state.set_imdb_filtered(f.min_imdb_rating.is_some() || f.min_imdb_votes.is_some());

        if rewrite_years {
            let text = |year: Option<u16>| year.map(|y| y.to_string()).unwrap_or_default();
            state.set_year_from(text(f.year_from).into());
            state.set_year_to(text(f.year_to).into());
        }
        state.set_year_summary(filters::year_summary(f.year_from, f.year_to).into());
        state.set_decade_index(filters::decade_index(f.year_from, f.year_to).map_or(-1, |i| i as i32));

        state.set_runtime_index(filters::runtime_index(f.max_runtime) as i32);
        state.set_runtime_summary(filters::runtime_summary(f.max_runtime).into());
        state.set_runtime_applies(self.pick.media != MediaChoice::Series);

        state.set_filters_changed(
            filters::differs_from_default(f) || self.pick.media != self.settings.default_media,
        );
    }

    /// Something that shapes picks changed (filters, services, region, type). A pick on its
    /// way starts over with the change, and the next pick is prepared again once changes
    /// settle.
    pub(super) fn pick_inputs_changed(&mut self, ui: &AppWindow) {
        if matches!(self.pick.pending, Some((_, Request::Pick))) {
            self.start_request(ui, Request::Pick);
        }
        self.schedule_prefetch();
        self.refresh_idle_message(ui);
    }

    pub(super) fn schedule_prefetch(&mut self) {
        if !self.settings.setup_complete {
            return;
        }
        Self::later(&self.pick.prefetch_timer, &self.weak, PREFETCH_SETTLE, |c, _ui| {
            c.send(Command::Prefetch { media: c.pick.media });
        });
    }

    /// What a pick will draw from, shown before the first pick.
    pub(super) fn refresh_idle_message(&self, ui: &AppWindow) {
        let region = self.effective_region();
        let place = if region.is_empty() {
            String::new()
        } else {
            format!(" in {}", self.region_name(&region))
        };
        let names: Vec<String> = self.catalog.as_ref().map_or_else(Vec::new, |catalog| {
            catalog
                .groups
                .iter()
                .filter(|g| self.settings.services.state(g) != SelectionState::Off)
                .map(|g| g.name.clone())
                .collect()
        });
        let free = self.settings.include_free;
        let message = if names.is_empty() && self.catalog.is_none() && !self.settings.services.is_empty() {
            "Loading your services…".to_owned()
        } else {
            match (names.is_empty(), free) {
                (false, true) => {
                    // "Netflix, Stan and anything free", not "Netflix and Stan and ...".
                    let listed = match names.len() {
                        1..=3 => names.join(", "),
                        n => format!("{}, {} more services", names[..3].join(", "), n - 3),
                    };
                    format!("Picks come from {listed} and anything free to stream{place}.")
                }
                (false, false) => format!("Picks come from {}{place}.", format::join_names(&names)),
                (true, true) => {
                    format!("No services selected, so picks come from titles free to stream{place}.")
                }
                (true, false) => {
                    "Choose your services in My Services, or turn on free titles.".to_owned()
                }
            }
        };
        ui.global::<PickState>().set_idle_message(message.into());
    }
}
