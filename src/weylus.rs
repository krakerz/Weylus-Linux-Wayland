use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tracing::{error, info};

use crate::access::{AccessControl, AllowList, Prompt};
use crate::clients::{ClientInfo, Clients};
use crate::config::Config;
use crate::video::EncoderOptions;
use crate::web::{Web2UiMessage, WebServerConfig, WebStartUpMessage};
use crate::websocket::WeylusClientConfig;

pub struct Weylus {
    notify_shutdown: Arc<tokio::sync::Notify>,
    web_thread: Option<std::thread::JoinHandle<()>>,
    clients: Option<Clients>,
    access: Option<Arc<Mutex<AccessControl>>>,
}

impl Weylus {
    pub fn new() -> Self {
        Self {
            notify_shutdown: Arc::new(tokio::sync::Notify::new()),
            web_thread: None,
            clients: None,
            access: None,
        }
    }

    pub fn start(
        &mut self,
        config: &Config,
        mut on_web_message: impl FnMut(Web2UiMessage) + Send + 'static,
    ) -> bool {
        let encoder_options = EncoderOptions {
            try_vaapi: config.try_vaapi,
            try_nvenc: config.try_nvenc,
            ..Default::default()
        };

        let allow_list = match AllowList::parse(config.allowed_clients.iter().map(String::as_str)) {
            Ok(list) => list,
            Err(err) => {
                error!("Invalid allowed clients: {err}.");
                return false;
            }
        };
        let prompt = if config.ask_new_devices && !config.no_gui {
            Prompt::Ask
        } else {
            Prompt::Never
        };
        let access = Arc::new(Mutex::new(AccessControl::new(allow_list, prompt)));

        let (sender_ui, mut receiver_ui) = tokio::sync::mpsc::channel(100);
        let clients = {
            let sender = sender_ui.clone();
            Clients::new(move |list| {
                let _ = sender.try_send(Web2UiMessage::ClientsChanged(list));
            })
        };
        let (sender_startup, receiver_startup) = tokio::sync::oneshot::channel();

        let web_thread = crate::web::run(
            sender_ui,
            sender_startup,
            self.notify_shutdown.clone(),
            WebServerConfig {
                bind_addr: SocketAddr::new(config.bind_address, config.web_port),
                access_code: config.access_code.clone(),
                custom_index_html: config.custom_index_html.clone(),
                custom_access_html: config.custom_access_html.clone(),
                custom_style_css: config.custom_style_css.clone(),
                custom_lib_js: config.custom_lib_js.clone(),
                enable_custom_input_areas: config.wayland_support,
                access: access.clone(),
                clients: clients.clone(),
            },
            WeylusClientConfig {
                encoder_options,
                wayland_support: config.wayland_support,
                // without the portal there is no session to remember
                remember_screen: config.remember_screen && config.wayland_support,
            },
        );

        match receiver_startup.blocking_recv() {
            Ok(WebStartUpMessage::Start) => (),
            Ok(WebStartUpMessage::Error) => {
                if web_thread.join().is_err() {
                    error!("Webserver thread panicked.");
                }
                return false;
            }
            Err(err) => {
                error!("Error communicating with webserver thread: {}", err);
                if web_thread.join().is_err() {
                    error!("Webserver thread panicked.");
                }
                return false;
            }
        }
        self.web_thread = Some(web_thread);
        self.clients = Some(clients);
        self.access = Some(access);
        std::thread::spawn(move || {
            while let Some(msg) = receiver_ui.blocking_recv() {
                on_web_message(msg);
            }
        });
        true
    }

    pub fn clients(&self) -> Vec<ClientInfo> {
        self.clients
            .as_ref()
            .map(Clients::snapshot)
            .unwrap_or_default()
    }

    pub fn disconnect_client(&self, id: u64) -> bool {
        self.clients
            .as_ref()
            .is_some_and(|clients| clients.disconnect(id))
    }

    /// Disconnects client `id` and refuses its address until the server stops. Returns the
    /// address, or `None` when the client is gone.
    pub fn block_client(&self, id: u64) -> Option<String> {
        let clients = self.clients.as_ref()?;
        let access = self.access.as_ref()?;
        let ip_text = clients.ip_of(id)?;
        let ip = ip_text.parse().ok()?;
        let in_allow_list = access.lock().expect("lock poisoned").block(ip);
        if in_allow_list {
            info!("Blocking {ip_text} for this session although it is in the allowed clients.");
        } else {
            info!("Blocking {ip_text} until the server stops.");
        }
        clients.disconnect_ip(ip);
        Some(ip_text)
    }

    pub fn stop(&mut self) {
        self.notify_shutdown.notify_one();
        self.wait();
    }

    fn wait(&mut self) {
        if let Some(t) = self.web_thread.take()
            && t.join().is_err()
        {
            error!("Web thread panicked.");
        }
        self.clients = None;
        self.access = None;
    }
}

impl Drop for Weylus {
    fn drop(&mut self) {
        self.stop();
    }
}
