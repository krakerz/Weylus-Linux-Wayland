use fastwebsockets::{FragmentCollectorRead, Frame, OpCode, WebSocket, WebSocketError};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{JoinHandle, spawn};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::channel;
use tracing::{error, info, trace, warn};

use crate::capturable::{Capturable, Recorder, get_capturables};
use crate::input::device::InputDevice;
use crate::protocol::{
    ClientConfiguration, KeyboardEvent, MessageInbound, MessageOutbound, PointerEvent,
    WeylusReceiver, WeylusSender, WheelEvent,
};

use crate::cerror::CErrorCode;
use crate::clients::VideoInfo;
use crate::codec::CodecOrder;
use crate::video::{EncoderOptions, VideoEncoder};

struct VideoConfig {
    capturable: Box<dyn Capturable>,
    capture_cursor: bool,
    max_width: usize,
    max_height: usize,
    frame_rate: f64,
    bitrate_kbps: u32,
    codecs: CodecOrder,
    /// generation of the remembered screen share the capturable came from, 0 if none
    generation: u64,
}

enum VideoCommands {
    Start(VideoConfig),
    Pause,
    Resume,
    Restart,
    RequestKeyframe,
}

/// A running recorder kept after its connection ended, so the next one can pick it up.
struct ParkedRecorder {
    source: String,
    recorder: Box<dyn Recorder>,
}

/// The screen share (portal session) remembered until the server exits or the client refreshes.
struct RememberedScreen {
    // Declared first so the recorder is dropped before the capturables that keep the portal
    // session alive.
    recorder: Option<ParkedRecorder>,
    capturables: Vec<Box<dyn Capturable>>,
    capture_cursor: bool,
    /// identifies the portal session, a recorder only fits capturables of the same one
    generation: u64,
}

static REMEMBERED_SCREEN: Mutex<Option<RememberedScreen>> = Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn remembered_screen() -> std::sync::MutexGuard<'static, Option<RememberedScreen>> {
    REMEMBERED_SCREEN.lock().expect("lock poisoned")
}

/// The remembered capturables, if they were requested with the same cursor mode.
fn remembered_capturables(capture_cursor: bool) -> Option<(Vec<Box<dyn Capturable>>, u64)> {
    remembered_screen()
        .as_ref()
        .filter(|screen| screen.capture_cursor == capture_cursor)
        .map(|screen| (screen.capturables.clone(), screen.generation))
}

/// Drops the remembered screen, its parked recorder first.
fn forget_screen() {
    // dropped after the lock is released, closing a pipeline can take a moment
    let old = remembered_screen().take();
    drop(old);
}

fn remember_screen(capturables: &[Box<dyn Capturable>], capture_cursor: bool) -> u64 {
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    *remembered_screen() = Some(RememberedScreen {
        recorder: None,
        capturables: capturables.to_vec(),
        capture_cursor,
        generation,
    });
    generation
}

fn take_parked_recorder(generation: u64, source: &str) -> Option<Box<dyn Recorder>> {
    let mut guard = remembered_screen();
    let screen = guard
        .as_mut()
        .filter(|screen| screen.generation == generation)?;
    if screen.recorder.as_ref()?.source == source {
        screen.recorder.take().map(|parked| parked.recorder)
    } else {
        None
    }
}

/// Keeps a recorder for the next connection; drops it if its portal session is gone.
fn park_recorder(generation: u64, source: String, recorder: Box<dyn Recorder>) {
    let mut guard = remembered_screen();
    if let Some(screen) = guard
        .as_mut()
        .filter(|screen| screen.generation == generation)
    {
        // a different parked recorder goes first: only one pipeline at a time
        screen.recorder = None;
        screen.recorder = Some(ParkedRecorder { source, recorder });
    }
}

fn send_message<S>(sender: &mut S, message: MessageOutbound)
where
    S: WeylusSender,
{
    if let Err(err) = sender.send_message(message) {
        warn!("Failed to send message to client: {err}");
    }
}

pub struct WeylusClientHandler<S, R, FnUInput> {
    sender: S,
    receiver: Option<R>,
    video_sender: mpsc::Sender<VideoCommands>,
    input_device: Option<Box<dyn InputDevice>>,
    capturables: Vec<Box<dyn Capturable>>,
    on_uinput_inaccessible: FnUInput,
    config: WeylusClientConfig,
    capture_cursor: bool,
    /// generation of the remembered screen share `capturables` came from, 0 if none
    generation: u64,
    client_name: Option<String>,
    video_thread: JoinHandle<()>,
}

