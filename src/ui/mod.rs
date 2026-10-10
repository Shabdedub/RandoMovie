//! Starting the window, theming it, and connecting Slint callbacks to the controller.

mod controller;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;
use std::time::{Duration, Instant};

use randomovie::paths::AppPaths;
use randomovie::platform;
use randomovie::store::{Settings, Theme as ThemeSetting, WindowGeometry};
use randomovie::{log_info, log_warn};
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit;

use crate::worker::{self, Command, Sink};
use crate::{
    AppState, AppWindow, OnboardingState, Page, ServicesState, SettingsState, Theme, TokenForm,
};
use controller::Controller;

thread_local! {
    /// The controller, for updates posted from the worker thread.
    static CONTROLLER: RefCell<Option<Rc<RefCell<Controller>>>> = const { RefCell::new(None) };
}

pub fn run(paths: AppPaths, launched: Instant) -> Result<(), Box<dyn Error>> {
    let settings = Settings::load(&paths.settings_file());
    let system_dark = platform::theme::system_prefers_dark();
    let dark = resolve_dark(settings.theme, system_dark);
    let geometry = initial_geometry(&settings);

    // Theme (and maximize) the window as it's created, so it never flashes the wrong
    // title-bar colour or jumps from normal to maximized.
    let initial_theme = settings.theme;
    let maximized = settings.window.is_some_and(|w| w.maximized);
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(move |attributes| {
            attributes
                .with_theme(winit_theme(initial_theme))
                .with_maximized(maximized)
        })
        .select()?;

    let ui = AppWindow::new()?;
    let theme = ui.global::<Theme>();
    theme.set_font_family(platform::fonts::ui_font().into());
    theme.set_icon_font(platform::fonts::icon_font().into());
    apply_colors(&ui, dark);
    if let Some(geometry) = geometry {
        ui.window()
            .set_size(slint::PhysicalSize::new(geometry.width, geometry.height));
        ui.window()
            .set_position(slint::PhysicalPosition::new(geometry.x, geometry.y));
    }

    let worker = worker::spawn(paths.clone(), make_sink(ui.as_weak()))?;
    let normal_geometry = Rc::new(Cell::new(geometry.map(|g| WindowGeometry {
        maximized: false,
        ..g
    })));
    let controller = Rc::new(RefCell::new(Controller::new(
        &ui,
        worker.sender(),
        settings.clone(),
        system_dark,
        normal_geometry.clone(),
    )));
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
    wire_callbacks(&ui, &controller);
    worker.send(Command::Startup(Box::new(settings.clone())));

    let network_sender = worker.sender();
    if !platform::network::watch(move |online| {
        let _ = network_sender.send(Command::NetworkChanged(online));
    }) {
        log_warn!("network change notifications unavailable");
    }

    {
        let controller = controller.clone();
        let weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                controller.borrow_mut().remember_geometry(&ui);
            }
            slint::CloseRequestResponse::HideWindow
        });
    }
    {
        let controller = controller.clone();
        let weak = ui.as_weak();
        let normal = normal_geometry.clone();
        let mut hwnd: Option<isize> = None;
        let mut first_frame_logged = false;
        ui.window().on_winit_window_event(move |window, event| {
            use winit::event::WindowEvent;
            type Action = Box<dyn FnOnce(&mut Controller, &AppWindow)>;
            // Actions run after this event returns: they change the window, which can
            // send further events synchronously.
            let mut actions: Vec<Action> = Vec::new();
            if hwnd.is_none() {
                // Style the title bar as soon as the HWND exists.
                hwnd = hwnd_of(window);
                if hwnd.is_some() {
                    log_info!("window created {} ms after launch", launched.elapsed().as_millis());
                    actions.push(Box::new(|c, ui| c.style_title_bar(ui)));
                }
            }
            match event {
                WindowEvent::RedrawRequested => {
                    if !first_frame_logged {
                        first_frame_logged = true;
                        log_info!("first frame {} ms after launch", launched.elapsed().as_millis());
                    }
                    if hwnd.is_some_and(platform::repaint::system_invalidated) {
                        force_full_repaint(&weak);
                    }
                }
                WindowEvent::Occluded(false) => force_full_repaint(&weak),
                WindowEvent::Resized(_) | WindowEvent::Moved(_) => {
                    if !window.is_maximized() && !window.is_minimized() {
                        let (position, size) = (window.position(), window.size());
                        normal.set(Some(WindowGeometry {
                            x: position.x,
                            y: position.y,
                            width: size.width,
                            height: size.height,
                            maximized: false,
                        }));
                    }
                }
                WindowEvent::ThemeChanged(theme) => {
                    let dark = *theme == winit::window::Theme::Dark;
                    actions.push(Box::new(move |c, ui| c.system_theme_changed(ui, dark)));
                }
                WindowEvent::ScaleFactorChanged { .. } => {
                    actions.push(Box::new(|c, ui| c.scale_changed(ui)));
                }
                _ => {}
            }
            for action in actions {
                let controller = controller.clone();
                let weak = weak.clone();
                slint::Timer::single_shot(Duration::ZERO, move || {
                    if let (Some(ui), Ok(mut controller)) =
                        (weak.upgrade(), controller.try_borrow_mut())
                    {
                        action(&mut controller, &ui);
                    }
                });
            }
            slint::winit_030::EventResult::Propagate
        });
    }

    ui.show()?;
    log_info!("window shown {} ms after launch", launched.elapsed().as_millis());

    slint::run_event_loop()?;

    let final_settings = controller.borrow().settings().clone();
    worker.shutdown(Duration::from_secs(3));
    CONTROLLER.with(|slot| *slot.borrow_mut() = None);
    final_settings.save(&paths.settings_file())?;
    log_info!("RandoMovie closed");
    Ok(())
}

