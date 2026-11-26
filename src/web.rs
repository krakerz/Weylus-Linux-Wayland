use bytes::Bytes;
use fastwebsockets::upgrade;
use handlebars::Handlebars;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

use crate::access::{self, AccessControl, Admission, Approval};
use crate::clients::{ClientInfo, Clients};
use crate::websocket::{WeylusClientConfig, WeylusClientHandler, weylus_websocket_channel};

#[derive(Debug)]
pub enum WebStartUpMessage {
    Start,
    Error,
}

pub enum Web2UiMessage {
    UInputInaccessible,
    /// Answer through `reply`; dropping it refuses the device.
    ApproveClient(ApprovalRequest),
    /// The request with this id timed out; close its prompt.
    ApprovalExpired(u64),
    ClientsChanged(Vec<ClientInfo>),
}

pub struct ApprovalRequest {
    pub id: u64,
    pub ip: IpAddr,
    pub reply: oneshot::Sender<Approval>,
}

pub const INDEX_HTML: &str = std::include_str!("../www/templates/index.html");
pub const ACCESS_HTML: &str = std::include_str!("../www/static/access_code.html");
pub const STYLE_CSS: &str = std::include_str!("../www/static/style.css");
pub const LIB_JS: &str = std::include_str!("../www/static/lib.js");

#[derive(Serialize)]
struct IndexTemplateContext {
    access_code: Option<String>,
    uinput_enabled: bool,
    capture_cursor_enabled: bool,
    log_level: String,
    enable_custom_input_areas: bool,
}

fn response_from_str(s: &str, content_type: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .body(s.to_string().into())
        .unwrap()
}

fn response_not_found() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("content-type", "text/html; charset=utf-8")
        .body("Not found!".into())
        .unwrap()
}

fn response_locked_out() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("content-type", "text/plain; charset=utf-8")
        .body("Too many wrong access codes, try again in a few minutes.".into())
        .unwrap()
}

async fn response_from_path_or_default(
    path: Option<&PathBuf>,
    default: &str,
    content_type: &str,
) -> Response<Full<Bytes>> {
    match path {
        Some(path) => match tokio::fs::read_to_string(path).await {
            Ok(s) => response_from_str(&s, content_type),
            Err(err) => {
                warn!("Failed to load file: {}", err);
                response_from_str(default, content_type)
            }
        },
        None => response_from_str(default, content_type),
    }
}

/// Decides whether a connection from `ip` is served at all; may wait for the desktop prompt.
async fn admit(
    context: &Arc<Context<'static>>,
    ip: IpAddr,
    sender_ui: &mpsc::Sender<Web2UiMessage>,
) -> bool {
    let ip = ip.to_canonical();
    let admission = context
        .web_config
        .access
        .lock()
        .expect("lock poisoned")
        .admission(ip, Instant::now());
    match admission {
        Admission::Allow => return true,
        Admission::Deny(reason) => {
            warn!("Refused connection from {ip}: {reason}.");
            return false;
        }
        Admission::Ask => {}
    }

    let mut rx = {
        let mut pending = context.pending_approvals.lock().expect("lock poisoned");
        if let Some(rx) = pending.get(&ip) {
            rx.clone()
        } else {
            let (tx, rx) = watch::channel(None);
            let (reply, reply_rx) = oneshot::channel();
            let id = context.next_approval_id.fetch_add(1, Ordering::Relaxed);
            info!("Asking whether to allow {ip}.");
            // try_send: the lock must not be held across an await
            if sender_ui
                .try_send(Web2UiMessage::ApproveClient(ApprovalRequest {
                    id,
                    ip,
                    reply,
                }))
                .is_err()
            {
                return false;
            }
            pending.insert(ip, rx.clone());
            tokio::spawn(resolve_approval(
                context.clone(),
                sender_ui.clone(),
                ip,
                id,
                reply_rx,
                tx,
            ));
            rx
        }
    };
    let admitted = match rx.wait_for(|answer| answer.is_some()).await {
        Ok(answer) => *answer == Some(true),
        Err(_) => false,
    };
    if !admitted {
        warn!("Refused connection from {ip}: not approved.");
    }
    admitted
}

