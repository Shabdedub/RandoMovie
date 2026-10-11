//! The Library: titles marked "Seen it" (the Watched tab) or "Not interested", read from the
//! database each time the page shows, and filtered by type and genre. A row opens the title
//! card. A title can be taken off its list, so it can be picked again, or moved to the other
//! list, with Undo for either.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use randomovie::card::TitleCard;
use randomovie::error::ApiError;
use randomovie::format;
use randomovie::imdb::ImdbStatus;
use randomovie::library::{LibraryFilter, genre_options};
use randomovie::media::{MediaType, TitleRef};
use randomovie::picker::filters::genre_summary;
use randomovie::store::ExclusionReason;
use randomovie::util::time::now_unix;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, VecModel};

use super::Controller;
use super::card::{CardSlot, Surface};
use super::pick::PLACEHOLDER_DELAY;
use super::search::{error_message, thumb_key};
use crate::worker::{Command, ImageSpec, LibraryItem};
use crate::{AppWindow, GenreOption, LibraryRow, LibraryState};

const TABS: [ExclusionReason; 2] = [ExclusionReason::Seen, ExclusionReason::NotInterested];

pub(super) struct LibraryView {
    pub(super) card: CardSlot,
    /// Every marked title, the most recently marked first.
    items: Vec<LibraryItem>,
    /// The titles of the tab on screen, in the order of `rows`.
    shown: Vec<TitleRef>,
    rows: Rc<VecModel<LibraryRow>>,
    tabs: Rc<VecModel<SharedString>>,
    tab: ExclusionReason,
    /// For both lists, for this session.
    filter: LibraryFilter,
    /// The genre panel's chips, and their names in order.
    genre_options: Rc<VecModel<GenreOption>>,
    genre_names: Vec<String>,
    /// The newest load; answers to older ones are dropped.
    load_id: u64,
    thumbs: HashMap<String, Image>,
    pending_thumbs: HashSet<String>,
    /// The title on the card, or being opened.
    open_title: Option<TitleRef>,
    /// The request opening it.
    opening: Option<u64>,
    placeholder_timer: Timer,
    /// A removal or move sent to the worker and not yet confirmed: the title as it was.
    changing: Option<LibraryItem>,
    /// The last removal or move, as the title was before it, for Undo.
    undo: Option<LibraryItem>,
}

impl LibraryView {
    pub(super) fn new() -> LibraryView {
        LibraryView {
            card: CardSlot::new(),
            items: Vec::new(),
            shown: Vec::new(),
            rows: Rc::new(VecModel::default()),
            tabs: Rc::new(VecModel::from(TABS.map(|reason| SharedString::from(list_name(reason))).to_vec())),
            tab: ExclusionReason::Seen,
            filter: LibraryFilter::default(),
            genre_options: Rc::new(VecModel::default()),
            genre_names: Vec::new(),
            load_id: 0,
            thumbs: HashMap::new(),
            pending_thumbs: HashSet::new(),
            open_title: None,
            opening: None,
            placeholder_timer: Timer::default(),
            changing: None,
            undo: None,
        }
    }

    fn item(&self, title: TitleRef) -> Option<&LibraryItem> {
        self.items.iter().find(|item| item.title == title)
    }

    fn poster_path(&self, title: TitleRef) -> Option<&str> {
        self.item(title)?.summary.as_ref()?.poster_path.as_deref()
    }

    /// The title passes the filters.
    fn shows(&self, item: &LibraryItem) -> bool {
        let genres = item.summary.as_ref().and_then(|summary| summary.genres.as_deref());
        self.filter.matches(item.title.media, genres)
    }
}

/// The filter bar's Movies / Series / Both index.
fn media_index(media: Option<MediaType>) -> i32 {
    match media {
        Some(MediaType::Movie) => 0,
        Some(MediaType::Tv) => 1,
        None => 2,
    }
}