fn make_sink(weak: slint::Weak<AppWindow>) -> Sink {
    Box::new(move |update| {
        let weak = weak.clone();
        let posted = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            CONTROLLER.with(|slot| {
                if let Some(controller) = slot.borrow().as_ref() {
                    controller.borrow_mut().apply(&ui, update);
                }
            });
        });
        // Only fails once the event loop has ended, when updates no longer matter.
        let _ = posted;
    })
}

pub fn resolve_dark(theme: ThemeSetting, system_dark: bool) -> bool {
    match theme {
        ThemeSetting::System => system_dark,
        ThemeSetting::Dark => true,
        ThemeSetting::Light => false,
    }
}

fn winit_theme(theme: ThemeSetting) -> Option<winit::window::Theme> {
    match theme {
        ThemeSetting::System => None,
        ThemeSetting::Dark => Some(winit::window::Theme::Dark),
        ThemeSetting::Light => Some(winit::window::Theme::Light),
    }
}

/// Accent and colour scheme for `dark`; the accent is the Windows one when it's readable.
pub fn apply_colors(ui: &AppWindow, dark: bool) {
    let accent = platform::theme::pick_accent(platform::theme::accent_palette(), dark);
    let theme = ui.global::<Theme>();
    theme.set_accent(color(accent.fill));
    theme.set_on_accent(color(accent.on_fill));
    ui.invoke_apply_color_scheme(dark);
}

/// Switches the theme at run time: colours, winit's window theme and the title bar.
pub fn apply_theme(ui: &AppWindow, theme: ThemeSetting, dark: bool) {
    apply_colors(ui, dark);
    ui.window()
        .with_winit_window(|window| window.set_theme(winit_theme(theme)));
    style_title_bar(ui, dark);
}

fn color(rgb: platform::theme::Rgb) -> slint::Color {
    slint::Color::from_rgb_u8(rgb.r, rgb.g, rgb.b)
}

/// The Win32 window handle, once winit has created the window.
fn hwnd_of(window: &slint::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    window
        .with_winit_window(|window| match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(win32)) => Some(win32.hwnd.get()),
            _ => None,
        })
        .flatten()
}

pub fn style_title_bar(ui: &AppWindow, dark: bool) {
    match hwnd_of(ui.window()) {
        Some(hwnd) => platform::theme::style_title_bar(hwnd, dark),
        None => log_warn!("the title bar couldn't be styled: no window handle yet"),
    }
}

/// Marks the whole window dirty so the next frame repaints all of it (see
/// `platform::repaint` for why the software renderer needs this).
fn force_full_repaint(weak: &slint::Weak<AppWindow>) {
    if let Some(ui) = weak.upgrade() {
        let app = ui.global::<AppState>();
        app.set_repaint_tick(!app.get_repaint_tick());
    }
}

/// Logical size the window opens at when nothing is saved.
const DEFAULT_WIDTH: f32 = 1120.0;
const DEFAULT_HEIGHT: f32 = 760.0;