async fn resolve_approval(
    context: Arc<Context<'static>>,
    sender_ui: mpsc::Sender<Web2UiMessage>,
    ip: IpAddr,
    id: u64,
    reply: oneshot::Receiver<Approval>,
    answer: watch::Sender<Option<bool>>,
) {
    let admitted = match tokio::time::timeout(access::APPROVAL_TIMEOUT, reply).await {
        Ok(Ok(approval)) => {
            context
                .web_config
                .access
                .lock()
                .expect("lock poisoned")
                .approve(ip, approval, Instant::now());
            match approval {
                Approval::Once => info!("Allowed {ip} until the server stops."),
                Approval::Always => info!("Always allowing {ip}."),
                Approval::Deny => info!("Denied {ip}."),
            }
            approval != Approval::Deny
        }
        Ok(Err(_)) => false,
        Err(_) => {
            info!("No answer for {ip}; connection refused.");
            let _ = sender_ui.send(Web2UiMessage::ApprovalExpired(id)).await;
            false
        }
    };
    context
        .pending_approvals
        .lock()
        .expect("lock poisoned")
        .remove(&ip);
    let _ = answer.send(Some(admitted));
}

async fn serve(
    addr: SocketAddr,
    mut req: Request<Incoming>,
    context: Arc<Context<'_>>,
    sender_ui: mpsc::Sender<Web2UiMessage>,
    num_clients: Arc<AtomicUsize>,
    semaphore_websocket_shutdown: Arc<tokio::sync::Semaphore>,
    notify_disconnect: Arc<tokio::sync::Notify>,
) -> Result<Response<BoxBody<Bytes, Infallible>>, hyper::Error> {
    debug!("Got request: {:?}", req);
    let ip = addr.ip().to_canonical();
    let mut authed = false;
    if let Some(access_code) = &context.web_config.access_code {
        if req.method() == Method::GET && (req.uri().path() == "/" || req.uri().path() == "/ws") {
            if context
                .web_config
                .access
                .lock()
                .expect("lock poisoned")
                .is_locked_out(ip, Instant::now())
            {
                return Ok(response_locked_out().map(|r| r.boxed()));
            }
            use url::form_urlencoded;
            if let Some(query) = req.uri().query() {
                let params = form_urlencoded::parse(query.as_bytes())
                    .into_owned()
                    .collect::<HashMap<String, String>>();
                if let Some(code) = params.get("access_code") {
                    let mut access = context.web_config.access.lock().expect("lock poisoned");
                    if access::access_code_matches(code, access_code) {
                        access.clear_failures(ip);
                        authed = true;
                        debug!(address = ?addr, "Web-Client authenticated.");
                    } else if access.record_failure(ip, Instant::now()) {
                        warn!(
                            "Wrong access code from {ip}; blocked for {} minutes.",
                            access::LOCKOUT.as_secs() / 60
                        );
                    } else {
                        warn!("Wrong access code from {ip}.");
                    }
                }
            }
        }
    } else {
        authed = true;
    }
    if req.method() != Method::GET {
        return Ok(response_not_found().map(|r| r.boxed()));
    }
    match req.uri().path() {
        "/" => {
            if !authed {
                return Ok(response_from_path_or_default(
                    context.web_config.custom_access_html.as_ref(),
                    ACCESS_HTML,
                    "text/html; charset=utf-8",
                )
                .await
                .map(|r| r.boxed()));
            }
            let config = IndexTemplateContext {
                access_code: context.web_config.access_code.clone(),
                uinput_enabled: true,
                capture_cursor_enabled: true,
                log_level: crate::log::get_log_level().to_string(),
                enable_custom_input_areas: context.web_config.enable_custom_input_areas,
            };

            let html = if let Some(path) = context.web_config.custom_index_html.as_ref() {
                let mut reg = Handlebars::new();
                if let Err(err) = reg.register_template_file("index", path) {
                    warn!("Failed to register template from path: {}", err);
                    context.templates.render("index", &config)
                } else {
                    reg.render("index", &config)
                }
            } else {
                context.templates.render("index", &config)
            };

            match html {
                Ok(html) => {
                    Ok(response_from_str(&html, "text/html; charset=utf-8").map(|r| r.boxed()))
                }
                Err(err) => {
                    error!("Failed to render index template: {}", err);
                    Ok(response_not_found().map(|r| r.boxed()))
                }
            }
        }
        "/ws" => {
            if !authed {
                return Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body("unauthorized".to_string().boxed())
                    .unwrap());
            }

            let (response, fut) = upgrade::upgrade(&mut req).unwrap();
            num_clients.fetch_add(1, Ordering::Relaxed);

            let config = context.weylus_client_config;
            let clients = context.web_config.clients.clone();
            tokio::spawn(async move {
                match fut.await {
                    Ok(ws) => {
                        let client_entry = clients.add(addr.ip());
                        let (sender, receiver) = weylus_websocket_channel(
                            ws,
                            semaphore_websocket_shutdown,
                            client_entry.disconnect_signal(),
                        );
                        std::thread::spawn(move || {
                            let client = WeylusClientHandler::new(
                                sender,
                                receiver,
                                || {
                                    if let Err(err) =
                                        sender_ui.blocking_send(Web2UiMessage::UInputInaccessible)
                                    {
                                        warn!(
                                            "Failed to send message 'UInputInaccessible': {err}."
                                        );
                                    }
                                },
                                config,
                                client_entry.video_reporter(),
                            );
                            client.run();
                            drop(client_entry);
                            num_clients.fetch_sub(1, Ordering::Relaxed);
                            notify_disconnect.notify_waiters();
                        });
                    }
                    Err(err) => {
                        eprintln!("Error in websocket connection: {}", err);
                        num_clients.fetch_sub(1, Ordering::Relaxed);
                        notify_disconnect.notify_waiters();
                    }
                }
            });

            Ok(response.map(|r| r.boxed()))
        }
        "/style.css" => Ok(response_from_path_or_default(
            context.web_config.custom_style_css.as_ref(),
            STYLE_CSS,
            "text/css; charset=utf-8",
        )
        .await
        .map(|r| r.boxed())),
        "/lib.js" => Ok(response_from_path_or_default(
            context.web_config.custom_lib_js.as_ref(),
            LIB_JS,
            "text/javascript; charset=utf-8",
        )
        .await
        .map(|r| r.boxed())),
        _ => Ok(response_not_found().map(|r| r.boxed())),
    }
}