fn list_name(reason: ExclusionReason) -> &'static str {
    match reason {
        ExclusionReason::Seen => "Watched",
        ExclusionReason::NotInterested => "Not interested",
    }
}

fn other_list(reason: ExclusionReason) -> ExclusionReason {
    match reason {
        ExclusionReason::Seen => ExclusionReason::NotInterested,
        ExclusionReason::NotInterested => ExclusionReason::Seen,
    }
}

fn item_name(item: &LibraryItem) -> String {
    item.summary
        .as_ref()
        .map_or_else(|| "This title".to_owned(), |summary| summary.name.clone())
}

/// "Movie · 1999", or just "Movie" without a year.
fn meta(title: TitleRef, year: Option<u16>) -> String {
    match year {
        Some(year) => format!("{} · {year}", title.media.label()),
        None => title.media.label().to_owned(),
    }
}

fn new_row(item: &LibraryItem, thumb: Image, now: i64) -> LibraryRow {
    let (name, meta) = match &item.summary {
        Some(summary) => (summary.name.clone(), meta(item.title, summary.year)),
        None => ("Title details unavailable".to_owned(), item.title.media.label().to_owned()),
    };
    // Only a rating is shown; "unavailable" or "loading" would just be noise in the list.
    let imdb = match item.imdb {
        ImdbStatus::Rated(_) => format::imdb_badge(&item.imdb).trim_start_matches('★').trim_start().to_owned(),
        ImdbStatus::Unavailable | ImdbStatus::Loading => String::new(),
    };
    LibraryRow {
        name: name.into(),
        meta: meta.into(),
        thumb,
        imdb: imdb.into(),
        marked: format!("Marked {}", format::ago(now - item.marked_at)).into(),
        pending: item.summary.is_none() && !item.unavailable,
    }
}

impl Controller {
    pub(super) fn init_library(&mut self, ui: &AppWindow) {
        let state = ui.global::<LibraryState>();
        state.set_rows(ModelRc::from(self.library.rows.clone()));
        state.set_tabs(ModelRc::from(self.library.tabs.clone()));
        state.set_genres(ModelRc::from(self.library.genre_options.clone()));
        state.set_groups(self.library.card.groups_model());
    }

    /// The page shows: the list is read again, since titles are marked on Pick meanwhile. It
    /// opens with nothing selected (an open card keeps its title selected).
    pub(super) fn library_shown(&mut self, ui: &AppWindow) {
        let state = ui.global::<LibraryState>();
        if !state.get_card_open() {
            state.set_selected(-1);
        }
        self.reload_library();
        self.reload_card_images(ui, Surface::Library);
    }

    /// The page hides: its images are freed, and the last change can no longer be undone.
    pub(super) fn library_hidden(&mut self, ui: &AppWindow) {
        self.release_card_images(ui, Surface::Library);
        self.release_library_thumbs();
        self.clear_library_notice(ui);
    }

    pub(super) fn reload_library(&mut self) {
        let id = self.next_request_id();
        self.library.load_id = id;
        self.send(Command::LoadLibrary { id });
    }

    /// Back online or IMDb ratings ready: titles whose details or rating were missing get them.
    pub(super) fn refresh_library(&mut self, ui: &AppWindow) {
        if Self::surface_visible(ui, Surface::Library) {
            self.reload_library();
            self.request_library_thumbs(ui);
        }
    }

    pub(super) fn on_library(&mut self, ui: &AppWindow, id: u64, items: Vec<LibraryItem>) {
        if id != self.library.load_id {
            return;
        }
        self.library.items = items;
        self.fill_library_rows(ui);
    }