#[derive(Clone, Copy)]
pub struct WeylusClientConfig {
    pub encoder_options: EncoderOptions,
    pub wayland_support: bool,
    /// keep the screen share (and its running capture) across connections
    pub remember_screen: bool,
}

impl<S, R, FnUInput> WeylusClientHandler<S, R, FnUInput> {
    pub fn new(
        sender: S,
        receiver: R,
        on_uinput_inaccessible: FnUInput,
        config: WeylusClientConfig,
        report_video: Box<dyn Fn(VideoInfo) + Send>,
    ) -> Self
    where
        R: WeylusReceiver,
        S: WeylusSender + Clone + Send + Sync + 'static,
    {
        let (video_sender, video_receiver) = mpsc::channel::<VideoCommands>();
        let video_thread = {
            let sender = sender.clone();
            // offload creating the videostream to another thread to avoid blocking the thread that
            // is receiving messages from the websocket
            spawn(move || {
                handle_video(
                    video_receiver,
                    sender,
                    config.encoder_options,
                    config.remember_screen,
                    report_video,
                )
            })
        };

        Self {
            sender,
            receiver: Some(receiver),
            video_sender,
            input_device: None,
            capturables: vec![],
            on_uinput_inaccessible,
            config,
            capture_cursor: false,
            generation: 0,
            client_name: None,
            video_thread,
        }
    }

    pub fn run(mut self)
    where
        R: WeylusReceiver,
        S: WeylusSender + Clone + Send + Sync + 'static,
        FnUInput: Fn(),
    {
        for message in self.receiver.take().unwrap() {
            match message {
                Ok(message) => {
                    trace!("Received message: {message:?}");
                    match message {
                        MessageInbound::PointerEvent(event) => self.process_pointer_event(&event),
                        MessageInbound::WheelEvent(event) => self.process_wheel_event(&event),
                        MessageInbound::KeyboardEvent(event) => self.process_keyboard_event(&event),
                        MessageInbound::GetCapturableList => self.send_capturable_list(false),
                        MessageInbound::RefreshCapturableList => self.send_capturable_list(true),
                        MessageInbound::Config(config) => self.update_config(config),
                        MessageInbound::PauseVideo => {
                            self.video_sender.send(VideoCommands::Pause).unwrap()
                        }
                        MessageInbound::ResumeVideo => {
                            self.video_sender.send(VideoCommands::Resume).unwrap()
                        }
                        MessageInbound::RestartVideo => {
                            self.video_sender.send(VideoCommands::Restart).unwrap()
                        }
                        MessageInbound::ClientLog(msg) => info!("Client: {msg}"),
                        MessageInbound::SetCaptureCursor(c) => self.capture_cursor = c,
                        MessageInbound::RequestKeyframe => self
                            .video_sender
                            .send(VideoCommands::RequestKeyframe)
                            .unwrap(),
                        MessageInbound::ChooseCustomInputAreas => {
                            let (sender, receiver) = std::sync::mpsc::channel();
                            crate::desktop::request_input_areas(sender);
                            let mut sender = self.sender.clone();
                            spawn(move || {
                                while let Ok(areas) = receiver.recv() {
                                    send_message(
                                        &mut sender,
                                        MessageOutbound::CustomInputAreas(areas),
                                    );
                                }
                            });
                        }
                    }
                }
                Err(err) => {
                    warn!("Failed to read message {err}!");
                    self.send_message(MessageOutbound::Error(
                        "Failed to read message!".to_string(),
                    ));
                }
            }
        }

        drop(self.video_sender);
        if let Err(err) = self.video_thread.join() {
            warn!("Failed to join video thread: {err:?}");
        }
    }

    fn send_message(&mut self, message: MessageOutbound)
    where
        S: WeylusSender,
    {
        send_message(&mut self.sender, message)
    }

    fn process_wheel_event(&mut self, event: &WheelEvent) {
        match &mut self.input_device {
            Some(i) => i.send_wheel_event(event),
            None => warn!("Input device is not initalized, can not process WheelEvent!"),
        }
    }

