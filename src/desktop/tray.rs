//! System tray icon (StatusNotifierItem through libayatana-appindicator).

use std::ffi::CStr;

use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tracing::warn;

use super::{Desktop, commands};

const ID_SHOW: &str = "show";
const ID_TOGGLE_SERVER: &str = "toggle_server";
const ID_QUIT: &str = "quit";

/// Libraries tray-icon loads at run time, in its order; one is enough.
#[cfg(target_os = "linux")]
const INDICATOR_LIBS: [&CStr; 4] = [
    c"libayatana-appindicator3.so.1",
    c"libappindicator3.so.1",
    c"libayatana-appindicator3.so",
    c"libappindicator3.so",
];

pub(super) struct Tray {
    icon: TrayIcon<Wry>,
    toggle_server: MenuItem<Wry>,
}

/// Whether an appindicator library can be loaded. tray-icon panics when none can.
#[cfg(target_os = "linux")]
fn indicator_available() -> bool {
    INDICATOR_LIBS.iter().any(|name| {
        // SAFETY: `name` is a valid C string; the handle is only checked for null. The library
        // stays loaded, which is what tray-icon wants next anyway.
        !unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY) }.is_null()
    })
}

#[cfg(not(target_os = "linux"))]
fn indicator_available() -> bool {
    true
}

/// Creates the tray icon and stores it in the app state. Logs a warning and leaves the app
/// without a tray when that isn't possible.
pub(super) fn create(app: &AppHandle) {
    if !indicator_available() {
        warn!("No tray icon: libayatana-appindicator3 (or libappindicator3) is not installed");
        return;
    }
    match build(app) {
        Ok(tray) => {
            let _ = app.state::<Desktop>().tray.set(tray);
        }
        Err(err) => warn!("Failed to create the tray icon: {err}"),
    }
}

fn build(app: &AppHandle) -> tauri::Result<Tray> {
    let show = MenuItem::with_id(app, ID_SHOW, "Show Weylus", true, None::<&str>)?;
    let toggle_server =
        MenuItem::with_id(app, ID_TOGGLE_SERVER, "Start server", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &toggle_server, &separator, &quit])?;

    let icon = TrayIconBuilder::with_id("weylus")
        .icon(Image::from_bytes(include_bytes!("../../icons/icon.png"))?)
        .tooltip("Weylus - stopped")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(Tray {
        icon,
        toggle_server,
    })
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        ID_SHOW => show_window(app),
        ID_TOGGLE_SERVER => {
            let app = app.clone();
            // Weylus::start and stop block on the web thread.
            tauri::async_runtime::spawn_blocking(move || {
                let running = app
                    .state::<Desktop>()
                    .server
                    .lock()
                    .expect("lock poisoned")
                    .state
                    .running;
                if running {
                    commands::stop_and_publish(&app);
                } else if let Err(err) = commands::start(&app, None) {
                    warn!("{err}");
                }
            });
        }
        // Ends in RunEvent::Exit, which stops the server.
        ID_QUIT => app.exit(0),
        _ => {}
    }
}

/// Makes the tray menu label and tooltip follow the server state.
pub(super) fn update(app: &AppHandle, running: bool) {
    let desktop = app.state::<Desktop>();
    let Some(tray) = desktop.tray.get() else {
        return;
    };
    let (label, tooltip) = if running {
        ("Stop server", "Weylus - running")
    } else {
        ("Start server", "Weylus - stopped")
    };
    if let Err(err) = tray.toggle_server.set_text(label) {
        warn!("Failed to update the tray menu: {err}");
    }
    let _ = tray.icon.set_tooltip(Some(tooltip));
}

pub(super) fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn toggle_window(app: &AppHandle) {
    match app.get_webview_window("main") {
        Some(window) if window.is_visible().unwrap_or(false) => {
            let _ = window.hide();
        }
        _ => show_window(app),
    }
}

/// Whether closing the window should hide it instead of quitting.
pub(super) fn keeps_running_when_closed(app: &AppHandle) -> bool {
    let desktop = app.state::<Desktop>();
    desktop.tray.get().is_some() && desktop.config.lock().expect("lock poisoned").close_to_tray
}