    /// A title marked before the Library existed, with its details now (or still without).
    pub(super) fn on_library_item(&mut self, ui: &AppWindow, id: u64, item: LibraryItem) {
        if id != self.library.load_id {
            return;
        }
        let Some(slot) = self.library.items.iter_mut().find(|slot| slot.title == item.title) else {
            return;
        };
        *slot = item.clone();
        if let Some(index) = self.library.shown.iter().position(|&title| title == item.title) {
            let thumb = self.library_thumb(ui, item.title);
            self.library.rows.set_row_data(index, new_row(&item, thumb, now_unix()));
        }
        self.request_library_thumbs(ui);
    }

    /// The tab's rows from `items`, through the filters. The selection follows its title while
    /// it's listed.
    fn fill_library_rows(&mut self, ui: &AppWindow) {
        let state = ui.global::<LibraryState>();
        let selected = usize::try_from(state.get_selected())
            .ok()
            .and_then(|index| self.library.shown.get(index))
            .copied();
        let now = now_unix();
        let tab = self.library.tab;
        let shown: Vec<TitleRef> = self
            .library
            .items
            .iter()
            .filter(|item| item.reason == tab && self.library.shows(item))
            .map(|item| item.title)
            .collect();
        let rows: Vec<LibraryRow> = shown
            .iter()
            .filter_map(|&title| self.library.item(title))
            .map(|item| new_row(item, self.library_thumb(ui, item.title), now))
            .collect();
        let selected = selected
            .and_then(|title| shown.iter().position(|&s| s == title))
            .map_or(-1, |index| index as i32);
        self.library.shown = shown;
        self.library.rows.set_vec(rows);
        state.set_tab(TABS.iter().position(|&reason| reason == tab).unwrap_or(0) as i32);
        state.set_selected(selected);
        state.set_loading(false);
        // The tabs count what the filters let through, as the list does.
        for (index, reason) in TABS.into_iter().enumerate() {
            let count = self
                .library
                .items
                .iter()
                .filter(|item| item.reason == reason && self.library.shows(item))
                .count();
            let label: SharedString = match count {
                0 => list_name(reason).into(),
                n => format!("{} ({n})", list_name(reason)).into(),
            };
            if self.library.tabs.row_data(index).as_ref() != Some(&label) {
                self.library.tabs.set_row_data(index, label);
            }
        }
        self.fill_library_filters(ui);
        self.request_library_thumbs(ui);
    }

    /// The filter bar: the type choice, and genre chips for every genre on either list.
    fn fill_library_filters(&mut self, ui: &AppWindow) {
        let filter = &self.library.filter;
        let names = genre_options(
            self.library
                .items
                .iter()
                .filter_map(|item| item.summary.as_ref()?.genres.as_deref()),
            &filter.genres,
        );
        let options: Vec<GenreOption> = names
            .iter()
            .map(|name| GenreOption {
                name: name.as_str().into(),
                selected: filter.genres.iter().any(|chosen| chosen.eq_ignore_ascii_case(name)),
            })
            .collect();
        // In place when the names are the same, so the open genre panel keeps its focus.
        if names == self.library.genre_names {
            for (index, option) in options.into_iter().enumerate() {
                if self.library.genre_options.row_data(index).as_ref() != Some(&option) {
                    self.library.genre_options.set_row_data(index, option);
                }
            }
        } else {
            self.library.genre_options.set_vec(options);
            self.library.genre_names = names;
        }
        let state = ui.global::<LibraryState>();
        state.set_media_index(media_index(filter.media));
        state.set_genre_summary(genre_summary(&filter.genres).into());
        state.set_filtered(filter.is_active());
    }

    pub fn library_media_selected(&mut self, ui: &AppWindow, index: i32) {
        let media = match index {
            0 => Some(MediaType::Movie),
            1 => Some(MediaType::Tv),
            _ => None,
        };
        if media != self.library.filter.media {
            self.library.filter.media = media;
            self.library_filters_changed(ui);
        }
    }

    pub fn library_genre_toggled(&mut self, ui: &AppWindow, index: i32) {
        if let Some(name) = usize::try_from(index).ok().and_then(|i| self.library.genre_names.get(i)).cloned() {
            self.library.filter.toggle_genre(&name);
            self.library_filters_changed(ui);
        }
    }

