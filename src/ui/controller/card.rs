//! A title card on one page, with its images and logo chips. Pick, Search and the Library each
//! have one; they differ only in which Slint global shows them and what the page does around
//! them.

use std::rc::Rc;
use std::time::Duration;

use randomovie::card::{ImdbBadge, TitleCard};
use randomovie::picker::{ChipGroupKind, Verdict};
use randomovie::platform;
use randomovie::log_warn;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, VecModel};

use super::{Controller, logo_key};
use crate::worker::{CardImageKind, Command, ImageSpec};
use crate::{
    AppState, AppWindow, CardMetrics, LibraryState, Page, PickState, PickStatus, SearchState, Theme,
    TitleCardData, WatchChip, WatchGroup,
};

/// Logo size on the "Where to watch" chips, in logical pixels.
pub(super) const CHIP_LOGO: f32 = 24.0;
/// A new card waits this long for its poster, so text and poster appear together; a
/// poster that takes longer (not yet downloaded) appears when it arrives.
const POSTER_WAIT: Duration = Duration::from_millis(250);
/// The card's images are decoded again for a new size once resizing pauses this long.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);

/// The page a card belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    Pick,
    Search,
    Library,
}

impl Surface {
    pub(super) const ALL: [Surface; 3] = [Surface::Pick, Surface::Search, Surface::Library];

    pub(super) fn page(self) -> Page {
        match self {
            Surface::Pick => Page::Pick,
            Surface::Search => Page::Search,
            Surface::Library => Page::Library,
        }
    }
}

/// The card's size and its poster's, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CardBox {
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) poster_width: f32,
    pub(super) poster_height: f32,
}

/// The images last requested for the card, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImagesRequested {
    id: u64,
    poster: (u32, u32),
    backdrop: (u32, u32),
}

pub(super) struct CardSlot {
    /// The title, with the id of the request that brought it.
    current: Option<(u64, TitleCard)>,
    /// Changes with every new title, so the card resets "More" and its scroll position.
    serial: i32,
    pub(super) card_box: Option<CardBox>,
    images: Option<ImagesRequested>,
    /// `current` arrived but waits for its poster before it replaces the screen.
    awaiting_poster: bool,
    held_poster: Option<Image>,
    held_backdrop: Option<Image>,
    groups: Rc<VecModel<WatchGroup>>,
    chip_models: Vec<Rc<VecModel<WatchChip>>>,
    /// Logo path of each chip, per group.
    chip_logos: Vec<Vec<Option<String>>>,
    poster_timer: Timer,
    resize_timer: Timer,
}

impl CardSlot {
    pub(super) fn new() -> CardSlot {
        CardSlot {
            current: None,
            serial: 0,
            card_box: None,
            images: None,
            awaiting_poster: false,
            held_poster: None,
            held_backdrop: None,
            groups: Rc::new(VecModel::default()),
            chip_models: Vec::new(),
            chip_logos: Vec::new(),
            poster_timer: Timer::default(),
            resize_timer: Timer::default(),
        }
    }

    pub(super) fn groups_model(&self) -> ModelRc<WatchGroup> {
        ModelRc::from(self.groups.clone())
    }

    pub(super) fn card(&self) -> Option<&TitleCard> {
        self.current.as_ref().map(|(_, card)| card)
    }

    pub(super) fn card_mut(&mut self) -> Option<&mut TitleCard> {
        self.current.as_mut().map(|(_, card)| card)
    }

    pub(super) fn id(&self) -> Option<u64> {
        self.current.as_ref().map(|(id, _)| *id)
    }