    fn process_pointer_event(&mut self, event: &PointerEvent) {
        if let Some(device) = self.input_device.as_mut() {
            device.send_pointer_event(event)
        } else {
            warn!("Input device is not initalized, can not process PointerEvent!");
        }
    }

    fn process_keyboard_event(&mut self, event: &KeyboardEvent) {
        if let Some(device) = self.input_device.as_mut() {
            device.send_keyboard_event(event)
        } else {
            warn!("Input device is not initalized, can not process KeyboardEvent!");
        }
    }

    fn send_capturable_list(&mut self, refresh: bool)
    where
        S: WeylusSender,
    {
        let remember = self.config.remember_screen;
        let cached = if remember && !refresh {
            remembered_capturables(self.capture_cursor)
        } else {
            None
        };
        (self.capturables, self.generation) = match cached {
            Some(cached) => {
                info!("Reusing remembered screen share");
                cached
            }
            None => {
                if remember {
                    forget_screen();
                }
                let capturables = get_capturables(self.config.wayland_support, self.capture_cursor);
                // an empty list is a failed session, don't keep it
                // every portal session gets its own generation: a refreshed session can reuse
                // the stream's node id, so the name alone doesn't tell the sources apart
                let generation = if capturables.is_empty() {
                    0
                } else if remember {
                    remember_screen(&capturables, self.capture_cursor)
                } else {
                    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
                };
                (capturables, generation)
            }
        };
        let windows: Vec<String> = self.capturables.iter().map(|c| c.name()).collect();
        self.send_message(MessageOutbound::CapturableList(windows));
    }

    fn update_config(&mut self, config: ClientConfiguration)
    where
        S: WeylusSender,
        FnUInput: Fn(),
    {
        let client_name_changed = if self.client_name != config.client_name {
            self.client_name = config.client_name;
            true
        } else {
            false
        };
        if config.capturable_id < self.capturables.len() {
            let capturable = self.capturables[config.capturable_id].clone();

            self.capture_cursor = config.capture_cursor;

            if config.uinput_support {
                if self.input_device.is_none() || client_name_changed {
                    let device = crate::input::uinput_device::UInputDevice::new(
                        capturable.clone(),
                        &self.client_name,
                    );
                    match device {
                        Ok(d) => self.input_device = Some(Box::new(d)),
                        Err(e) => {
                            error!("Failed to create uinput device: {}", e);
                            if let CErrorCode::UInputNotAccessible = e.to_enum() {
                                (self.on_uinput_inaccessible)();
                            }
                            self.send_message(MessageOutbound::ConfigError(
                                "Failed to create uinput device!".to_string(),
                            ));
                            return;
                        }
                    }
                } else if let Some(d) = self.input_device.as_mut() {
                    d.set_capturable(capturable.clone());
                }
            } else if self.input_device.take().is_some() {
                info!("Input disabled by the client (uinput off).");
            }

            self.video_sender
                .send(VideoCommands::Start(VideoConfig {
                    capturable,
                    generation: self.generation,
                    capture_cursor: config.capture_cursor,
                    max_width: config.max_width,
                    max_height: config.max_height,
                    frame_rate: config.frame_rate,
                    // NaN stays NaN through clamp and becomes 0 in the cast
                    bitrate_kbps: (config.bitrate_mbps.clamp(0.0, 200.0) * 1000.0).round() as u32,
                    codecs: CodecOrder::from_names(&config.codecs),
                }))
                .unwrap();
        } else {
            error!("Got invalid id for capturable: {}", config.capturable_id);
            self.send_message(MessageOutbound::ConfigError(
                "Invalid id for capturable!".to_string(),
            ));
        }
    }
}

/// Video messages allowed between the encoder and the socket; more means the link is behind, so
/// the tick is skipped before capture (dropping after encoding would break P-frame references).
const MAX_VIDEO_BACKLOG: usize = 2;

const RECORDER_RETRIES: u32 = 3;