#[derive(Clone)]
pub struct WebServerConfig {
    pub bind_addr: SocketAddr,
    pub access_code: Option<String>,
    pub custom_index_html: Option<PathBuf>,
    pub custom_access_html: Option<PathBuf>,
    pub custom_style_css: Option<PathBuf>,
    pub custom_lib_js: Option<PathBuf>,
    pub enable_custom_input_areas: bool,
    /// Shared with `Weylus`, which blocks devices through it.
    pub access: Arc<Mutex<AccessControl>>,
    pub clients: Clients,
}

struct Context<'a> {
    web_config: WebServerConfig,
    weylus_client_config: WeylusClientConfig,
    templates: Handlebars<'a>,
    /// Prompts in flight per IP; later connections from that IP wait for the same answer.
    pending_approvals: Mutex<HashMap<IpAddr, watch::Receiver<Option<bool>>>>,
    next_approval_id: AtomicU64,
}

pub fn run(
    sender_ui: tokio::sync::mpsc::Sender<Web2UiMessage>,
    sender_startup: oneshot::Sender<WebStartUpMessage>,
    notify_shutdown: Arc<tokio::sync::Notify>,
    web_server_config: WebServerConfig,
    weylus_client_config: WeylusClientConfig,
) -> std::thread::JoinHandle<()> {
    let mut templates = Handlebars::new();
    templates
        .register_template_string("index", INDEX_HTML)
        .unwrap();

    let context = Context {
        web_config: web_server_config,
        weylus_client_config,
        templates,
        pending_approvals: Mutex::default(),
        next_approval_id: AtomicU64::new(1),
    };
    std::thread::spawn(move || run_server(context, sender_ui, sender_startup, notify_shutdown))
}

