//! The Pick screen on the UI thread: pick and "Previous" requests, the card on screen and its
//! images, the filter bar, and scheduling the background pick that keeps "Pick again"
//! instant.

use std::rc::Rc;
use std::time::{Duration, Instant};

use randomovie::card::{ImdbBadge, TitleCard};
use randomovie::error::ApiError;
use randomovie::format;
use randomovie::media::{MediaChoice, TitleRef};
use randomovie::picker::filters::{self, DECADES, RATINGS, RUNTIMES, VOTES};
use randomovie::picker::{ChipGroupKind, PickError, Verdict};
use randomovie::platform;
use randomovie::services::SelectionState;
use randomovie::store::{ExclusionReason, Filters};
use randomovie::{log_info, log_warn};
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, Rgba8Pixel, SharedString, Timer, TimerMode, VecModel};

use super::{Controller, logo_key};
use crate::worker::{CardImageKind, Command, ImageSpec};
use crate::{
    AppState, AppWindow, CardMetrics, Exclusion, GenreOption, Page, PickState, PickStatus, Theme,
    TitleCardData, WatchChip, WatchGroup,
};

/// Logo size on the "Where to watch" chips, in logical pixels.
pub(super) const CHIP_LOGO: f32 = 24.0;
/// An answer within this time replaces the card directly (a prepared pick is instant); a
/// slower one shows placeholders first.
const PLACEHOLDER_DELAY: Duration = Duration::from_millis(150);
/// A new card waits this long for its poster, so text and poster appear together; a
/// poster that takes longer (not yet downloaded) appears when it arrives.
const POSTER_WAIT: Duration = Duration::from_millis(250);
/// The card's images are decoded again for a new size once resizing pauses this long.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
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

/// The card's size and its poster's, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CardBox {
    width: f32,
    height: f32,
    poster_width: f32,
    poster_height: f32,
}

/// The images last requested for the card on screen, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImagesRequested {
    id: u64,
    poster: (u32, u32),
    backdrop: (u32, u32),
}

struct ShownCard {
    id: u64,
    card: TitleCard,
    excluded: Option<ExclusionReason>,
}

pub(super) struct PickView {
    next_id: u64,
    /// The request waiting for an answer.
    pending: Option<(u64, Request)>,
    /// When the last request was made, for the log's click-to-card time.
    requested_at: Option<Instant>,
    /// The card on screen (kept while an error shows, so "Previous" can return to it).
    shown: Option<ShownCard>,
    /// The worker has an earlier pick before the card on screen.
    earlier: bool,
    /// Movies / Series / Both: starts at the default type, not saved.
    media: MediaChoice,
    /// Unified genre names from TMDB.
    genres: Vec<String>,
    genre_options: Rc<VecModel<GenreOption>>,
    card_box: Option<CardBox>,
    /// The poster width last told to the worker, in physical pixels.
    poster_width_sent: Option<u32>,
    images: Option<ImagesRequested>,
    /// `shown` arrived but waits for its poster before it replaces the screen.
    awaiting_poster: bool,
    /// Images decoded for `shown` while it waits.
    held_poster: Option<slint::Image>,
    held_backdrop: Option<slint::Image>,
    groups: Rc<VecModel<WatchGroup>>,
    chip_models: Vec<Rc<VecModel<WatchChip>>>,
    /// Logo path of each chip, per group.
    chip_logos: Vec<Vec<Option<String>>>,
    error_action: Option<ErrorAction>,
    serial: i32,
    placeholder_timer: Timer,
    poster_timer: Timer,
    resize_timer: Timer,
    prefetch_timer: Timer,
}

impl PickView {
    pub(super) fn new(media: MediaChoice) -> PickView {
        PickView {
            next_id: 0,
            pending: None,
            requested_at: None,
            shown: None,
            earlier: false,
            media,
            genres: Vec::new(),
            genre_options: Rc::new(VecModel::default()),
            card_box: None,
            poster_width_sent: None,
            images: None,
            awaiting_poster: false,
            held_poster: None,
            held_backdrop: None,
            groups: Rc::new(VecModel::default()),
            chip_models: Vec::new(),
            chip_logos: Vec::new(),
            error_action: None,
            serial: 0,
            placeholder_timer: Timer::default(),
            poster_timer: Timer::default(),
            resize_timer: Timer::default(),
            prefetch_timer: Timer::default(),
        }
    }

    pub(super) fn media(&self) -> MediaChoice {
        self.media
    }

    pub(super) fn set_media(&mut self, media: MediaChoice) {
        self.media = media;
    }