fn handle_video<S: WeylusSender + Clone + 'static>(
    receiver: mpsc::Receiver<VideoCommands>,
    mut sender: S,
    base_options: EncoderOptions,
    remember_screen: bool,
    report_video: Box<dyn Fn(VideoInfo) + Send>,
) {
    const EFFECTIVE_INIFINITY: Duration = Duration::from_secs(3600 * 24 * 365 * 200);

    let mut recorder: Option<Box<dyn Recorder>> = None;
    // name of the capturable the recorder was built for
    let mut recorder_source: Option<String> = None;
    // generation of the remembered screen share the recorder belongs to
    let mut recorder_generation = 0;
    let mut video_encoder: Option<Box<VideoEncoder>> = None;

    let mut max_width = 1920;
    let mut max_height = 1080;
    let mut frame_duration = EFFECTIVE_INIFINITY;
    let mut last_frame = Instant::now();
    let mut paused = false;
    let mut encoder_options = base_options;
    let mut fps = 0.0;
    let mut video_info: Option<VideoInfo> = None;

    loop {
        let now = Instant::now();
        let elapsed = now - last_frame;
        let frames_passed = (elapsed.as_secs_f64() / frame_duration.as_secs_f64()) as u32;
        let next_frame = last_frame + (frames_passed + 1) * frame_duration;
        let timeout = next_frame - now;
        last_frame = next_frame;

        if frames_passed > 0 {
            trace!("Dropped {frames_passed} frame(s)!");
        }

        match receiver.recv_timeout(if paused { EFFECTIVE_INIFINITY } else { timeout }) {
            Ok(VideoCommands::Start(config)) => {
                let source = config.capturable.name();
                if recorder.is_some()
                    && recorder_source.as_ref() == Some(&source)
                    && recorder_generation == config.generation
                {
                    // rebuilding a PipeWire recorder for the same stream yields no buffers, so
                    // settings that don't change the source keep the running one. capture_cursor
                    // isn't one of them: PipeWire fixes the cursor mode when the portal session
                    // starts and no recorder reads it.
                    max_width = config.max_width;
                    max_height = config.max_height;
                    send_message(&mut sender, MessageOutbound::ConfigOk);
                } else {
                    #[allow(unused_assignments)]
                    {
                        // gstpipewire can not handle setting a pipeline's state to Null after another
                        // pipeline has been created and its state has been set to Play.
                        // This line makes sure that there always is only a single recorder and thus
                        // single pipeline in this thread by forcing rust to call the destructor of the
                        // current pipeline here, right before creating a new pipeline.
                        // See: https://gitlab.freedesktop.org/pipewire/pipewire/-/issues/986
                        //
                        // This shouldn't affect other Recorder trait objects.
                        recorder = None;
                        recorder_source = None;
                    }
                    let parked = if remember_screen && config.generation != 0 {
                        take_parked_recorder(config.generation, &source)
                    } else {
                        None
                    };
                    let new_recorder = match parked {
                        Some(r) => {
                            info!("Reusing running screen capture: {source}");
                            Ok(r)
                        }
                        None => {
                            // right after the previous pipeline was torn down gstpipewire can fail
                            // with "Element failed to change its state"; it recovers within a moment
                            let mut res = config.capturable.recorder(config.capture_cursor);
                            for attempt in 1..=RECORDER_RETRIES {
                                let Err(err) = &res else { break };
                                warn!(
                                    "Screen capture failed to start ({err}), retry {attempt}/{RECORDER_RETRIES}."
                                );
                                std::thread::sleep(Duration::from_millis(300));
                                res = config.capturable.recorder(config.capture_cursor);
                            }
                            res.inspect(|_| {
                                info!("Screen capture started: {source}");
                            })
                        }
                    };
                    match new_recorder {
                        Ok(r) => {
                            recorder = Some(r);
                            recorder_source = Some(source);
                            recorder_generation = config.generation;
                            max_width = config.max_width;
                            max_height = config.max_height;
                            send_message(&mut sender, MessageOutbound::ConfigOk);
                        }
                        Err(err) => {
                            warn!("Failed to init screen cast: {}!", err);
                            send_message(
                                &mut sender,
                                MessageOutbound::Error("Failed to init screen cast!".into()),
                            )
                        }
                    }
                }
                last_frame = Instant::now();

                // The Duration type can not handle infinity, if the frame rate is set to 0 we just
                // set the duration between two frames to a very long one, which is effectively
                // infinity.
                let d = 1.0 / config.frame_rate;
                frame_duration = if d.is_finite() {
                    Duration::from_secs_f64(d)
                } else {
                    EFFECTIVE_INIFINITY
                };
                frame_duration = frame_duration.min(EFFECTIVE_INIFINITY);
                fps = config.frame_rate;
                if let Some(info) = video_info.as_mut() {
                    info.fps = fps;
                    report_video(info.clone());
                }

                let options = EncoderOptions {
                    codecs: config.codecs,
                    bitrate_kbps: config.bitrate_kbps,
                    frame_rate: if config.bitrate_kbps > 0 {
                        config.frame_rate.round().clamp(1.0, 240.0) as u32
                    } else {
                        0
                    },
                    ..base_options
                };
                if options != encoder_options {
                    encoder_options = options;
                    // rate control is fixed at open time
                    video_encoder = None;
                }
            }
            Ok(VideoCommands::Pause) => {
                paused = true;
            }
            Ok(VideoCommands::Resume) => {
                paused = false;
            }
            Ok(VideoCommands::Restart) => {
                video_encoder = None;
            }
            Ok(VideoCommands::RequestKeyframe) => {
                if let Some(encoder) = video_encoder.as_mut() {
                    encoder.request_keyframe();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if sender.video_backlog() >= MAX_VIDEO_BACKLOG {
                    trace!("Video backlog, skipping frame.");
                    continue;
                }
                if recorder.is_none() {
                    warn!("Screen capture not initalized, can not send video frame!");
                    continue;
                }
                let pixel_data = recorder.as_mut().unwrap().capture();
                if let Err(err) = pixel_data {
                    warn!("Error capturing screen: {}", err);
                    continue;
                }
                let pixel_data = pixel_data.unwrap();
                let (width_in, height_in) = pixel_data.size();
                let scale =
                    (max_width as f64 / width_in as f64).min(max_height as f64 / height_in as f64);
                // limit video to 4K
                let scale_max = (3840.0 / width_in as f64).min(2160.0 / height_in as f64);
                let scale = scale.min(scale_max);
                let mut width_out = width_in;
                let mut height_out = height_in;
                if scale < 1.0 {
                    width_out = (width_out as f64 * scale) as usize;
                    height_out = (height_out as f64 * scale) as usize;
                }
                // video encoder is not setup or setup for encoding the wrong size: restart it
                if video_encoder.is_none()
                    || !video_encoder
                        .as_ref()
                        .unwrap()
                        .check_size(width_in, height_in, width_out, height_out)
                {
                    send_message(&mut sender, MessageOutbound::NewVideo);
                    let mut sender = sender.clone();
                    let res = VideoEncoder::new(
                        width_in,
                        height_in,
                        width_out,
                        height_out,
                        move |data| {
                            if let Err(err) = sender.send_video(data) {
                                warn!("Failed to send video frame: {err}!");
                            }
                        },
                        encoder_options,
                    );
                    match res {
                        Ok(r) => {
                            let info = VideoInfo {
                                encoder: r.codec_name().unwrap_or_default(),
                                width: width_out,
                                height: height_out,
                                fps,
                            };
                            report_video(info.clone());
                            video_info = Some(info);
                            video_encoder = Some(r);
                        }
                        Err(e) => {
                            warn!("{}", e);
                            continue;
                        }
                    };
                }
                let video_encoder = video_encoder.as_mut().unwrap();
                video_encoder.encode(pixel_data);
            }
            // stop thread once the channel is closed
            Err(RecvTimeoutError::Disconnected) => {
                if remember_screen
                    && recorder_generation != 0
                    && let (Some(recorder), Some(source)) = (recorder, recorder_source)
                {
                    park_recorder(recorder_generation, source, recorder);
                }
                return;
            }
        };
    }
}

pub struct WsWeylusReceiver {
    recv: tokio::sync::mpsc::Receiver<MessageInbound>,
}

impl Iterator for WsWeylusReceiver {
    type Item = Result<MessageInbound, Infallible>;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv.blocking_recv().map(Ok)
    }
}