    /// "Any genre".
    pub fn library_genres_cleared(&mut self, ui: &AppWindow) {
        if !self.library.filter.genres.is_empty() {
            self.library.filter.genres.clear();
            self.library_filters_changed(ui);
        }
    }

    pub fn library_clear_filters(&mut self, ui: &AppWindow) {
        if self.library.filter.is_active() {
            self.library.filter = LibraryFilter::default();
            self.library_filters_changed(ui);
        }
    }

    fn library_filters_changed(&mut self, ui: &AppWindow) {
        let state = ui.global::<LibraryState>();
        state.set_list_serial(state.get_list_serial().wrapping_add(1));
        self.fill_library_rows(ui);
        let count = self.library.shown.len();
        let noun = if count == 1 { "title" } else { "titles" };
        state.set_announcement(format!("{count} {noun} on {}", list_name(self.library.tab)).into());
    }

    pub fn library_tab_selected(&mut self, ui: &AppWindow, index: i32) {
        let tab = usize::try_from(index).ok().and_then(|i| TABS.get(i)).copied().unwrap_or(ExclusionReason::Seen);
        if tab == self.library.tab {
            return;
        }
        self.library.tab = tab;
        let state = ui.global::<LibraryState>();
        if state.get_card_open() || self.library.opening.is_some() {
            self.library_close_card(ui);
        }
        self.clear_library_notice(ui);
        state.set_selected(-1);
        state.set_list_serial(state.get_list_serial().wrapping_add(1));
        self.fill_library_rows(ui);
        let count = self.library.shown.len();
        let noun = if count == 1 { "title" } else { "titles" };
        state.set_announcement(format!("{}: {count} {noun}", list_name(tab)).into());
    }

    fn clear_library_notice(&mut self, ui: &AppWindow) {
        self.library.undo = None;
        let state = ui.global::<LibraryState>();
        state.set_notice(SharedString::new());
        state.set_can_undo(false);
    }

    // ---- Removing, moving and Undo -----------------------------------------------------

    /// The row's Remove button, or Delete: off the list, so it can be picked again.
    pub fn library_remove(&mut self, ui: &AppWindow, index: i32) {
        if let Some(&title) = usize::try_from(index).ok().and_then(|i| self.library.shown.get(i)) {
            self.change_library_title(ui, title, None);
        }
    }

    /// "Remove from …" on the opened title.
    pub fn library_remove_open(&mut self, ui: &AppWindow) {
        if let Some(title) = self.library.open_title {
            self.library_close_card(ui);
            self.change_library_title(ui, title, None);
        }
    }

    /// "Move to …" on the opened title: onto the other list, still out of picks.
    pub fn library_move_open(&mut self, ui: &AppWindow) {
        if let Some(title) = self.library.open_title {
            self.library_close_card(ui);
            self.change_library_title(ui, title, Some(other_list(self.library.tab)));
        }
    }

    fn change_library_title(&mut self, ui: &AppWindow, title: TitleRef, to: Option<ExclusionReason>) {
        let Some(item) = self.library.item(title).cloned() else { return };
        self.clear_library_notice(ui);
        self.library.changing = Some(item);
        self.send(match to {
            Some(reason) => Command::Exclude { title, reason },
            None => Command::Unexclude { title },
        });
    }

    pub fn library_undo(&mut self, ui: &AppWindow) {
        if let Some(item) = self.library.undo.take() {
            self.clear_library_notice(ui);
            ui.global::<LibraryState>().set_announcement(format!("{} is back on the list", item_name(&item)).into());
            self.send(Command::Restore(item));
        }
    }

