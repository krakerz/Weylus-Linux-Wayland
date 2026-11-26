use serde::{Deserialize, Deserializer, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientConfiguration {
    pub uinput_support: bool,
    pub capturable_id: usize,
    pub capture_cursor: bool,
    pub max_width: usize,
    pub max_height: usize,
    pub client_name: Option<String>,
    pub frame_rate: f64,
    /// target video bitrate; 0 = encoder's constant-quality default
    #[serde(default)]
    pub bitrate_mbps: f64,
    /// video codecs the client wants, best first (`h264`, `hevc`, `av1`); unknown names are ignored
    #[serde(default)]
    pub codecs: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum MessageInbound {
    PointerEvent(PointerEvent),
    WheelEvent(WheelEvent),
    KeyboardEvent(KeyboardEvent),
    /// Reply with the remembered screen share if there is one, else open a new portal session.
    GetCapturableList,
    /// Always open a new portal session (the screen picker shows again).
    RefreshCapturableList,
    Config(ClientConfiguration),
    PauseVideo,
    ResumeVideo,
    RestartVideo,
    RequestKeyframe,
    ChooseCustomInputAreas,
    ClientLog(String),
    /// sent before GetCapturableList/RefreshCapturableList: PipeWire fixes the cursor mode when the portal session starts
    SetCaptureCursor(bool),
    /// client clock in unix ms; answered at once with `Pong` (clock offset and RTT for the stats overlay)
    Ping(f64),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum MessageOutbound {
    CapturableList(Vec<String>),
    NewVideo,
    ConfigOk,
    CustomInputAreas(CustomInputAreas),
    ConfigError(String),
    Error(String),
    Pong {
        client_ms: f64,
        server_ms: f64,
    },
    /// unix ms of pts 0 of the encoder that sends the following video messages
    VideoEpoch(f64),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
pub struct CustomInputAreas {
    pub mouse: Option<Rect>,
    pub touch: Option<Rect>,
    pub pen: Option<Rect>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum PointerType {
    #[serde(rename = "")]
    Unknown,
    #[serde(rename = "mouse")]
    Mouse,
    #[serde(rename = "pen")]
    Pen,
    #[serde(rename = "touch")]
    Touch,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum PointerEventType {
    #[serde(rename = "pointerdown")]
    Down,
    #[serde(rename = "pointerup")]
    Up,
    #[serde(rename = "pointercancel")]
    Cancel,
    #[serde(rename = "pointermove")]
    Move,
    #[serde(rename = "pointerover")]
    Over,
    #[serde(rename = "pointerenter")]
    Enter,
    #[serde(rename = "pointerleave")]
    Leave,
    #[serde(rename = "pointerout")]
    Out,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum KeyboardEventType {
    #[serde(rename = "down")]
    Down,
    #[serde(rename = "up")]
    Up,
    #[serde(rename = "repeat")]
    Repeat,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "UPPERCASE")]
pub enum KeyboardLocation {
    Standard,
    Left,
    Right,
    Numpad,
}

fn location_from<'de, D: Deserializer<'de>>(deserializer: D) -> Result<KeyboardLocation, D::Error> {
    let code: u8 = Deserialize::deserialize(deserializer)?;
    match code {
        0 => Ok(KeyboardLocation::Standard),
        1 => Ok(KeyboardLocation::Left),
        2 => Ok(KeyboardLocation::Right),
        3 => Ok(KeyboardLocation::Numpad),
        _ => Err(serde::de::Error::custom(
            "Failed to parse keyboard location code.",
        )),
    }
}

bitflags! {
    #[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Copy)]
    pub struct Button: u8 {
        const NONE = 0b0000_0000;
        const PRIMARY = 0b0000_0001;
        const SECONDARY = 0b0000_0010;
        const AUXILARY = 0b0000_0100;
        const FOURTH = 0b0000_1000;
        const FIFTH = 0b0001_0000;
        const ERASER = 0b0010_0000;
    }
}

fn button_from<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Button, D::Error> {
    let bits: u8 = Deserialize::deserialize(deserializer)?;
    Button::from_bits(bits).ok_or(serde::de::Error::custom("Failed to parse button code."))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct KeyboardEvent {
    pub event_type: KeyboardEventType,
    pub code: String,
    pub key: String,
    #[serde(deserialize_with = "location_from")]
    pub location: KeyboardLocation,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PointerEvent {
    pub event_type: PointerEventType,
    pub pointer_id: i64,
    pub timestamp: u64,
    pub is_primary: bool,
    pub pointer_type: PointerType,
    #[serde(deserialize_with = "button_from")]
    pub button: Button,
    #[serde(deserialize_with = "button_from")]
    pub buttons: Button,
    pub x: f64,
    pub y: f64,
    // pub movement_x: f64,
    // pub movement_y: f64,
    pub pressure: f64,
    pub tilt_x: i32,
    pub tilt_y: i32,
    pub twist: i32,
    pub width: f64,
    pub height: f64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct WheelEvent {
    pub dx: i32,
    pub dy: i32,
    pub timestamp: u64,
}

pub trait WeylusSender {
    type Error: std::error::Error;
    fn send_message(&mut self, message: MessageOutbound) -> Result<(), Self::Error>;
    fn send_video(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    /// Video messages handed to `send_video` that haven't been written to the socket yet.
    fn video_backlog(&self) -> usize;
}

pub trait WeylusReceiver: Iterator<Item = Result<MessageInbound, Self::Error>> {
    type Error: std::error::Error;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_parses() {
        match serde_json::from_str::<MessageInbound>(r#"{"Ping":1234.5}"#).unwrap() {
            MessageInbound::Ping(v) => assert_eq!(v, 1234.5),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn pong_and_epoch_json_shape() {
        let pong = MessageOutbound::Pong {
            client_ms: 1.0,
            server_ms: 2.0,
        };
        assert_eq!(
            serde_json::to_string(&pong).unwrap(),
            r#"{"Pong":{"client_ms":1.0,"server_ms":2.0}}"#
        );
        assert_eq!(
            serde_json::to_string(&MessageOutbound::VideoEpoch(5.0)).unwrap(),
            r#"{"VideoEpoch":5.0}"#
        );
    }
}