impl WeylusReceiver for WsWeylusReceiver {
    type Error = Infallible;
}

pub enum WsMessage {
    Frame(Frame<'static>),
    Video(Vec<u8>),
    MessageOutbound(MessageOutbound),
}

unsafe impl Send for WsMessage {}

#[derive(Clone)]
pub struct WsWeylusSender {
    sender: tokio::sync::mpsc::Sender<WsMessage>,
    video_backlog: Arc<AtomicUsize>,
}

impl WeylusSender for WsWeylusSender {
    type Error = tokio::sync::mpsc::error::SendError<WsMessage>;

    fn send_message(&mut self, message: MessageOutbound) -> Result<(), Self::Error> {
        self.sender
            .blocking_send(WsMessage::MessageOutbound(message))
    }

    fn send_video(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.video_backlog.fetch_add(1, Ordering::Relaxed);
        let res = self.sender.blocking_send(WsMessage::Video(bytes.to_vec()));
        if res.is_err() {
            self.video_backlog.fetch_sub(1, Ordering::Relaxed);
        }
        res
    }

    fn video_backlog(&self) -> usize {
        self.video_backlog.load(Ordering::Relaxed)
    }
}

pub fn weylus_websocket_channel(
    websocket: WebSocket<TokioIo<Upgraded>>,
    semaphore_shutdown: Arc<tokio::sync::Semaphore>,
    disconnect: Arc<tokio::sync::Notify>,
) -> (WsWeylusSender, WsWeylusReceiver) {
    let (rx, mut tx) = websocket.split(tokio::io::split);

    let mut rx = FragmentCollectorRead::new(rx);

    let (sender_inbound, receiver_inbound) = channel::<MessageInbound>(32);
    let (sender_outbound, mut receiver_outbound) = channel::<WsMessage>(32);
    let video_backlog = Arc::new(AtomicUsize::new(0));

    {
        let sender_outbound = sender_outbound.clone();
        tokio::spawn(async move {
            let mut send_fn = |frame| async {
                if let Err(err) = sender_outbound.send(WsMessage::Frame(frame)).await {
                    warn!("Failed to send websocket frame while receiving fragmented frame: {err}.")
                };
                Ok(())
            };

            loop {
                let fut = rx.read_frame::<_, WebSocketError>(&mut send_fn);

                let frame = tokio::select! {
                    _ = semaphore_shutdown.acquire() => break,
                    _ = disconnect.notified() => break,
                    frame = fut => match frame {
                        Ok(frame) => frame,
                        Err(err) => {
                            warn!("Invalid websocket frame: {err}.");
                            break;
                        },
                    },
                };
                match frame.opcode {
                    OpCode::Close => break,
                    OpCode::Text => match serde_json::from_slice(&frame.payload) {
                        Ok(msg) => {
                            if let Err(err) = sender_inbound.send(msg).await {
                                warn!(
                                    "Failed to forward inbound message to WeylusClientHandler: {err}."
                                );
                            }
                        }
                        Err(err) => warn!("Failed to parse message: {err}"),
                    },
                    _ => {}
                }
            }
        });
    }

    let backlog = video_backlog.clone();
    tokio::spawn(async move {
        loop {
            let msg = if let Some(msg) = receiver_outbound.recv().await {
                msg
            } else {
                break;
            };

            match msg {
                WsMessage::Frame(frame) => {
                    if let Err(err) = tx.write_frame(frame).await {
                        if let WebSocketError::ConnectionClosed = err {
                            break;
                        }
                        warn!("Failed to send frame: {err}");
                    }
                }
                WsMessage::Video(data) => {
                    let res = tx.write_frame(Frame::binary(data.into())).await;
                    backlog.fetch_sub(1, Ordering::Relaxed);
                    if let Err(err) = res {
                        if let WebSocketError::ConnectionClosed = err {
                            break;
                        }
                        warn!("Failed to send video frame: {err}");
                    }
                }
                WsMessage::MessageOutbound(msg) => {
                    let json_string = serde_json::to_string(&msg).unwrap();
                    let data = json_string.as_bytes();
                    if let Err(err) = tx.write_frame(Frame::text(data.into())).await {
                        if let WebSocketError::ConnectionClosed = err {
                            break;
                        }
                        warn!("Failed to send outbound message: {err}");
                    }
                }
            }
        }
    });

    (
        WsWeylusSender {
            sender: sender_outbound,
            video_backlog,
        },
        WsWeylusReceiver {
            recv: receiver_inbound,
        },
    )
}