    /// A title's mark changed: from this page, or from Pick while the page shows nothing.
    pub(super) fn on_library_excluded(&mut self, ui: &AppWindow, title: TitleRef, reason: Option<ExclusionReason>) {
        let before = self.library.changing.take_if(|item| item.title == title);
        if !Self::surface_visible(ui, Surface::Library) {
            return; // the list is read again when the page shows
        }
        let state = ui.global::<LibraryState>();
        match (before, reason) {
            (Some(before), None) => {
                // Removed: its row goes and the selection stays where it was.
                let index = self.library.shown.iter().position(|&shown| shown == title);
                let was_selected = index.is_some_and(|index| state.get_selected() == index as i32);
                self.library.items.retain(|item| item.title != title);
                self.fill_library_rows(ui);
                if let Some(index) = index.filter(|_| was_selected) {
                    let last = self.library.shown.len() as i32 - 1;
                    state.set_selected((index as i32).min(last));
                }
                let name = item_name(&before);
                state.set_notice(format!("“{name}” is off the list and can be picked again.").into());
                state.set_announcement(
                    format!("{name} removed from {}. It can be picked again.", list_name(before.reason)).into(),
                );
                state.set_can_undo(true);
                self.library.undo = Some(before);
            }
            (Some(before), Some(reason)) if reason != before.reason => {
                // Moved: it becomes the newest title on the other list.
                if let Some(position) = self.library.items.iter().position(|item| item.title == title) {
                    let mut item = self.library.items.remove(position);
                    item.reason = reason;
                    item.marked_at = now_unix();
                    self.library.items.insert(0, item);
                }
                self.fill_library_rows(ui);
                let name = item_name(&before);
                state.set_notice(format!("“{name}” moved to {}.", list_name(reason)).into());
                state.set_announcement(format!("{name} moved to {}.", list_name(reason)).into());
                state.set_can_undo(true);
                self.library.undo = Some(before);
            }
            // An Undo, or a change the worker couldn't save: the database has the truth.
            _ => self.reload_library(),
        }
    }

    // ---- The opened title --------------------------------------------------------------

    pub fn library_open(&mut self, ui: &AppWindow, index: i32) {
        let Some(&title) = usize::try_from(index).ok().and_then(|i| self.library.shown.get(i)) else {
            return;
        };
        ui.global::<LibraryState>().set_selected(index);
        self.clear_library_notice(ui);
        let id = self.next_request_id();
        self.library.open_title = Some(title);
        self.library.opening = Some(id);
        Self::later(&self.library.placeholder_timer, &self.weak, PLACEHOLDER_DELAY, move |c, ui| {
            c.show_library_placeholder(ui, id);
        });
        self.send(Command::OpenTitle { id, title });
    }

    fn show_library_placeholder(&mut self, ui: &AppWindow, id: u64) {
        if self.library.opening == Some(id) {
            self.release_card_images(ui, Surface::Library);
            let state = ui.global::<LibraryState>();
            state.set_card_error(SharedString::new());
            state.set_card_loading(true);
            state.set_card_open(true);
        }
    }

    /// The answer to `id` is the Library's title card, not a search result's.
    pub(super) fn library_opening(&self, id: u64) -> bool {
        self.library.opening == Some(id)
    }

    pub(super) fn on_library_title_card(&mut self, ui: &AppWindow, id: u64, card: TitleCard) {
        if self.library.opening != Some(id) {
            return;
        }
        self.library.opening = None;
        self.library.placeholder_timer.stop();
        self.present_card(ui, Surface::Library, id, card);
    }

    /// The opened card is on screen.
    pub(super) fn library_card_revealed(&mut self, ui: &AppWindow) {
        let state = ui.global::<LibraryState>();
        state.set_card_error(SharedString::new());
        state.set_card_loading(false);
        state.set_card_open(true);
        if let Some(card) = self.library.card.card() {
            let year = if card.year.is_empty() { String::new() } else { format!(" ({})", card.year) };
            state.set_announcement(format!("{}{year}. {}.", card.name, card.verdict).into());
        }
    }

