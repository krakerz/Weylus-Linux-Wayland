use std::net::{IpAddr, SocketAddr};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tracing::{info, warn};

use super::{
    Desktop, LogLine, ServerState, UpdateView, address, autostart, input_areas, tray, update_view,
};
use crate::access::{self, AllowList, Approval};
use crate::clients::ClientInfo;
use crate::config::write_config;
use crate::protocol::CustomInputAreas;
use crate::update::{self, CheckMode, Status};
use crate::web::Web2UiMessage;

const SOURCE_URL: &str = "https://github.com/krakerz/Weylus-Linux-Wayland";

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct Settings {
    /// Empty means no access code.
    access_code: String,
    /// Comma/whitespace separated IPs and CIDR ranges; empty allows everyone.
    allowed_clients: String,
    ask_new_devices: bool,
    bind_address: String,
    web_port: u16,
    https: bool,
    auto_start: bool,
    wayland_support: bool,
    remember_screen: bool,
    close_to_tray: bool,
    try_vaapi: bool,
    try_nvenc: bool,
    check_updates: bool,
}

#[tauri::command]
pub(crate) async fn settings(app: AppHandle) -> Result<Settings, String> {
    let config = app
        .state::<Desktop>()
        .config
        .lock()
        .expect("lock poisoned")
        .clone();
    Ok(Settings {
        access_code: config.access_code.unwrap_or_default(),
        allowed_clients: config.allowed_clients.join(", "),
        ask_new_devices: config.ask_new_devices,
        bind_address: config.bind_address.to_string(),
        web_port: config.web_port,
        https: config.https,
        auto_start: config.auto_start,
        wayland_support: config.wayland_support,
        remember_screen: config.remember_screen,
        close_to_tray: config.close_to_tray,
        try_vaapi: config.try_vaapi,
        try_nvenc: config.try_nvenc,
        check_updates: config.check_updates,
    })
}

/// Starts the server with `settings`, or with the stored config when `None`. Blocks.
pub(crate) fn start(app: &AppHandle, settings: Option<Settings>) -> Result<ServerState, String> {
    let desktop = app.state::<Desktop>();
    let mut server = desktop.server.lock().expect("lock poisoned");
    if server.state.running {
        return Ok(server.state.clone());
    }

    let mut config = desktop.config.lock().expect("lock poisoned").clone();
    if let Some(settings) = settings {
        config.bind_address = settings
            .bind_address
            .trim()
            .parse::<IpAddr>()
            .map_err(|err| format!("Invalid bind address: {err}"))?;
        config.access_code = Some(settings.access_code).filter(|code| !code.is_empty());
        let allowed = access::split_entries(&settings.allowed_clients);
        AllowList::parse(allowed.iter().map(String::as_str))
            .map_err(|err| format!("Allowed clients: {err}"))?;
        config.allowed_clients = allowed;
        config.ask_new_devices = settings.ask_new_devices;
        config.web_port = settings.web_port;
        config.https = settings.https;
        config.auto_start = settings.auto_start;
        config.wayland_support = settings.wayland_support;
        config.remember_screen = settings.remember_screen;
        config.close_to_tray = settings.close_to_tray;
        config.try_vaapi = settings.try_vaapi;
        config.try_nvenc = settings.try_nvenc;
        config.check_updates = settings.check_updates;
    }

    let message_app = app.clone();
    if !server
        .weylus
        .start(&config, move |msg| on_web_message(&message_app, msg))
    {
        return Err("Couldn't start the server, see the log".to_string());
    }

    write_config(&config);
    let state = server_state_for(&config);
    server.state = state.clone();
    *desktop.config.lock().expect("lock poisoned") = config;
    publish(app, &state);
    Ok(state)
}

#[derive(Serialize, Clone)]
struct ApprovalPrompt {
    id: u64,
    ip: String,
    timeout_secs: u64,
}

