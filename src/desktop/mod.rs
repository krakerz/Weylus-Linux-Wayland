use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tracing::warn;

use crate::config::Config;
use crate::update::{self, CheckMode, Progress, Status, Updater};
use crate::web::ApprovalRequest;
use crate::weylus::Weylus;

mod address;
mod commands;
mod input_areas;
mod tray;

pub use input_areas::request_input_areas;

const MAX_LOG_LINES: usize = 1000;
const UPDATE_POLL: Duration = Duration::from_millis(150);

pub(crate) struct Desktop {
    config: Mutex<Config>,
    server: Mutex<Server>,
    updater: Arc<Updater>,
    logs: Mutex<LogBuffer>,
    tray: OnceLock<tray::Tray>,
    /// Prompts waiting for an answer, by request id.
    approvals: Mutex<HashMap<u64, ApprovalRequest>>,
}

struct Server {
    weylus: Weylus,
    state: ServerState,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct LogLine {
    seq: u64,
    line: String,
}

/// Keeps the last lines so the window can show the history from before it was ready.
#[derive(Default)]
struct LogBuffer {
    lines: VecDeque<LogLine>,
    next_seq: u64,
}

impl LogBuffer {
    fn push(&mut self, line: String) -> LogLine {
        let entry = LogLine {
            seq: self.next_seq,
            line,
        };
        self.next_seq += 1;
        if self.lines.len() == MAX_LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(entry.clone());
        entry
    }
}

#[derive(Serialize, Clone, Default, Debug)]
pub(crate) struct ServerState {
    running: bool,
    /// `http(s)://ip:port`, without the access code.
    url: Option<String>,
    qr_svg: Option<String>,
    other_urls: Vec<String>,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub(crate) struct UpdateView {
    state: &'static str,
    version: Option<String>,
    error: Option<String>,
    fraction: Option<f32>,
    progress: Option<String>,
    current_version: &'static str,
    install_kind: &'static str,
    blocker: Option<String>,
}

struct InstallInfo {
    kind: &'static str,
    blocker: Option<String>,
}

fn install_info() -> &'static InstallInfo {
    static INFO: OnceLock<InstallInfo> = OnceLock::new();
    INFO.get_or_init(|| {
        let kind = update::install_kind();
        InstallInfo {
            kind: kind.describe(),
            blocker: kind.update_blocker(),
        }
    })
}

fn view_of(status: &Status, progress: &Progress) -> UpdateView {
    let info = install_info();
    let mut view = UpdateView {
        state: "idle",
        version: None,
        error: None,
        fraction: None,
        progress: None,
        current_version: update::current_version(),
        install_kind: info.kind,
        blocker: info.blocker.clone(),
    };
    match status {
        Status::Idle => {}
        Status::Checking => view.state = "checking",
        Status::UpToDate => view.state = "up_to_date",
        Status::Available(update) => {
            view.state = "available";
            view.version = Some(update.version.clone());
        }
        Status::Downloading { version } => {
            view.state = "downloading";
            view.version = Some(version.clone());
            view.fraction = progress.fraction();
            view.progress = Some(progress.describe());
        }
        Status::Installed { version } => {
            view.state = "installed";
            view.version = Some(version.clone());
        }
        Status::CheckFailed(error) => {
            view.state = "check_failed";
            view.error = Some(error.clone());
        }
        Status::UpdateFailed { update, error } => {
            view.state = "update_failed";
            view.version = Some(update.version.clone());
            view.error = Some(error.clone());
        }
    }
    view
}

fn update_view(updater: &Updater) -> UpdateView {
    view_of(&updater.status(), updater.progress())
}

pub fn run(config: Config, log_receiver: mpsc::Receiver<String>) {
    let desktop = Desktop {
        config: Mutex::new(config),
        server: Mutex::new(Server {
            weylus: Weylus::new(),
            state: ServerState::default(),
        }),
        updater: Updater::new(),
        logs: Mutex::new(LogBuffer::default()),
        tray: OnceLock::new(),
        approvals: Mutex::default(),
    };

    let app = tauri::Builder::default()
        .manage(desktop)
        .setup(move |app| {
            setup(app.handle(), log_receiver);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && tray::keeps_running_when_closed(window.app_handle())
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::settings,
            commands::start_server,
            commands::stop_server,
            commands::server_state,
            commands::logs,
            commands::update_state,
            commands::update_check,
            commands::update_apply,
            commands::update_restart,
            commands::set_check_updates,
            commands::set_close_to_tray,
            commands::submit_input_areas,
            commands::cancel_input_areas,
            commands::answer_approval,
            commands::clients,
            commands::disconnect_client,
            commands::block_client,
            commands::check_allowed_clients,
            commands::open_source_link,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the Tauri app");

    let _ = app.run_return(|app, event| {
        // Here and not in a Drop: AppHandle clones held by worker threads keep the state alive.
        if let RunEvent::Exit = event {
            commands::stop(&app.state::<Desktop>());
        }
    });
}

fn setup(app: &tauri::AppHandle, log_receiver: mpsc::Receiver<String>) {
    let _ = input_areas::APP.set(app.clone());
    if let Some(window) = app.get_webview_window("main")
        && let Err(err) = window.set_title(&format!("Weylus - {}", env!("CARGO_PKG_VERSION")))
    {
        warn!("Failed to set the window title: {err}");
    }

    tray::create(app);

    let log_app = app.clone();
    std::thread::spawn(move || {
        for line in log_receiver {
            let entry = log_app
                .state::<Desktop>()
                .logs
                .lock()
                .expect("lock poisoned")
                .push(line);
            let _ = log_app.emit("log", &entry);
        }
    });

    let update_app = app.clone();
    std::thread::spawn(move || {
        let updater = Arc::clone(&update_app.state::<Desktop>().updater);
        let mut last = None;
        loop {
            let view = update_view(&updater);
            if last.as_ref() != Some(&view) || view.state == "downloading" {
                let _ = update_app.emit("update", &view);
                last = Some(view);
            }
            std::thread::sleep(UPDATE_POLL);
        }
    });

    let desktop = app.state::<Desktop>();
    let (check_updates, auto_start) = {
        let config = desktop.config.lock().expect("lock poisoned");
        (config.check_updates, config.auto_start)
    };
    if check_updates {
        desktop.updater.check_in_background(CheckMode::Quiet);
    }
    if auto_start {
        let app = app.clone();
        // Weylus::start blocks on the web thread's start-up message.
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(err) = commands::start(&app, None) {
                warn!("{err}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(status: Status) -> UpdateView {
        view_of(&status, &Progress::default())
    }

    #[test]
    fn status_maps_to_state() {
        assert_eq!(view(Status::Idle).state, "idle");
        assert_eq!(view(Status::Checking).state, "checking");
        assert_eq!(view(Status::UpToDate).state, "up_to_date");
        assert_eq!(view(Status::CheckFailed("x".into())).state, "check_failed");
        assert_eq!(
            view(Status::CheckFailed("x".into())).error.as_deref(),
            Some("x")
        );
    }

    #[test]
    fn downloading_and_installed_carry_the_version() {
        let downloading = view(Status::Downloading {
            version: "1.2.3".into(),
        });
        assert_eq!(downloading.state, "downloading");
        assert_eq!(downloading.version.as_deref(), Some("1.2.3"));
        assert!(downloading.progress.is_some());
        assert_eq!(downloading.fraction, None);

        let installed = view(Status::Installed {
            version: "1.2.3".into(),
        });
        assert_eq!(installed.state, "installed");
        assert_eq!(installed.version.as_deref(), Some("1.2.3"));
        assert!(installed.progress.is_none());
    }

    #[test]
    fn idle_states_have_no_version_or_progress() {
        for status in [Status::Idle, Status::Checking, Status::UpToDate] {
            let v = view(status);
            assert!(v.version.is_none() && v.error.is_none() && v.progress.is_none());
        }
    }

    #[test]
    fn log_buffer_numbers_lines_and_caps_at_the_limit() {
        let mut buffer = LogBuffer::default();
        for i in 0..MAX_LOG_LINES + 5 {
            let entry = buffer.push(format!("line {i}"));
            assert_eq!(entry.seq, i as u64);
        }
        assert_eq!(buffer.lines.len(), MAX_LOG_LINES);
        assert_eq!(buffer.lines.front().unwrap().seq, 5);
    }
}
