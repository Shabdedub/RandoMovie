//! The UI thread's state. It owns the settings, turns user actions into commands for the
//! worker, and applies the worker's updates to the Slint models. It never does I/O itself:
//! settings changes are saved by the worker.

mod card;
mod pick;
mod search;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use randomovie::format;
use randomovie::media::MediaChoice;
use randomovie::services::{
    SelectionState, ServiceCatalog, ServiceGroup, free_titles_hint, visible_groups,
};
use randomovie::store::settings::DEFAULT_OBSCURITY_FLOOR;
use randomovie::store::{Settings, Theme as ThemeSetting, WindowGeometry};
use randomovie::tmdb::model::Region;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use tokio::sync::mpsc::UnboundedSender;

use super::{apply_theme, resolve_dark};
use crate::worker::{Command, ImdbInfo, LogoRequest, TokenSource, TokenStatus, Update};
use crate::{
    AppState, AppWindow, OnboardingState, OnboardingStep, Page, ServiceTile, ServicesState,
    SettingsState, TokenForm, VariantRow,
};
use card::{CHIP_LOGO, Surface};
use pick::PickView;
use search::SearchView;

/// Logo sizes in logical pixels; requests are scaled to physical pixels.
const TILE_LOGO: f32 = 44.0;
const ACTIVE_LOGO: f32 = 28.0;
/// Service logos shown on the Pick page before "+N".
const ACTIVE_ROW_MAX: usize = 10;
/// Choices for the obscurity floor (minimum TMDB votes).
const FLOOR_PRESETS: [u32; 7] = [0, 25, 50, 100, 250, 500, 1000];

pub struct Controller {
    sender: UnboundedSender<Command>,
    /// For timers, which run later without a window reference.
    weak: slint::Weak<AppWindow>,
    settings: Settings,
    system_dark: bool,
    detected_region: String,
    regions: Vec<Region>,
    catalog: Option<ServiceCatalog>,
    catalog_error: String,
    filter: String,
    show_all: bool,
    tiles: Rc<VecModel<ServiceTile>>,
    /// Group key and logo path for each row of `tiles`.
    tile_rows: Vec<(String, Option<String>)>,
    active: Rc<VecModel<ServiceTile>>,
    active_rows: Vec<Option<String>>,
    variants: Rc<VecModel<VariantRow>>,
    variants_group: Option<String>,
    /// Decoded logos by "px:path"; only the sizes on screen are kept.
    logos: HashMap<String, slint::Image>,
    pending_logos: HashSet<String>,
    token_entered: bool,
    imdb: ImdbInfo,
    /// The window's size and position when not maximized, kept current by window events.
    normal_geometry: Rc<Cell<Option<WindowGeometry>>>,
    /// Ids for picks, searches and opened titles, unique across all of them.
    last_request_id: u64,
    pick: PickView,
    search: SearchView,
}

fn logo_key(path: &str, px: u32) -> String {
    format!("{px}:{path}")
}

fn tile_state(state: SelectionState) -> i32 {
    match state {
        SelectionState::Off => 0,
        SelectionState::Partial => 1,
        SelectionState::On => 2,
    }
}

impl Controller {
    pub fn new(
        ui: &AppWindow,
        sender: UnboundedSender<Command>,
        settings: Settings,
        system_dark: bool,
        normal_geometry: Rc<Cell<Option<WindowGeometry>>>,
    ) -> Controller {
        let media = settings.default_media;
        let mut controller = Controller {
            sender,
            weak: ui.as_weak(),
            settings,
            system_dark,
            detected_region: String::new(),
            regions: Vec::new(),
            catalog: None,
            catalog_error: String::new(),
            filter: String::new(),
            show_all: false,
            tiles: Rc::new(VecModel::default()),
            tile_rows: Vec::new(),
            active: Rc::new(VecModel::default()),
            active_rows: Vec::new(),
            variants: Rc::new(VecModel::default()),
            variants_group: None,
            logos: HashMap::new(),
            pending_logos: HashSet::new(),
            token_entered: false,
            imdb: ImdbInfo::default(),
            normal_geometry,
            last_request_id: 0,
            pick: PickView::new(media),
            search: SearchView::new(),
        };
        controller.initialise(ui);
        controller
    }