/// Runs on Weylus' forwarding thread, so it must not take the `server` lock.
fn on_web_message(app: &AppHandle, msg: Web2UiMessage) {
    match msg {
        Web2UiMessage::UInputInaccessible => {
            let _ = app.emit(
                "uinput-inaccessible",
                include_str!("../strings/uinput_error.txt"),
            );
        }
        Web2UiMessage::ApproveClient(request) => {
            let prompt = ApprovalPrompt {
                id: request.id,
                ip: request.ip.to_string(),
                timeout_secs: access::APPROVAL_TIMEOUT.as_secs(),
            };
            app.state::<Desktop>()
                .approvals
                .lock()
                .expect("lock poisoned")
                .insert(request.id, request);
            let _ = app.emit("approval", prompt);
            tray::show_window(app);
            if let Some(window) = app.get_webview_window("main") {
                let _ =
                    window.request_user_attention(Some(tauri::UserAttentionType::Informational));
            }
        }
        Web2UiMessage::ApprovalExpired(id) => {
            app.state::<Desktop>()
                .approvals
                .lock()
                .expect("lock poisoned")
                .remove(&id);
            let _ = app.emit("approval-closed", id);
        }
        Web2UiMessage::ClientsChanged(list) => {
            let _ = app.emit("clients", list);
        }
    }
}

fn server_state_for(config: &crate::config::Config) -> ServerState {
    let addrs = address::tablet_addresses(SocketAddr::new(config.bind_address, config.web_port));
    let primary = addrs[0];
    let scheme = config.scheme();
    if addrs.len() > 1 {
        info!("Found more than one IP address for browsers to connect to,");
        info!("other urls are:");
        for addr in &addrs[1..] {
            info!("{scheme}://{addr}");
        }
    }
    let qr_svg = address::qr_svg(&address::tablet_url(
        scheme,
        primary,
        config.access_code.as_deref(),
    ))
    .map_err(|err| warn!("Couldn't render the QR code: {err}"))
    .ok();
    ServerState {
        running: true,
        url: Some(format!("{scheme}://{primary}")),
        qr_svg,
        other_urls: addrs[1..]
            .iter()
            .map(|addr| format!("{scheme}://{addr}"))
            .collect(),
    }
}

/// Tells the window and the tray about a new server state.
fn publish(app: &AppHandle, state: &ServerState) {
    let _ = app.emit("server", state);
    tray::update(app, state.running);
}

/// Stops the server and publishes the new state. Blocks.
pub(crate) fn stop_and_publish(app: &AppHandle) -> ServerState {
    let desktop = app.state::<Desktop>();
    let state = stop(&desktop);
    publish(app, &state);
    // dropping the requests refuses the devices still waiting
    let pending: Vec<u64> = desktop
        .approvals
        .lock()
        .expect("lock poisoned")
        .drain()
        .map(|(id, _)| id)
        .collect();
    for id in pending {
        let _ = app.emit("approval-closed", id);
    }
    let _ = app.emit("clients", Vec::<ClientInfo>::new());
    state
}

/// Stops the server if it runs. Blocks until the web thread has ended.
pub(crate) fn stop(desktop: &Desktop) -> ServerState {
    let mut server = desktop.server.lock().expect("lock poisoned");
    // notify_one on a stopped server would leave a permit that ends the next start at once
    if server.state.running {
        server.weylus.stop();
        server.state = ServerState::default();
    }
    server.state.clone()
}

#[tauri::command]
pub(crate) async fn start_server(
    app: AppHandle,
    settings: Settings,
) -> Result<ServerState, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app, Some(settings)))
        .await
        .map_err(|err| err.to_string())?
}