    pub(super) fn on_library_title_card_failed(&mut self, ui: &AppWindow, id: u64, error: ApiError) {
        if self.library.opening != Some(id) {
            return;
        }
        self.library.opening = None;
        self.library.placeholder_timer.stop();
        let message = error_message(&error);
        let state = ui.global::<LibraryState>();
        state.set_card_loading(false);
        state.set_card_error(message.as_str().into());
        state.set_card_open(true);
        state.set_announcement(format!("This title didn't load. {message}").into());
    }

    /// "Back to library" and Esc.
    pub fn library_close_card(&mut self, ui: &AppWindow) {
        self.library.opening = None;
        self.library.placeholder_timer.stop();
        // Freed first: a card still waiting for its poster would otherwise open after this.
        self.release_card_images(ui, Surface::Library);
        let state = ui.global::<LibraryState>();
        state.set_card_open(false);
        state.set_card_loading(false);
        state.set_card_error(SharedString::new());
    }

    pub fn library_open_link(&mut self, _ui: &AppWindow) {
        self.open_card_link(Surface::Library);
    }

    pub fn library_card_resized(&mut self, ui: &AppWindow, width: f32, height: f32, poster_width: f32, poster_height: f32) {
        self.card_resized(ui, Surface::Library, width, height, poster_width, poster_height);
    }

    // ---- Thumbnails --------------------------------------------------------------------

    fn library_thumb(&self, ui: &AppWindow, title: TitleRef) -> Image {
        self.library
            .poster_path(title)
            .and_then(|path| self.library.thumbs.get(&thumb_key(&Self::thumb_spec(ui, path))))
            .cloned()
            .unwrap_or_default()
    }

    /// Asks for every thumbnail of the tab that isn't decoded or on its way.
    pub(super) fn request_library_thumbs(&mut self, ui: &AppWindow) {
        if !Self::surface_visible(ui, Surface::Library) {
            return;
        }
        let mut specs = Vec::new();
        for &title in &self.library.shown {
            let Some(path) = self.library.poster_path(title) else { continue };
            let spec = Self::thumb_spec(ui, path);
            let key = thumb_key(&spec);
            if !self.library.thumbs.contains_key(&key) && self.library.pending_thumbs.insert(key) {
                specs.push(spec);
            }
        }
        if !specs.is_empty() {
            self.send(Command::LoadThumbs(specs));
        }
    }

    pub(super) fn on_library_thumb(&mut self, ui: &AppWindow, spec: &ImageSpec, pixels: &SharedPixelBuffer<Rgba8Pixel>) {
        let key = thumb_key(spec);
        if !self.library.pending_thumbs.remove(&key) {
            return; // asked for by Search
        }
        if !Self::surface_visible(ui, Surface::Library) || Self::thumb_spec(ui, &spec.path) != *spec {
            return; // the page hid meanwhile, or the scale changed
        }
        let image = Image::from_rgba8(pixels.clone());
        for (index, &title) in self.library.shown.iter().enumerate() {
            if self.library.poster_path(title) == Some(spec.path.as_str())
                && let Some(mut row) = self.library.rows.row_data(index)
            {
                row.thumb = image.clone();
                self.library.rows.set_row_data(index, row);
            }
        }
        self.library.thumbs.insert(key, image);
    }

    pub(super) fn on_library_thumb_failed(&mut self, spec: &ImageSpec) {
        self.library.pending_thumbs.remove(&thumb_key(spec));
    }

    /// Drops every decoded thumbnail (the page hides, or the scale changed).
    pub(super) fn release_library_thumbs(&mut self) {
        self.library.thumbs.clear();
        self.library.pending_thumbs.clear();
        for index in 0..self.library.rows.row_count() {
            if let Some(mut row) = self.library.rows.row_data(index)
                && row.thumb.size().width > 0
            {
                row.thumb = Image::default();
                self.library.rows.set_row_data(index, row);
            }
        }
    }
}