#[tokio::main]
async fn run_server(
    context: Context<'static>,
    sender_ui: tokio::sync::mpsc::Sender<Web2UiMessage>,
    sender_startup: oneshot::Sender<WebStartUpMessage>,
    notify_shutdown: Arc<tokio::sync::Notify>,
) {
    let addr = context.web_config.bind_addr;

    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            error!("Failed to bind to socket: {err}.");
            sender_startup.send(WebStartUpMessage::Error).unwrap();
            return;
        }
    };

    sender_startup.send(WebStartUpMessage::Start).unwrap();

    let context = Arc::new(context);

    let broadcast_shutdown = Arc::new(tokio::sync::Notify::new());

    let num_clients = Arc::new(AtomicUsize::new(0));
    let notify_disconnect = Arc::new(tokio::sync::Notify::new());
    let semaphore_websocket_shutdown = Arc::new(tokio::sync::Semaphore::new(0));

    loop {
        let (tcp, remote_address) = tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok(conn) => conn,
                    Err(err) => {
                        warn!("Connection failed: {err}.");
                        continue;
                    }
                }
            },
            _ = notify_shutdown.notified() => {
                info!("Webserver is shutting down.");
                broadcast_shutdown.notify_waiters();
                break;
            }
        };

        debug!(address = ?remote_address, "Client connected.");

        if let Err(err) = tcp.set_nodelay(true) {
            warn!("Failed to set TCP_NODELAY: {err}.");
        }
        limit_unsent_bytes(&tcp);

        let io = TokioIo::new(tcp);

        let sender_ui = sender_ui.clone();
        let broadcast_shutdown = broadcast_shutdown.clone();
        let context = context.clone();
        let num_clients = num_clients.clone();
        let semaphore_websocket_shutdown = semaphore_websocket_shutdown.clone();
        let notify_disconnect = notify_disconnect.clone();

        tokio::task::spawn(async move {
            // before hyper reads anything, so a refused device gets neither the page nor the websocket
            let admitted = tokio::select! {
                admitted = admit(&context, remote_address.ip(), &sender_ui) => admitted,
                _ = broadcast_shutdown.notified() => false,
            };
            if !admitted {
                return;
            }
            let conn = http1::Builder::new().serve_connection(
                io,
                service_fn({
                    move |req| {
                        let context = context.clone();
                        let num_clients = num_clients.clone();
                        let semaphore_websocket_shutdown = semaphore_websocket_shutdown.clone();
                        let notify_disconnect = notify_disconnect.clone();
                        serve(
                            remote_address,
                            req,
                            context,
                            sender_ui.clone(),
                            num_clients,
                            semaphore_websocket_shutdown,
                            notify_disconnect,
                        )
                    }
                }),
            );

            let conn = conn.with_upgrades();

            tokio::select! {
                conn = conn => match conn {
                    Ok(_) => (),
                    Err(err) => {
                        warn!("Error polling connection ({remote_address}): {err}.")
                    }
                },
                _ = broadcast_shutdown.notified() => {
                    info!("Closing connection to: {remote_address}.");
                }
            }
        });
    }

    semaphore_websocket_shutdown.add_permits(num_clients.load(Ordering::Relaxed));

    loop {
        let remaining_clients = num_clients.load(Ordering::Relaxed);
        if remaining_clients == 0 {
            break;
        } else {
            debug!("Waiting for remaining clients ({remaining_clients}) to disconnect.");
        }
        tokio::select! {
            _ = notify_disconnect.notified() => (),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                semaphore_websocket_shutdown.add_permits(num_clients.load(Ordering::Relaxed));
            },
        }
    }
}

/// Unsent bytes the kernel may hold per socket; beyond this, writes wait, so the video backlog
/// counter in websocket.rs sees a slow link instead of an ever-growing send buffer.
const TCP_NOTSENT_LOWAT_BYTES: libc::c_int = 128 * 1024;

fn limit_unsent_bytes(tcp: &tokio::net::TcpStream) {
    use std::os::fd::AsRawFd;

    // SAFETY: the fd is valid for the duration of the call and optval points to a live c_int of
    // the given size.
    let ret = unsafe {
        libc::setsockopt(
            tcp.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_NOTSENT_LOWAT,
            (&TCP_NOTSENT_LOWAT_BYTES as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        warn!(
            "Failed to set TCP_NOTSENT_LOWAT: {}.",
            std::io::Error::last_os_error()
        );
    }
}