    /// Logo paths of the chips on the card.
    pub(super) fn chip_logo_paths(&self) -> impl Iterator<Item = &str> {
        self.chip_logos.iter().flatten().filter_map(|path| path.as_deref())
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
pub(super) fn card_data(card: &TitleCard) -> TitleCardData {
    TitleCardData {
        name: card.name.as_str().into(),
        year: card.year.as_str().into(),
        kind: card.kind.into(),
        facts: card.facts.as_str().into(),
        overview: card.overview.as_str().into(),
        imdb: card.imdb.trim_start_matches('★').trim_start().into(),
        imdb_state: imdb_state(card.imdb_badge),
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

pub(super) fn imdb_state(badge: ImdbBadge) -> i32 {
    match badge {
        ImdbBadge::Rated => 0,
        ImdbBadge::Unavailable => 1,
        ImdbBadge::Loading => 2,
    }
}

// ---- The three Slint globals -------------------------------------------------------------

fn set_poster(ui: &AppWindow, surface: Surface, image: Image) {
    match surface {
        Surface::Pick => ui.global::<PickState>().set_poster(image),
        Surface::Search => ui.global::<SearchState>().set_poster(image),
        Surface::Library => ui.global::<LibraryState>().set_poster(image),
    }
}

fn set_backdrop(ui: &AppWindow, surface: Surface, image: Image) {
    match surface {
        Surface::Pick => ui.global::<PickState>().set_backdrop(image),
        Surface::Search => ui.global::<SearchState>().set_backdrop(image),
        Surface::Library => ui.global::<LibraryState>().set_backdrop(image),
    }
}

fn images_missing(ui: &AppWindow, surface: Surface) -> bool {
    let (poster, backdrop) = match surface {
        Surface::Pick => {
            let state = ui.global::<PickState>();
            (state.get_poster(), state.get_backdrop())
        }
        Surface::Search => {
            let state = ui.global::<SearchState>();
            (state.get_poster(), state.get_backdrop())
        }
        Surface::Library => {
            let state = ui.global::<LibraryState>();
            (state.get_poster(), state.get_backdrop())
        }
    };
    poster.size().width == 0 || backdrop.size().width == 0
}

pub(super) fn write_card(ui: &AppWindow, surface: Surface, data: TitleCardData, serial: i32) {
    match surface {
        Surface::Pick => {
            let state = ui.global::<PickState>();
            state.set_card(data);
            state.set_card_serial(serial);
        }
        Surface::Search => {
            let state = ui.global::<SearchState>();
            state.set_card(data);
            state.set_card_serial(serial);
        }
        Surface::Library => {
            let state = ui.global::<LibraryState>();
            state.set_card(data);
            state.set_card_serial(serial);
        }
    }
}

impl Controller {
    pub(super) fn slot(&self, surface: Surface) -> &CardSlot {
        match surface {
            Surface::Pick => &self.pick.card,
            Surface::Search => &self.search.card,
            Surface::Library => &self.library.card,
        }
    }

    pub(super) fn slot_mut(&mut self, surface: Surface) -> &mut CardSlot {
        match surface {
            Surface::Pick => &mut self.pick.card,
            Surface::Search => &mut self.search.card,
            Surface::Library => &mut self.library.card,
        }
    }

    /// The page with this card is on screen.
    pub(super) fn surface_visible(ui: &AppWindow, surface: Surface) -> bool {
        let app = ui.global::<AppState>();
        !app.get_onboarding() && app.get_page() == surface.page()
    }

    /// The page shows the card itself, rather than placeholders, an error or a list.
    fn card_showing(ui: &AppWindow, surface: Surface) -> bool {
        match surface {
            Surface::Pick => ui.global::<PickState>().get_status() == PickStatus::Shown,
            Surface::Search => {
                let state = ui.global::<SearchState>();
                state.get_card_open() && !state.get_card_loading() && state.get_card_error().is_empty()
            }
            Surface::Library => {
                let state = ui.global::<LibraryState>();
                state.get_card_open() && !state.get_card_loading() && state.get_card_error().is_empty()
            }
        }
    }

    /// A new title for the page's card. It replaces the screen once its poster is decoded,
    /// or after `POSTER_WAIT`; the page then finishes the job in `card_revealed`.
    pub(super) fn present_card(&mut self, ui: &AppWindow, surface: Surface, id: u64, card: TitleCard) {
        let visible = Self::surface_visible(ui, surface);
        let has_poster = card.poster_path.is_some();
        let slot = self.slot_mut(surface);
        slot.serial = slot.serial.wrapping_add(1);
        slot.current = Some((id, card));
        slot.images = None;
        slot.held_poster = None;
        slot.held_backdrop = None;
        if has_poster && visible && slot.card_box.is_some() {
            slot.awaiting_poster = true;
            self.request_card_images(ui, surface);
            Self::later(&self.slot(surface).poster_timer, &self.weak, POSTER_WAIT, move |c, ui| {
                c.reveal(ui, surface, id);
            });
        } else {
            self.reveal(ui, surface, id);
        }
    }

    /// Replaces the screen with the card that arrived, with whatever images are ready.
    fn reveal(&mut self, ui: &AppWindow, surface: Surface, id: u64) {
        let slot = self.slot_mut(surface);
        if slot.id() != Some(id) {
            return;
        }
        slot.awaiting_poster = false;
        slot.poster_timer.stop();
        let poster = slot.held_poster.take();
        let backdrop = slot.held_backdrop.take();
        let with_poster = poster.is_some();
        set_poster(ui, surface, poster.unwrap_or_default());
        set_backdrop(ui, surface, backdrop.unwrap_or_default());
        // The page shows the card first, so the image request below sees it on screen.
        self.card_revealed(ui, surface, with_poster);
        self.show_slot(ui, surface);
        self.request_logos(ui);
    }

    fn card_revealed(&mut self, ui: &AppWindow, surface: Surface, with_poster: bool) {
        match surface {
            Surface::Pick => self.pick_card_revealed(ui, with_poster),
            Surface::Search => self.search_card_revealed(ui, with_poster),
            Surface::Library => self.library_card_revealed(ui),
        }
    }

    /// Writes the slot's card to its page and asks for its images.
    pub(super) fn show_slot(&mut self, ui: &AppWindow, surface: Surface) {
        let slot = self.slot(surface);
        let Some(data) = slot.card().map(card_data) else { return };
        write_card(ui, surface, data, slot.serial);
        self.fill_groups(ui, surface);
        self.request_card_images(ui, surface);
    }

    fn fill_groups(&mut self, ui: &AppWindow, surface: Surface) {
        let px = Self::logo_px(ui, CHIP_LOGO);
        let slot = self.slot(surface);
        let Some(card) = slot.card() else { return };
        let mut groups = Vec::new();
        let mut models = Vec::new();
        let mut logos = Vec::new();
        for group in &card.watch.groups {
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
        let slot = self.slot_mut(surface);
        slot.chip_models = models;
        slot.chip_logos = logos;
        slot.groups.set_vec(groups);
    }

    /// Puts a decoded logo into every chip that shows it, on every card.
    pub(super) fn place_chip_logo(&self, path: &str, image: &Image) {
        for surface in Surface::ALL {
            let slot = self.slot(surface);
            for (group, logos) in slot.chip_logos.iter().enumerate() {
                let Some(model) = slot.chip_models.get(group) else { continue };
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
    }

    pub(super) fn open_card_link(&mut self, surface: Surface) {
        let link = self.slot(surface).card().and_then(|card| card.watch.link.as_deref());
        if let Some(link) = link
            && !platform::shell::open_url(link)
        {
            log_warn!("refused to open a non-web link");
        }
    }

    // ---- Images ----------------------------------------------------------------------

    pub(super) fn card_resized(
        &mut self,
        ui: &AppWindow,
        surface: Surface,
        width: f32,
        height: f32,
        poster_width: f32,
        poster_height: f32,
    ) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let card_box = CardBox {
            width,
            height,
            poster_width,
            poster_height,
        };
        let slot = self.slot_mut(surface);
        if slot.card_box == Some(card_box) {
            return;
        }
        let first = slot.card_box.is_none();
        slot.card_box = Some(card_box);
        if surface == Surface::Pick {
            self.send_poster_width(ui);
        }
        if first {
            self.request_card_images(ui, surface);
        } else {
            // While the window is being resized the old images stretch; exact ones follow.
            Self::later(&self.slot(surface).resize_timer, &self.weak, RESIZE_SETTLE, move |c, ui| {
                c.request_card_images(ui, surface);
            });
        }
    }

    /// Asks for the poster and backdrop at the exact physical size the card shows them.
    pub(super) fn request_card_images(&mut self, ui: &AppWindow, surface: Surface) {
        let slot = self.slot(surface);
        let wanted_now = Self::card_showing(ui, surface) || slot.awaiting_poster;
        if !Self::surface_visible(ui, surface) || !wanted_now {
            return;
        }
        let (Some((id, card)), Some(card_box)) = (&slot.current, slot.card_box) else {
            return;
        };
        let scale = ui.window().scale_factor();
        let px = |logical: f32| (logical * scale).round().max(1.0) as u32;
        let wanted = ImagesRequested {
            id: *id,
            poster: (px(card_box.poster_width), px(card_box.poster_height)),
            backdrop: (px(card_box.width), px(card_box.height)),
        };
        if slot.images == Some(wanted) {
            return;
        }
        let poster_radius = ui.global::<CardMetrics>().get_poster_radius() * scale;
        let card_radius = ui.global::<Theme>().get_radius_card() * scale;
        let command = Command::LoadCardImages {
            id: *id,
            poster: card.poster_path.clone().map(|path| ImageSpec {
                path,
                width: wanted.poster.0,
                height: wanted.poster.1,
                radius: poster_radius,
            }),
            backdrop: card.backdrop_path.clone().map(|path| ImageSpec {
                path,
                width: wanted.backdrop.0,
                height: wanted.backdrop.1,
                radius: card_radius,
            }),
        };
        self.slot_mut(surface).images = Some(wanted);
        self.send(command);
    }

    pub(super) fn on_card_image(
        &mut self,
        ui: &AppWindow,
        id: u64,
        kind: CardImageKind,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    ) {
        let surface = Surface::ALL
            .into_iter()
            .find(|&s| self.slot(s).images.is_some_and(|images| images.id == id));
        let Some(surface) = surface else {
            return; // replaced, or released
        };
        if !Self::surface_visible(ui, surface) {
            return;
        }
        let image = Image::from_rgba8(pixels);
        let slot = self.slot_mut(surface);
        if slot.awaiting_poster {
            match kind {
                CardImageKind::Poster => {
                    slot.held_poster = Some(image);
                    self.reveal(ui, surface, id);
                }
                CardImageKind::Backdrop => slot.held_backdrop = Some(image),
            }
            return;
        }
        match kind {
            CardImageKind::Poster => set_poster(ui, surface, image),
            CardImageKind::Backdrop => set_backdrop(ui, surface, image),
        }
    }

    /// Frees the decoded poster and backdrop (the page hides, or the card changes).
    pub(super) fn release_card_images(&mut self, ui: &AppWindow, surface: Surface) {
        let slot = self.slot_mut(surface);
        if slot.awaiting_poster
            && let Some(id) = slot.id()
        {
            // The waiting card shows now, without images; they're requested again later.
            slot.held_poster = None;
            slot.held_backdrop = None;
            self.reveal(ui, surface, id);
        }
        set_poster(ui, surface, Image::default());
        set_backdrop(ui, surface, Image::default());
        self.slot_mut(surface).images = None;
    }

    /// The page came back into view, or the scale or network changed.
    pub(super) fn reload_card_images(&mut self, ui: &AppWindow, surface: Surface) {
        if images_missing(ui, surface) {
            self.slot_mut(surface).images = None;
        }
        self.request_card_images(ui, surface);
    }
}