/// The saved size and position if its title bar is still on a monitor; otherwise the
/// default size, shrunk to fit the primary monitor's work area, centred on it.
fn initial_geometry(settings: &Settings) -> Option<WindowGeometry> {
    if let Some(saved) = settings.window
        && platform::monitor::title_bar_visible(saved.x, saved.y, saved.width)
    {
        return Some(saved);
    }
    let area = platform::monitor::primary_work_area()?;
    // Leave room for the title bar and borders, which aren't part of the inner size.
    let width = ((DEFAULT_WIDTH * area.scale) as u32).min(area.width * 9 / 10);
    let height = ((DEFAULT_HEIGHT * area.scale) as u32).min(area.height * 85 / 100);
    let title_bar = (32.0 * area.scale) as u32;
    Some(WindowGeometry {
        x: area.x + (area.width.saturating_sub(width) / 2) as i32,
        y: area.y + (area.height.saturating_sub(height + title_bar) / 2) as i32,
        width,
        height,
        maximized: false,
    })
}

/// Builds a Slint callback that runs `$body` with the controller and window borrowed.
macro_rules! handler {
    ($controller:expr, $weak:expr, |$c:ident, $ui:ident $(, $arg:ident : $ty:ty)*| $body:expr) => {{
        let controller = $controller.clone();
        let weak = $weak.clone();
        move |$($arg: $ty),*| {
            if let Some($ui) = weak.upgrade() {
                let mut $c = controller.borrow_mut();
                $body;
            }
        }
    }};
}

fn wire_callbacks(ui: &AppWindow, controller: &Rc<RefCell<Controller>>) {
    let weak = ui.as_weak();

    let app = ui.global::<AppState>();
    app.on_navigate(handler!(controller, weak, |c, ui, page: Page| c.navigate(&ui, page)));
    app.on_open_services_sheet(handler!(controller, weak, |c, ui| c.set_sheet_open(&ui, true)));
    app.on_close_services_sheet(handler!(controller, weak, |c, ui| c.set_sheet_open(&ui, false)));
    app.on_open_url(|url| {
        if !platform::shell::open_url(&url) {
            log_warn!("refused to open a non-web link");
        }
    });

    let services = ui.global::<ServicesState>();
    services.on_toggle(handler!(controller, weak, |c, ui, key: slint::SharedString| c
        .toggle_service(&ui, &key)));
    services.on_open_variants(handler!(controller, weak, |c, ui, key: slint::SharedString| c
        .open_variants(&ui, &key)));
    services.on_toggle_variant(handler!(controller, weak, |c, ui, id: i32| c
        .toggle_variant(&ui, id)));
    services.on_filter_edited(handler!(controller, weak, |c, ui, text: slint::SharedString| c
        .filter_services(&ui, &text)));
    services.on_show_all(handler!(controller, weak, |c, ui| c.show_all_services(&ui)));
    services.on_include_free_toggled(handler!(controller, weak, |c, ui, on: bool| c
        .set_include_free(&ui, on)));
    services.on_retry(handler!(controller, weak, |c, ui| c.retry_services(&ui)));

    let token = ui.global::<TokenForm>();
    token.on_submit(handler!(controller, weak, |c, ui, text: slint::SharedString| c
        .submit_token(&ui, &text)));
    token.on_cancel(handler!(controller, weak, |c, ui| c.cancel_token_dialog(&ui)));

    let onboarding = ui.global::<OnboardingState>();
    onboarding.on_next(handler!(controller, weak, |c, ui| c.onboarding_next(&ui)));
    onboarding.on_back(handler!(controller, weak, |c, ui| c.onboarding_back(&ui)));
    onboarding.on_finish(handler!(controller, weak, |c, ui| c.onboarding_finish(&ui)));

    let settings = ui.global::<SettingsState>();
    settings.on_region_selected(handler!(controller, weak, |c, ui, index: i32| c
        .select_region(&ui, index)));
    settings.on_theme_selected(handler!(controller, weak, |c, ui, index: i32| c
        .select_theme(&ui, index)));
    settings.on_media_selected(handler!(controller, weak, |c, ui, index: i32| c
        .select_default_media(&ui, index)));
    settings.on_include_free_toggled(handler!(controller, weak, |c, ui, on: bool| c
        .set_include_free(&ui, on)));
    settings.on_floor_selected(handler!(controller, weak, |c, ui, index: i32| c
        .select_floor(&ui, index)));
    settings.on_clear_cache(handler!(controller, weak, |c, ui| c.clear_cache(&ui)));
    settings.on_refresh_imdb(handler!(controller, weak, |c, ui| c.refresh_imdb(&ui)));
    settings.on_change_token(handler!(controller, weak, |c, ui| c.open_token_dialog(&ui, "")));
    settings.on_remove_token(handler!(controller, weak, |c, ui| c.remove_token(&ui)));
}