    /// Logo paths of the chips on screen.
    pub(super) fn chip_logo_paths(&self) -> impl Iterator<Item = &str> {
        self.chip_logos.iter().flatten().filter_map(|path| path.as_deref())
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

fn group_kind(kind: ChipGroupKind) -> i32 {
    match kind {
        ChipGroupKind::YourServices => 0,
        ChipGroupKind::Free => 1,
        ChipGroupKind::OtherSubscriptions => 2,
        ChipGroupKind::RentBuy => 3,
    }
}

/// The card's text for Slint. The IMDb star is drawn as an icon, so the text drops it.
fn card_data(card: &TitleCard) -> TitleCardData {
    TitleCardData {
        name: card.name.as_str().into(),
        year: card.year.as_str().into(),
        kind: card.kind.into(),
        facts: card.facts.as_str().into(),
        overview: card.overview.as_str().into(),
        imdb: card.imdb.trim_start_matches('★').trim_start().into(),
        imdb_state: match card.imdb_badge {
            ImdbBadge::Rated => 0,
            ImdbBadge::Unavailable => 1,
            ImdbBadge::Loading => 2,
        },
        verdict: card.verdict.as_str().into(),
        verdict_state: match card.watch.verdict {
            Verdict::Included(_) => 0,
            Verdict::Free(_) => 1,
            Verdict::NotOnServices => 2,
            Verdict::Unavailable => 3,
        },
        region_name: card.region_name.as_str().into(),
        link: card.watch.link.as_deref().unwrap_or_default().into(),
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
            "Nothing on your services qualified. Add services or turn on free titles.".to_owned(),
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
        state.set_groups(ModelRc::from(self.pick.groups.clone()));
        state.set_genres(ModelRc::from(self.pick.genre_options.clone()));
        state.set_rating_options(string_model(filters::labels(&RATINGS)));
        state.set_votes_options(string_model(filters::labels(&VOTES)));
        state.set_runtime_options(string_model(filters::labels(&RUNTIMES)));
        state.set_status(PickStatus::Idle);
        self.refresh_filter_bar(ui, true);
        self.refresh_idle_message(ui);
    }

    /// Runs `action` after `delay`, replacing whatever `timer` was waiting to do.
    fn later(
        timer: &Timer,
        weak: &slint::Weak<AppWindow>,
        delay: Duration,
        action: impl FnOnce(&mut Controller, &AppWindow) + 'static,
    ) {
        let weak = weak.clone();
        let mut action = Some(action);
        timer.start(TimerMode::SingleShot, delay, move || {
            if let Some(action) = action.take() {
                crate::ui::with_controller(&weak, action);
            }
        });
    }

    fn pick_visible(ui: &AppWindow) -> bool {
        let app = ui.global::<AppState>();
        !app.get_onboarding() && app.get_page() == Page::Pick
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
        if ui.global::<PickState>().get_status() == PickStatus::Failed && self.pick.shown.is_some() {
            self.show_card(ui);
            return;
        }
        if self.pick.earlier {
            self.start_request(ui, Request::Previous);
        }
    }

    /// Sends a request; a newer one replaces any still waiting.
    fn start_request(&mut self, ui: &AppWindow, request: Request) {
        self.pick.next_id += 1;
        let id = self.pick.next_id;
        self.pick.pending = Some((id, request));
        self.pick.requested_at = Some(Instant::now());
        let state = ui.global::<PickState>();
        state.set_busy(request == Request::Pick);
        state.set_can_go_back(false);
        Self::later(&self.pick.placeholder_timer, &self.weak, PLACEHOLDER_DELAY, move |c, ui| {
            c.show_placeholder(ui, id);
        });
        self.send(match request {
            Request::Pick => Command::Pick {
                id,
                media: self.pick.media,
            },
            Request::Previous => Command::Previous { id },
        });
    }

    fn show_placeholder(&mut self, ui: &AppWindow, id: u64) {
        if self.pick.pending.is_some_and(|(pending, _)| pending == id) {
            self.release_card_images(ui);
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
        let has_poster = card.poster_path.is_some();
        self.pick.earlier = can_go_back;
        self.pick.serial = self.pick.serial.wrapping_add(1);
        self.pick.shown = Some(ShownCard { id, card, excluded });
        self.pick.images = None;
        self.pick.held_poster = None;
        self.pick.held_backdrop = None;
        if has_poster && Self::pick_visible(ui) && self.pick.card_box.is_some() {
            // The old card stays until the poster is decoded (or POSTER_WAIT passes).
            self.pick.awaiting_poster = true;
            self.request_card_images(ui);
            Self::later(&self.pick.poster_timer, &self.weak, POSTER_WAIT, move |c, ui| {
                c.reveal(ui, id);
            });
        } else {
            self.reveal(ui, id);
        }
    }

    /// Replaces the screen with the card that arrived, with whatever images are ready.
    fn reveal(&mut self, ui: &AppWindow, id: u64) {
        if !self.pick.shown.as_ref().is_some_and(|shown| shown.id == id) {
            return;
        }
        let first_show = self.pick.awaiting_poster || self.pick.images.is_none();
        self.pick.awaiting_poster = false;
        self.pick.poster_timer.stop();
        if self.pick.pending.is_none()
            && let Some(clicked) = self.pick.requested_at.take()
        {
            let poster = if self.pick.held_poster.is_some() { "with" } else { "without" };
            log_info!("card on screen {} ms after the click, {poster} its poster", clicked.elapsed().as_millis());
        }
        let state = ui.global::<PickState>();
        state.set_poster(self.pick.held_poster.take().unwrap_or_default());
        state.set_backdrop(self.pick.held_backdrop.take().unwrap_or_default());
        self.show_card(ui);
        if first_show && let Some(shown) = &self.pick.shown {
            let card = &shown.card;
            let year = if card.year.is_empty() { String::new() } else { format!(" ({})", card.year) };
            state.set_announcement(format!("{}{year}. {}.", card.name, card.verdict).into());
        }
        self.request_logos(ui);
    }

    /// Puts the card in `shown` on screen.
    fn show_card(&mut self, ui: &AppWindow) {
        let Some(shown) = &self.pick.shown else { return };
        let state = ui.global::<PickState>();
        state.set_busy(matches!(self.pick.pending, Some((_, Request::Pick))));
        state.set_card(card_data(&shown.card));
        state.set_card_serial(self.pick.serial);
        state.set_excluded(exclusion(shown.excluded));
        state.set_has_picked(true);
        state.set_can_go_back(self.pick.pending.is_none() && self.pick.earlier);
        self.fill_groups(ui);
        state.set_status(PickStatus::Shown);
        self.request_card_images(ui);
    }

    fn fill_groups(&mut self, ui: &AppWindow) {
        let px = Self::logo_px(ui, CHIP_LOGO);
        let Some(shown) = &self.pick.shown else { return };
        let mut groups = Vec::new();
        let mut models = Vec::new();
        let mut logos = Vec::new();
        for group in &shown.card.watch.groups {
            let chips: Vec<WatchChip> = group
                .chips
                .iter()
                .map(|chip| WatchChip {
                    name: chip.name.as_str().into(),
                    logo: chip
                        .logo_path
                        .as_deref()
                        .and_then(|path| self.logos.get(&logo_key(path, px)))
                        .cloned()
                        .unwrap_or_default(),
                })
                .collect();
            let model = Rc::new(VecModel::from(chips));
            groups.push(WatchGroup {
                label: group.kind.label().into(),
                kind: group_kind(group.kind),
                chips: ModelRc::from(model.clone()),
            });
            models.push(model);
            logos.push(group.chips.iter().map(|c| c.logo_path.clone()).collect());
        }
        self.pick.chip_models = models;
        self.pick.chip_logos = logos;
        self.pick.groups.set_vec(groups);
    }

    pub(super) fn on_pick_failed(&mut self, ui: &AppWindow, id: u64, error: PickError) {
        if !self.answer(id) {
            return;
        }
        let narrow = filters::differs_from_default(&self.settings.filters);
        let (title, message, action) = describe(&error, narrow);
        self.pick.error_action = Some(action);
        self.release_card_images(ui);
        let state = ui.global::<PickState>();
        state.set_busy(false);
        state.set_error_title(title.into());
        state.set_error_message(message.as_str().into());
        state.set_error_action(action_label(action).into());
        state.set_can_go_back(self.pick.shown.is_some() || self.pick.earlier);
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
        let Some(shown) = self.pick.shown.as_mut().filter(|shown| shown.id == id) else {
            return;
        };
        shown.card.imdb = text;
        shown.card.imdb_badge = badge;
        ui.global::<PickState>().set_card(card_data(&shown.card));
    }

    pub fn exclude(&mut self, _ui: &AppWindow, which: Exclusion) {
        let Some(shown) = &self.pick.shown else { return };
        let title = shown.card.title;
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
        if let Some(shown) = self.pick.shown.as_mut()
            && shown.card.title == title
        {
            shown.excluded = reason;
            ui.global::<PickState>().set_excluded(exclusion(reason));
        }
    }

    pub fn open_where_to_watch(&mut self, _ui: &AppWindow) {
        let link = self.pick.shown.as_ref().and_then(|s| s.card.watch.link.as_deref());
        if let Some(link) = link
            && !platform::shell::open_url(link)
        {
            log_warn!("refused to open a non-web link");
        }
    }

    // ---- Card images -------------------------------------------------------------------

    pub fn card_resized(&mut self, ui: &AppWindow, width: f32, height: f32, poster_width: f32, poster_height: f32) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let card_box = CardBox {
            width,
            height,
            poster_width,
            poster_height,
        };
        if self.pick.card_box == Some(card_box) {
            return;
        }
        let first = self.pick.card_box.is_none();
        self.pick.card_box = Some(card_box);
        self.send_poster_width(ui);
        if first {
            self.request_card_images(ui);
        } else {
            // While the window is being resized the old images stretch; exact ones follow.
            Self::later(&self.pick.resize_timer, &self.weak, RESIZE_SETTLE, |c, ui| {
                c.request_card_images(ui);
            });
        }
    }

    /// Tells the worker the poster's physical width, so prepared picks download that size,
    /// including the first one, which is prepared before any card asks for images.
    pub(super) fn send_poster_width(&mut self, ui: &AppWindow) {
        let Some(card_box) = self.pick.card_box else { return };
        let width = (card_box.poster_width * ui.window().scale_factor()).round().max(1.0) as u32;
        if self.pick.poster_width_sent != Some(width) {
            self.pick.poster_width_sent = Some(width);
            self.send(Command::PosterWidth(width));
        }
    }

    /// Asks for the poster and backdrop at the exact physical size the card shows them.
    pub(super) fn request_card_images(&mut self, ui: &AppWindow) {
        let showing = ui.global::<PickState>().get_status() == PickStatus::Shown;
        if !Self::pick_visible(ui) || !(showing || self.pick.awaiting_poster) {
            return;
        }
        let (Some(shown), Some(card_box)) = (&self.pick.shown, self.pick.card_box) else {
            return;
        };
        let scale = ui.window().scale_factor();
        let px = |logical: f32| (logical * scale).round().max(1.0) as u32;
        let wanted = ImagesRequested {
            id: shown.id,
            poster: (px(card_box.poster_width), px(card_box.poster_height)),
            backdrop: (px(card_box.width), px(card_box.height)),
        };
        if self.pick.images == Some(wanted) {
            return;
        }
        let poster_radius = ui.global::<CardMetrics>().get_poster_radius() * scale;
        let card_radius = ui.global::<Theme>().get_radius_card() * scale;
        let command = Command::LoadCardImages {
            id: shown.id,
            poster: shown.card.poster_path.clone().map(|path| ImageSpec {
                path,
                width: wanted.poster.0,
                height: wanted.poster.1,
                radius: poster_radius,
            }),
            backdrop: shown.card.backdrop_path.clone().map(|path| ImageSpec {
                path,
                width: wanted.backdrop.0,
                height: wanted.backdrop.1,
                radius: card_radius,
            }),
        };
        self.pick.images = Some(wanted);
        self.send(command);
    }

    pub(super) fn on_card_image(
        &mut self,
        ui: &AppWindow,
        id: u64,
        kind: CardImageKind,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    ) {
        let current = self.pick.images.is_some_and(|images| images.id == id);
        if !current || !Self::pick_visible(ui) {
            return; // replaced, or released while another screen shows
        }
        let image = slint::Image::from_rgba8(pixels);
        if self.pick.awaiting_poster {
            match kind {
                CardImageKind::Poster => {
                    self.pick.held_poster = Some(image);
                    self.reveal(ui, id);
                }
                CardImageKind::Backdrop => self.pick.held_backdrop = Some(image),
            }
            return;
        }
        let state = ui.global::<PickState>();
        match kind {
            CardImageKind::Poster => state.set_poster(image),
            CardImageKind::Backdrop => state.set_backdrop(image),
        }
    }

    /// Frees the decoded poster and backdrop (another screen shows, or the card changes).
    pub(super) fn release_card_images(&mut self, ui: &AppWindow) {
        if self.pick.awaiting_poster {
            // The waiting card shows now, without images; they're requested again later.
            if let Some(id) = self.pick.shown.as_ref().map(|shown| shown.id) {
                self.pick.held_poster = None;
                self.pick.held_backdrop = None;
                self.reveal(ui, id);
            }
        }
        let state = ui.global::<PickState>();
        state.set_poster(slint::Image::default());
        state.set_backdrop(slint::Image::default());
        self.pick.images = None;
    }

    /// The Pick page came back into view, or the scale or network changed.
    pub(super) fn reload_card_images(&mut self, ui: &AppWindow) {
        let state = ui.global::<PickState>();
        let missing = state.get_poster().size().width == 0 || state.get_backdrop().size().width == 0;
        if missing {
            self.pick.images = None;
        }
        self.request_card_images(ui);
    }

    /// Puts a decoded logo into every chip that shows it.
    pub(super) fn place_chip_logo(&self, path: &str, image: &slint::Image) {
        for (group, logos) in self.pick.chip_logos.iter().enumerate() {
            let Some(model) = self.pick.chip_models.get(group) else { continue };
            for (row, logo) in logos.iter().enumerate() {
                if logo.as_deref() == Some(path)
                    && let Some(mut chip) = model.row_data(row)
                {
                    chip.logo = image.clone();
                    model.set_row_data(row, chip);
                }
            }
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