    fn next_request_id(&mut self) -> u64 {
        self.last_request_id += 1;
        self.last_request_id
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

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    fn initialise(&mut self, ui: &AppWindow) {
        let app = ui.global::<AppState>();
        app.set_onboarding(!self.settings.setup_complete);
        app.set_page(Page::Pick);
        app.set_imdb_status(format::IMDB_LOADING.into());

        // Until the worker reports whether a token exists, the first step waits.
        let onboarding = ui.global::<OnboardingState>();
        onboarding.set_step(OnboardingStep::Token);
        onboarding.set_token_step_shown(true);
        ui.global::<TokenForm>().set_checking(true);

        let services = ui.global::<ServicesState>();
        services.set_tiles(ModelRc::from(self.tiles.clone()));
        services.set_active(ModelRc::from(self.active.clone()));
        services.set_variants(ModelRc::from(self.variants.clone()));
        services.set_include_free(self.settings.include_free);
        services.set_loading(true);

        let settings = ui.global::<SettingsState>();
        settings.set_version(env!("CARGO_PKG_VERSION").into());
        settings.set_theme_index(match self.settings.theme {
            ThemeSetting::System => 0,
            ThemeSetting::Dark => 1,
            ThemeSetting::Light => 2,
        });
        settings.set_media_index(match self.settings.default_media {
            MediaChoice::Movies => 0,
            MediaChoice::Series => 1,
            MediaChoice::Both => 2,
        });
        settings.set_include_free(self.settings.include_free);
        settings.set_token_source("Looking for a saved token".into());
        settings.set_cache_size("Measuring".into());
        self.refresh_floor_ui(ui);
        self.refresh_region_ui(ui);
        self.refresh_imdb_ui(ui);
        self.init_pick(ui);
        self.init_search(ui);
    }

    fn send(&self, command: Command) {
        let _ = self.sender.send(command);
    }

    fn save_settings(&self) {
        self.send(Command::SaveSettings(Box::new(self.settings.clone())));
    }

    fn effective_region(&self) -> String {
        self.settings
            .region
            .clone()
            .unwrap_or_else(|| self.detected_region.clone())
    }

    fn region_name(&self, code: &str) -> String {
        self.regions
            .iter()
            .find(|r| r.code == code)
            .map_or_else(|| code.to_owned(), |r| r.english_name.clone())
    }

    fn logo_px(ui: &AppWindow, logical: f32) -> u32 {
        (logical * ui.window().scale_factor()).round().max(1.0) as u32
    }

    // ---- Worker updates ----------------------------------------------------------------

    pub fn apply(&mut self, ui: &AppWindow, update: Update) {
        match update {
            Update::DetectedRegion(code) => {
                self.detected_region = code;
                self.refresh_region_ui(ui);
                self.refresh_services(ui);
            }
            Update::Token(status) => self.on_token(ui, status),
            Update::Unauthorized => self.open_token_dialog(
                ui,
                "TMDB didn't accept the saved token. Enter a new one to keep going.",
            ),
            Update::Regions(regions) => {
                self.regions = regions;
                self.refresh_region_ui(ui);
            }
            Update::CatalogLoading => {
                self.catalog_error.clear();
                ui.global::<ServicesState>().set_error(SharedString::new());
                ui.global::<ServicesState>()
                    .set_loading(self.catalog.is_none());
            }
            Update::Catalog(catalog) => {
                if catalog.region != self.effective_region() {
                    return; // the region changed while this was loading
                }
                self.catalog = Some(*catalog);
                self.catalog_error.clear();
                self.refresh_services(ui);
            }
            Update::CatalogFailed(message) => {
                self.catalog_error = format!("Couldn't load services. {message}");
                self.refresh_services(ui);
            }
            Update::Logo { path, px, pixels } => {
                let key = logo_key(&path, px);
                self.pending_logos.remove(&key);
                self.logos.insert(key, slint::Image::from_rgba8(pixels));
                self.place_logo(ui, &path, px);
            }
            Update::LogoFailed { path, px } => {
                self.pending_logos.remove(&logo_key(&path, px));
            }
            Update::Online(online) => {
                ui.global::<AppState>().set_offline(!online);
                if online {
                    self.request_logos(ui);
                    self.reload_card_images(ui, Surface::Pick);
                    self.reload_card_images(ui, Surface::Search);
                    self.request_thumbs(ui);
                    self.retry_failed_search(ui);
                }
            }
            Update::Imdb(info) => {
                let became_ready = info.ready && !self.imdb.ready;
                self.imdb = info;
                self.refresh_imdb_ui(ui);
                if became_ready {
                    // Search rows that said "Loading IMDb ratings…" can show them now.
                    self.search_availability_changed(ui);
                }
            }
            Update::CacheSize(bytes) => {
                let settings = ui.global::<SettingsState>();
                settings.set_cache_size(format!("{} of cached data and images", format::bytes(bytes)).into());
                settings.set_clearing_cache(false);
            }
            Update::Genres(names) => self.on_genres(ui, names),
            Update::Card {
                id,
                card,
                can_go_back,
                excluded,
            } => self.on_card(ui, id, *card, can_go_back, excluded),
            Update::PickFailed { id, error } => self.on_pick_failed(ui, id, error),
            Update::CardImage { id, kind, pixels } => self.on_card_image(ui, id, kind, pixels),
            Update::CardImdb { id, text, badge } => self.on_card_imdb(ui, id, text, badge),
            Update::Excluded { title, reason } => self.on_excluded(ui, title, reason),
            Update::SearchResults {
                id,
                page,
                total_pages,
                hits,
            } => self.on_search_results(ui, id, page, total_pages, hits),
            Update::SearchFailed { id, page, error } => self.on_search_failed(ui, id, page, error),
            Update::SearchHitInfo { id, title, info } => self.on_search_hit_info(ui, id, title, info),
            Update::Thumb { spec, pixels } => self.on_thumb(ui, spec, pixels),
            Update::ThumbFailed(spec) => self.on_thumb_failed(spec),
            Update::TitleCard { id, card } => self.on_title_card(ui, id, *card),
            Update::TitleCardFailed { id, error } => self.on_title_card_failed(ui, id, error),
        }
    }

    fn on_token(&mut self, ui: &AppWindow, status: TokenStatus) {
        let form = ui.global::<TokenForm>();
        let settings = ui.global::<SettingsState>();
        match status {
            TokenStatus::Checking => {
                form.set_checking(true);
                form.set_error(SharedString::new());
            }
            TokenStatus::Rejected(message) => {
                form.set_checking(false);
                form.set_error(message.into());
            }
            TokenStatus::Missing => {
                form.set_checking(false);
                settings.set_token_source("No token saved yet.".into());
                settings.set_token_removable(false);
                if ui.global::<AppState>().get_onboarding() {
                    let onboarding = ui.global::<OnboardingState>();
                    onboarding.set_step(OnboardingStep::Token);
                    onboarding.set_token_step_shown(true);
                } else {
                    self.open_token_dialog(ui, "Add your TMDB token to load services and picks.");
                }
                ui.global::<ServicesState>().set_loading(false);
            }
            TokenStatus::Ready(source) => {
                form.set_checking(false);
                form.set_error(SharedString::new());
                form.set_notice(SharedString::new());
                form.set_dialog_open(false);
                settings.set_token_source(
                    match source {
                        TokenSource::CredentialManager => {
                            "Saved in Windows Credential Manager."
                        }
                        TokenSource::Environment => "From the TMDB_TOKEN environment variable.",
                    }
                    .into(),
                );
                settings.set_token_removable(source == TokenSource::CredentialManager);
                let onboarding = ui.global::<OnboardingState>();
                if ui.global::<AppState>().get_onboarding()
                    && onboarding.get_step() == OnboardingStep::Token
                {
                    // A token found at startup needs no first-run step.
                    onboarding.set_token_step_shown(self.token_entered);
                    onboarding.set_step(OnboardingStep::Region);
                }
            }
        }
    }

    // ---- Navigation --------------------------------------------------------------------

    pub fn navigate(&mut self, ui: &AppWindow, page: Page) {
        let app = ui.global::<AppState>();
        let previous = app.get_page();
        app.set_services_sheet_open(false);
        app.set_page(page);
        // Decoded posters, backdrops and thumbnails are kept only while their page shows.
        if previous != page {
            match previous {
                Page::Pick => self.release_card_images(ui, Surface::Pick),
                Page::Search => {
                    self.release_card_images(ui, Surface::Search);
                    self.release_thumbs();
                }
                _ => {}
            }
            match page {
                Page::Pick => self.reload_card_images(ui, Surface::Pick),
                Page::Search => {
                    self.refresh_stale_search(ui);
                    self.reload_card_images(ui, Surface::Search);
                    self.request_thumbs(ui);
                }
                _ => {}
            }
        }
        self.refresh_services(ui);
    }

    /// The Pick card reports its size and its poster's.
    pub fn pick_card_resized(&mut self, ui: &AppWindow, width: f32, height: f32, poster_width: f32, poster_height: f32) {
        self.card_resized(ui, Surface::Pick, width, height, poster_width, poster_height);
    }

    pub fn set_sheet_open(&mut self, ui: &AppWindow, open: bool) {
        ui.global::<AppState>().set_services_sheet_open(open);
        self.refresh_services(ui);
    }

    /// Whether any screen showing the service tiles is visible.
    fn tiles_visible(ui: &AppWindow) -> bool {
        let app = ui.global::<AppState>();
        if app.get_onboarding() {
            return ui.global::<OnboardingState>().get_step() == OnboardingStep::Services;
        }
        app.get_page() == Page::Services || app.get_services_sheet_open()
    }

    // ---- Services ----------------------------------------------------------------------

    /// Rebuilds everything the services panel and the Pick page row show.
    fn refresh_services(&mut self, ui: &AppWindow) {
        let state = ui.global::<ServicesState>();
        state.set_loading(self.catalog.is_none() && self.catalog_error.is_empty());
        state.set_error(self.catalog_error.as_str().into());
        state.set_include_free(self.settings.include_free);
        state.set_free_hint(free_titles_hint(&self.effective_region()).into());
        let empty = match &self.catalog {
            Some(catalog) if catalog.groups.is_empty() => format!(
                "TMDB doesn't list any streaming services for {}. Choose another region.",
                self.region_name(&catalog.region)
            ),
            _ => String::new(),
        };
        state.set_empty_message(empty.into());
        self.refresh_selected_count(ui);
        self.rebuild_active(ui);
        if Self::tiles_visible(ui) {
            self.rebuild_tiles(ui);
        } else {
            self.release_tiles(ui);
        }
        self.request_logos(ui);
        self.refresh_idle_message(ui);
    }

    fn tile(&self, group: &ServiceGroup, px: u32) -> ServiceTile {
        let logo = group
            .logo_path
            .as_deref()
            .and_then(|path| self.logos.get(&logo_key(path, px)))
            .cloned()
            .unwrap_or_default();
        ServiceTile {
            key: group.key.as_str().into(),
            name: group.name.as_str().into(),
            logo,
            state: tile_state(self.settings.services.state(group)),
            variant_count: group.variants.len() as i32,
        }
    }

    fn rebuild_tiles(&mut self, ui: &AppWindow) {
        let px = Self::logo_px(ui, TILE_LOGO);
        let (tiles, rows, hidden) = match &self.catalog {
            Some(catalog) => {
                let (groups, hidden) =
                    visible_groups(catalog, &self.settings.services, &self.filter, self.show_all);
                let tiles: Vec<ServiceTile> = groups.iter().map(|g| self.tile(g, px)).collect();
                let rows = groups
                    .iter()
                    .map(|g| (g.key.clone(), g.logo_path.clone()))
                    .collect();
                (tiles, rows, hidden)
            }
            None => (Vec::new(), Vec::new(), 0),
        };
        self.tile_rows = rows;
        self.tiles.set_vec(tiles);
        ui.global::<ServicesState>().set_hidden_count(hidden as i32);
    }

    /// Drops the tiles and their decoded logos while no screen shows them.
    fn release_tiles(&mut self, ui: &AppWindow) {
        if !self.tile_rows.is_empty() {
            self.tiles.set_vec(Vec::new());
            self.tile_rows.clear();
        }
        let px = Self::logo_px(ui, TILE_LOGO);
        let prefix = format!("{px}:");
        self.logos.retain(|key, _| !key.starts_with(&prefix));
        ui.global::<ServicesState>().set_hidden_count(0);
    }

    fn rebuild_active(&mut self, ui: &AppWindow) {
        let px = Self::logo_px(ui, ACTIVE_LOGO);
        let selected: Vec<&ServiceGroup> = match &self.catalog {
            Some(catalog) => catalog
                .groups
                .iter()
                .filter(|g| self.settings.services.state(g) != SelectionState::Off)
                .collect(),
            None => Vec::new(),
        };
        let more = selected.len().saturating_sub(ACTIVE_ROW_MAX);
        let shown = &selected[..selected.len().min(ACTIVE_ROW_MAX)];
        let tiles: Vec<ServiceTile> = shown.iter().map(|g| self.tile(g, px)).collect();
        self.active_rows = shown.iter().map(|g| g.logo_path.clone()).collect();
        self.active.set_vec(tiles);
        ui.global::<ServicesState>().set_active_more(more as i32);
    }

    fn refresh_selected_count(&self, ui: &AppWindow) {
        let count = self.catalog.as_ref().map_or(0, |catalog| {
            catalog
                .groups
                .iter()
                .filter(|g| self.settings.services.state(g) != SelectionState::Off)
                .count()
        });
        ui.global::<ServicesState>().set_selected_count(count as i32);
    }

    /// Asks the worker for every logo on screen that isn't decoded or on its way.
    fn request_logos(&mut self, ui: &AppWindow) {
        let tile_px = Self::logo_px(ui, TILE_LOGO);
        let active_px = Self::logo_px(ui, ACTIVE_LOGO);
        let chip_px = Self::logo_px(ui, CHIP_LOGO);
        let mut wanted: Vec<(String, u32)> = self
            .tile_rows
            .iter()
            .filter_map(|(_, path)| path.as_ref().map(|p| (p.clone(), tile_px)))
            .chain(
                self.active_rows
                    .iter()
                    .filter_map(|path| path.as_ref().map(|p| (p.clone(), active_px))),
            )
            .collect();
        // Chips on the card of the page that shows.
        let card = match ui.global::<AppState>().get_page() {
            Page::Pick => Some(Surface::Pick),
            Page::Search => Some(Surface::Search),
            _ => None,
        };
        if let Some(surface) = card {
            wanted.extend(self.slot(surface).chip_logo_paths().map(|p| (p.to_owned(), chip_px)));
        }
        let mut requests = Vec::new();
        for (path, px) in wanted {
            let key = logo_key(&path, px);
            if !self.logos.contains_key(&key) && self.pending_logos.insert(key) {
                requests.push(LogoRequest { path, px });
            }
        }
        if !requests.is_empty() {
            self.send(Command::LoadLogos(requests));
        }
    }

    /// Puts a freshly decoded logo into every row that shows it.
    fn place_logo(&mut self, ui: &AppWindow, path: &str, px: u32) {
        let Some(image) = self.logos.get(&logo_key(path, px)).cloned() else {
            return;
        };
        if px == Self::logo_px(ui, TILE_LOGO) {
            for (row, (_, logo)) in self.tile_rows.iter().enumerate() {
                if logo.as_deref() == Some(path)
                    && let Some(mut tile) = self.tiles.row_data(row)
                {
                    tile.logo = image.clone();
                    self.tiles.set_row_data(row, tile);
                }
            }
        }
        if px == Self::logo_px(ui, ACTIVE_LOGO) {
            for (row, logo) in self.active_rows.iter().enumerate() {
                if logo.as_deref() == Some(path)
                    && let Some(mut tile) = self.active.row_data(row)
                {
                    tile.logo = image.clone();
                    self.active.set_row_data(row, tile);
                }
            }
        }
        if px == Self::logo_px(ui, CHIP_LOGO) {
            self.place_chip_logo(path, &image);
        }
    }

    /// Updates the tile rows for one service after a selection change, in place.
    fn refresh_tile_rows(&mut self, ui: &AppWindow, key: &str) {
        let Some(group) = self.catalog.as_ref().and_then(|c| c.group(key)) else {
            return;
        };
        let state = tile_state(self.settings.services.state(group));
        for (row, (row_key, _)) in self.tile_rows.iter().enumerate() {
            if row_key == key
                && let Some(mut tile) = self.tiles.row_data(row)
            {
                tile.state = state;
                self.tiles.set_row_data(row, tile);
            }
        }
        self.refresh_selected_count(ui);
        self.rebuild_active(ui);
        self.request_logos(ui);
    }

    pub fn toggle_service(&mut self, ui: &AppWindow, key: &str) {
        let Some(group) = self.catalog.as_ref().and_then(|c| c.group(key)).cloned() else {
            return;
        };
        let turn_on = self.settings.services.state(&group) != SelectionState::On;
        self.settings.services.set_group(&group, turn_on);
        self.save_settings();
        self.refresh_tile_rows(ui, key);
        if self.variants_group.as_deref() == Some(key) {
            self.fill_variants(&group);
        }
        self.pick_inputs_changed(ui);
        self.search_availability_changed(ui);
    }

    pub fn open_variants(&mut self, ui: &AppWindow, key: &str) {
        let Some(group) = self.catalog.as_ref().and_then(|c| c.group(key)).cloned() else {
            return;
        };
        ui.global::<ServicesState>()
            .set_variants_title(format!("{} plans and channels", group.name).into());
        self.variants_group = Some(group.key.clone());
        self.fill_variants(&group);
    }

    fn fill_variants(&self, group: &ServiceGroup) {
        let rows: Vec<VariantRow> = group
            .variants
            .iter()
            .map(|v| VariantRow {
                id: v.id as i32,
                name: v.name.as_str().into(),
                selected: self.settings.services.variant_selected(group, v.id),
            })
            .collect();
        self.variants.set_vec(rows);
    }

    pub fn toggle_variant(&mut self, ui: &AppWindow, id: i32) {
        let Some(key) = self.variants_group.clone() else {
            return;
        };
        let Some(group) = self.catalog.as_ref().and_then(|c| c.group(&key)).cloned() else {
            return;
        };
        let id = id as u32;
        let turn_on = !self.settings.services.variant_selected(&group, id);
        self.settings.services.set_variant(&group, id, turn_on);
        self.save_settings();
        self.fill_variants(&group);
        self.refresh_tile_rows(ui, &key);
        self.pick_inputs_changed(ui);
        self.search_availability_changed(ui);
    }

    pub fn filter_services(&mut self, ui: &AppWindow, text: &str) {
        self.filter = text.to_owned();
        self.rebuild_tiles(ui);
        self.request_logos(ui);
    }

    pub fn show_all_services(&mut self, ui: &AppWindow) {
        self.show_all = true;
        self.rebuild_tiles(ui);
        self.request_logos(ui);
    }

    pub fn set_include_free(&mut self, ui: &AppWindow, on: bool) {
        self.settings.include_free = on;
        self.save_settings();
        ui.global::<ServicesState>().set_include_free(on);
        ui.global::<SettingsState>().set_include_free(on);
        self.pick_inputs_changed(ui);
    }

    pub fn retry_services(&mut self, ui: &AppWindow) {
        self.catalog_error.clear();
        self.refresh_services(ui);
        self.send(Command::LoadCatalog);
        if self.regions.is_empty() {
            self.send(Command::LoadRegions);
        }
    }

    // ---- Token -------------------------------------------------------------------------

    pub fn submit_token(&mut self, ui: &AppWindow, token: &str) {
        let form = ui.global::<TokenForm>();
        if form.get_checking() {
            return;
        }
        self.token_entered = true;
        form.set_checking(true);
        form.set_error(SharedString::new());
        self.send(Command::SubmitToken(token.to_owned()));
    }

    pub fn open_token_dialog(&mut self, ui: &AppWindow, notice: &str) {
        let form = ui.global::<TokenForm>();
        form.set_notice(notice.into());
        form.set_error(SharedString::new());
        form.set_checking(false);
        if ui.global::<AppState>().get_onboarding() {
            let onboarding = ui.global::<OnboardingState>();
            onboarding.set_token_step_shown(true);
            onboarding.set_step(OnboardingStep::Token);
        } else {
            form.set_dialog_open(true);
        }
    }

    pub fn cancel_token_dialog(&mut self, ui: &AppWindow) {
        let form = ui.global::<TokenForm>();
        if form.get_checking() {
            return;
        }
        form.set_dialog_open(false);
        form.set_notice(SharedString::new());
        form.set_error(SharedString::new());
    }

    pub fn remove_token(&mut self, ui: &AppWindow) {
        ui.global::<SettingsState>()
            .set_token_source("Removing the saved token".into());
        self.send(Command::RemoveToken);
    }

    // ---- Onboarding --------------------------------------------------------------------

    pub fn onboarding_next(&mut self, ui: &AppWindow) {
        let onboarding = ui.global::<OnboardingState>();
        if onboarding.get_step() == OnboardingStep::Region {
            onboarding.set_step(OnboardingStep::Services);
            self.refresh_services(ui);
        }
    }

    pub fn onboarding_back(&mut self, ui: &AppWindow) {
        let onboarding = ui.global::<OnboardingState>();
        match onboarding.get_step() {
            OnboardingStep::Services => onboarding.set_step(OnboardingStep::Region),
            OnboardingStep::Region if onboarding.get_token_step_shown() => {
                ui.global::<TokenForm>().set_error(SharedString::new());
                onboarding.set_step(OnboardingStep::Token);
            }
            _ => {}
        }
        self.refresh_services(ui);
    }

    pub fn onboarding_finish(&mut self, ui: &AppWindow) {
        self.settings.setup_complete = true;
        self.save_settings();
        let app = ui.global::<AppState>();
        app.set_onboarding(false);
        app.set_page(Page::Pick);
        self.refresh_services(ui);
        // Setup is done, so the first pick can be prepared.
        self.schedule_prefetch();
    }

    // ---- Settings ----------------------------------------------------------------------

    fn refresh_region_ui(&self, ui: &AppWindow) {
        let detected = self.region_name(&self.detected_region);
        let mut names: Vec<SharedString> = Vec::with_capacity(self.regions.len() + 1);
        names.push(if detected.is_empty() {
            "Automatic".into()
        } else {
            format!("Automatic ({detected})").into()
        });
        names.extend(self.regions.iter().map(|r| SharedString::from(r.english_name.as_str())));
        let index = match &self.settings.region {
            None => 0,
            Some(code) => self
                .regions
                .iter()
                .position(|r| &r.code == code)
                .map_or(0, |i| i + 1),
        };
        let settings = ui.global::<SettingsState>();
        settings.set_region_names(ModelRc::new(VecModel::from(names)));
        settings.set_region_index(index as i32);
        settings.set_detected_region(detected.into());
        let current = self.effective_region();
        let current_name = if current.is_empty() { String::new() } else { self.region_name(&current) };
        ui.global::<AppState>().set_region_name(current_name.into());
    }

    pub fn select_region(&mut self, ui: &AppWindow, index: i32) {
        let region = match index {
            i if i <= 0 => None,
            i => self.regions.get(i as usize - 1).map(|r| r.code.clone()),
        };
        if region == self.settings.region {
            return;
        }
        let old_region = self.effective_region();
        self.settings.region = region;
        self.save_settings();
        self.refresh_region_ui(ui);
        if self.effective_region() != old_region {
            self.catalog = None;
            self.catalog_error.clear();
            self.refresh_services(ui);
            self.send(Command::LoadCatalog);
            self.pick_inputs_changed(ui);
            self.search_availability_changed(ui);
        }
    }

    pub fn select_theme(&mut self, ui: &AppWindow, index: i32) {
        let theme = match index {
            0 => ThemeSetting::System,
            2 => ThemeSetting::Light,
            _ => ThemeSetting::Dark,
        };
        if theme == self.settings.theme {
            return;
        }
        self.settings.theme = theme;
        self.save_settings();
        apply_theme(ui, theme, resolve_dark(theme, self.system_dark));
    }

    /// Windows switched between light and dark; matters only in System mode.
    pub fn system_theme_changed(&mut self, ui: &AppWindow, dark: bool) {
        if self.system_dark == dark {
            return;
        }
        self.system_dark = dark;
        if self.settings.theme == ThemeSetting::System {
            apply_theme(ui, ThemeSetting::System, dark);
        }
    }

    /// The window moved to a monitor with another scale: logos, thumbnails and the cards'
    /// images are decoded again to stay sharp.
    pub fn scale_changed(&mut self, ui: &AppWindow) {
        self.logos.clear();
        self.pending_logos.clear();
        self.refresh_services(ui);
        for surface in [Surface::Pick, Surface::Search] {
            self.release_card_images(ui, surface);
            self.reload_card_images(ui, surface);
        }
        self.release_thumbs();
        self.request_thumbs(ui);
        self.send_poster_width(ui);
    }

    /// The default type also becomes the filter bar's current choice.
    pub fn select_default_media(&mut self, ui: &AppWindow, index: i32) {
        let media = pick::media_choice(index);
        self.settings.default_media = media;
        self.save_settings();
        if self.pick.media() != media {
            self.pick.set_media(media);
            self.pick_inputs_changed(ui);
        }
        self.refresh_filter_bar(ui, false);
    }

    fn floor_values(&self) -> Vec<u32> {
        let mut values = FLOOR_PRESETS.to_vec();
        if !values.contains(&self.settings.obscurity_floor) {
            values.push(self.settings.obscurity_floor);
        }
        values
    }

    fn refresh_floor_ui(&self, ui: &AppWindow) {
        let values = self.floor_values();
        let labels: Vec<SharedString> = values
            .iter()
            .map(|&v| match v {
                0 => SharedString::from("Any"),
                v if v == DEFAULT_OBSCURITY_FLOOR => format!("{v} votes (default)").into(),
                v => format!("{v} votes").into(),
            })
            .collect();
        let index = values
            .iter()
            .position(|&v| v == self.settings.obscurity_floor)
            .unwrap_or(0);
        let settings = ui.global::<SettingsState>();
        settings.set_floor_options(ModelRc::new(VecModel::from(labels)));
        settings.set_floor_index(index as i32);
    }

    pub fn select_floor(&mut self, ui: &AppWindow, index: i32) {
        if let Some(&value) = self.floor_values().get(index.max(0) as usize) {
            self.settings.obscurity_floor = value;
            self.save_settings();
            self.pick_inputs_changed(ui);
        }
    }

    pub fn clear_cache(&mut self, ui: &AppWindow) {
        ui.global::<SettingsState>().set_clearing_cache(true);
        self.send(Command::ClearCache);
    }

    pub fn refresh_imdb(&mut self, ui: &AppWindow) {
        let settings = ui.global::<SettingsState>();
        settings.set_imdb_refreshing(true);
        settings.set_imdb_message(SharedString::new());
        self.send(Command::RefreshImdb);
    }

    fn refresh_imdb_ui(&self, ui: &AppWindow) {
        let info = &self.imdb;
        let date = info.data_date.map(format::day).unwrap_or_default();
        let summary = if info.ready {
            format!("Data from {date}, {} titles", format::votes_compact(info.records as u32))
        } else if info.refreshing {
            "Downloading IMDb ratings".to_owned()
        } else {
            "Not downloaded yet".to_owned()
        };
        let settings = ui.global::<SettingsState>();
        settings.set_imdb_summary(summary.into());
        settings.set_imdb_message(info.message.clone().unwrap_or_default().into());
        settings.set_imdb_refreshing(info.refreshing);
        // Shown on the Pick page only while the ratings download for the first time.
        let status = if info.ready { "" } else { format::IMDB_LOADING };
        ui.global::<AppState>().set_imdb_status(status.into());
    }

    // ---- Window ------------------------------------------------------------------------

    /// Dark or light title bar to match the current theme, once the window exists.
    pub fn style_title_bar(&mut self, ui: &AppWindow) {
        super::style_title_bar(ui, resolve_dark(self.settings.theme, self.system_dark));
    }

    /// Records the window's size and position when it closes. A maximized window keeps its
    /// normal size and position; a minimized one keeps the previous record.
    pub fn remember_geometry(&mut self, ui: &AppWindow) {
        let window = ui.window();
        if window.is_minimized() {
            return;
        }
        let geometry = if window.is_maximized() {
            match self.normal_geometry.get() {
                Some(normal) => WindowGeometry {
                    maximized: true,
                    ..normal
                },
                None => return,
            }
        } else {
            let position = window.position();
            let size = window.size();
            WindowGeometry {
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
                maximized: false,
            }
        };
        self.settings.window = Some(geometry);
        self.settings.sanitize();
    }
}