#[tauri::command]
pub(crate) async fn stop_server(app: AppHandle) -> Result<ServerState, String> {
    tauri::async_runtime::spawn_blocking(move || stop_and_publish(&app))
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub(crate) async fn server_state(app: AppHandle) -> Result<ServerState, String> {
    Ok(app
        .state::<Desktop>()
        .server
        .lock()
        .expect("lock poisoned")
        .state
        .clone())
}

#[tauri::command]
pub(crate) async fn logs(app: AppHandle) -> Result<Vec<LogLine>, String> {
    Ok(app
        .state::<Desktop>()
        .logs
        .lock()
        .expect("lock poisoned")
        .lines
        .iter()
        .cloned()
        .collect())
}

#[tauri::command]
pub(crate) fn update_state(state: tauri::State<'_, Desktop>) -> UpdateView {
    update_view(&state.updater)
}

#[tauri::command]
pub(crate) fn update_check(state: tauri::State<'_, Desktop>) {
    state.updater.check_in_background(CheckMode::Manual);
}

#[tauri::command]
pub(crate) fn update_apply(state: tauri::State<'_, Desktop>) {
    state.updater.apply_in_background();
}

#[tauri::command]
pub(crate) fn update_restart(app: AppHandle, state: tauri::State<'_, Desktop>) {
    if matches!(state.updater.status(), Status::Installed { .. }) {
        update::request_relaunch();
        app.exit(0);
    }
}

#[tauri::command]
pub(crate) fn set_check_updates(state: tauri::State<'_, Desktop>, enabled: bool) {
    let mut config = state.config.lock().expect("lock poisoned");
    config.check_updates = enabled;
    write_config(&config);
}

#[tauri::command]
pub(crate) fn set_close_to_tray(state: tauri::State<'_, Desktop>, enabled: bool) {
    let mut config = state.config.lock().expect("lock poisoned");
    config.close_to_tray = enabled;
    write_config(&config);
}

#[tauri::command]
pub(crate) fn autostart_enabled() -> bool {
    autostart::is_enabled()
}

#[tauri::command]
pub(crate) fn set_autostart(enabled: bool) -> Result<(), String> {
    let result = if enabled {
        autostart::enable()
    } else {
        autostart::disable()
    };
    result.map_err(|err| format!("{err:#}"))
}

#[tauri::command]
pub(crate) fn submit_input_areas(areas: CustomInputAreas) -> Result<(), String> {
    input_areas::submit(areas)
}

#[tauri::command]
pub(crate) fn cancel_input_areas() {
    input_areas::cancel();
}

#[tauri::command]
pub(crate) fn answer_approval(
    app: AppHandle,
    id: u64,
    approval: Approval,
) -> Result<Option<String>, String> {
    const EXPIRED: &str = "The request has expired";
    let desktop = app.state::<Desktop>();
    let request = desktop
        .approvals
        .lock()
        .expect("lock poisoned")
        .remove(&id)
        .ok_or(EXPIRED)?;
    let ip = request.ip;
    request.reply.send(approval).map_err(|_| EXPIRED)?;
    if approval != Approval::Always {
        return Ok(None);
    }
    let mut config = desktop.config.lock().expect("lock poisoned");
    let covered = AllowList::parse(config.allowed_clients.iter().map(String::as_str))
        .is_ok_and(|list| list.contains(ip));
    if !covered {
        config.allowed_clients.push(ip.to_canonical().to_string());
        write_config(&config);
    }
    Ok(Some(config.allowed_clients.join(", ")))
}

#[tauri::command]
pub(crate) fn clients(app: AppHandle) -> Vec<ClientInfo> {
    app.state::<Desktop>()
        .server
        .lock()
        .expect("lock poisoned")
        .weylus
        .clients()
}

#[tauri::command]
pub(crate) fn disconnect_client(app: AppHandle, id: u64) -> Result<(), String> {
    let desktop = app.state::<Desktop>();
    let server = desktop.server.lock().expect("lock poisoned");
    if !server.weylus.disconnect_client(id) {
        return Err("This device is no longer connected".to_string());
    }
    info!("Disconnected device {id}.");
    Ok(())
}

/// Disconnects the device and refuses its address until the server stops.
#[tauri::command]
pub(crate) fn block_client(app: AppHandle, id: u64) -> Result<(), String> {
    let desktop = app.state::<Desktop>();
    let server = desktop.server.lock().expect("lock poisoned");
    server
        .weylus
        .block_client(id)
        .map(|_| ())
        .ok_or_else(|| "This device is no longer connected".to_string())
}

#[tauri::command]
pub(crate) fn check_allowed_clients(text: String) -> Result<(), String> {
    let entries = access::split_entries(&text);
    AllowList::parse(entries.iter().map(String::as_str))
        .map(|_| ())
        .map_err(|err| format!("Allowed clients: {err}"))
}

/// Opens the source code page in the default browser (AGPL source offer).
#[tauri::command]
pub(crate) fn open_source_link() {
    if let Err(err) = Command::new("xdg-open").arg(SOURCE_URL).spawn() {
        warn!("Couldn't open {SOURCE_URL}: {err}");
    }
}
