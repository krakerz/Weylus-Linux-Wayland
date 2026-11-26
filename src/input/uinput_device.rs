use std::cmp::Ordering;
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::time::{Duration, Instant};

use crate::capturable::{Capturable, Geometry};
use crate::input::device::InputDevice;
use crate::protocol::{
    Button, KeyboardEvent, KeyboardEventType, KeyboardLocation, PointerEvent, PointerEventType,
    PointerType, Rect, WheelEvent,
};

use crate::cerror::CError;

use tracing::{debug, warn};

unsafe extern "C" {
    fn init_uinput_keyboard(name: *const c_char, err: *mut CError) -> c_int;
    fn init_uinput_stylus(name: *const c_char, err: *mut CError) -> c_int;
    fn init_uinput_mouse(name: *const c_char, err: *mut CError) -> c_int;
    fn init_uinput_touch(name: *const c_char, err: *mut CError) -> c_int;
    fn destroy_uinput_device(fd: c_int);
    fn send_uinput_event(device: c_int, typ: c_int, code: c_int, value: c_int, err: *mut CError);
}

struct MultiTouch {
    id: i64,
}

pub struct UInputDevice {
    keyboard_fd: c_int,
    stylus_fd: c_int,
    mouse_fd: c_int,
    touch_fd: c_int,
    touches: [Option<MultiTouch>; 5],
    tool_pen_active: bool,
    pen_touching: bool,
    last_pen_event: Instant,
    capturable: Box<dyn Capturable>,
    geometry: Rect,
}

impl UInputDevice {
    pub fn new(capturable: Box<dyn Capturable>, id: &Option<String>) -> Result<Self, Box<CError>> {
        let mut suffix = String::new();
        if let Some(id) = id {
            suffix = format!(" - {}", id);
        }
        let mut err = CError::new();
        let name_stylus = format!("Weylus Stylus{}", suffix);
        let name_stylus_c_str = CString::new(name_stylus.as_bytes()).unwrap();
        let stylus_fd = unsafe { init_uinput_stylus(name_stylus_c_str.as_ptr(), &mut err) };
        if err.is_err() {
            return Err(Box::new(err));
        }

        let name_mouse = format!("Weylus Mouse{}", suffix);
        let name_mouse_c_str = CString::new(name_mouse.as_bytes()).unwrap();
        let mouse_fd = unsafe { init_uinput_mouse(name_mouse_c_str.as_ptr(), &mut err) };
        if err.is_err() {
            unsafe { destroy_uinput_device(stylus_fd) };
            return Err(Box::new(err));
        }

        let name_touch = format!("Weylus Touch{}", suffix);
        let name_touch_c_str = CString::new(name_touch.as_bytes()).unwrap();
        let touch_fd = unsafe { init_uinput_touch(name_touch_c_str.as_ptr(), &mut err) };
        if err.is_err() {
            unsafe {
                destroy_uinput_device(stylus_fd);
                destroy_uinput_device(mouse_fd);
            }
            return Err(Box::new(err));
        }

        let name_keyboard = format!("Weylus Keyboard{}", suffix);
        let name_keyboard_c_str = CString::new(name_keyboard.as_bytes()).unwrap();
        let keyboard_fd = unsafe { init_uinput_keyboard(name_keyboard_c_str.as_ptr(), &mut err) };
        if err.is_err() {
            unsafe {
                destroy_uinput_device(stylus_fd);
                destroy_uinput_device(mouse_fd);
                destroy_uinput_device(touch_fd);
            }
            return Err(Box::new(err));
        }

        Ok(Self {
            keyboard_fd,
            stylus_fd,
            mouse_fd,
            touch_fd,
            touches: Default::default(),
            tool_pen_active: false,
            pen_touching: false,
            last_pen_event: Instant::now(),
            capturable,
            geometry: Rect::default(),
        })
    }

    fn transform_x(&self, x: f64) -> i32 {
        let x = (x * self.geometry.w + self.geometry.x) * ABS_MAX;
        x as i32
    }

    fn transform_y(&self, y: f64) -> i32 {
        let y = (y * self.geometry.h + self.geometry.y) * ABS_MAX;
        y as i32
    }

    fn transform_pressure(&self, p: f64) -> i32 {
        (p * ABS_MAX) as i32
    }

    fn transform_touch_size(&self, s: f64) -> i32 {
        (s * ABS_MAX) as i32
    }

    fn find_slot(&self, id: i64) -> Option<usize> {
        self.touches
            .iter()
            .enumerate()
            .find_map(|(slot, mt)| match mt {
                Some(mt) => {
                    if mt.id == id {
                        Some(slot)
                    } else {
                        None
                    }
                }
                _ => None,
            })
    }

    /// One complete hover frame (tool in proximity, tip up) at the event's position.
    fn send_pen_hover_frame(&mut self, event: &PointerEvent) {
        if !self.tool_pen_active && !event.buttons.contains(Button::ERASER) {
            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_PEN, 1);
            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_RUBBER, 0);
            self.tool_pen_active = true;
        }
        self.send(
            self.stylus_fd,
            ET_ABSOLUTE,
            EC_ABSOLUTE_X,
            self.transform_x(event.x),
        );
        self.send(
            self.stylus_fd,
            ET_ABSOLUTE,
            EC_ABSOLUTE_Y,
            self.transform_y(event.y),
        );
        self.send(self.stylus_fd, ET_ABSOLUTE, EC_ABSOLUTE_PRESSURE, 0);
        self.send(
            self.stylus_fd,
            ET_ABSOLUTE,
            EC_ABSOLUTE_TILT_X,
            event.tilt_x,
        );
        self.send(
            self.stylus_fd,
            ET_ABSOLUTE,
            EC_ABSOLUTE_TILT_Y,
            event.tilt_y,
        );
        self.send(self.stylus_fd, ET_MSC, EC_MSC_SERIAL, STYLUS_SERIAL);
        self.send(self.stylus_fd, ET_SYNC, EC_SYNC_REPORT, 0);
    }

    fn send(&self, fd: c_int, typ: c_int, code: c_int, value: c_int) {
        let mut err = CError::new();
        unsafe {
            send_uinput_event(fd, typ, code, value, &mut err);
        }
        if err.is_err() {
            warn!("{}", err);
        }
    }
}

impl Drop for UInputDevice {
    fn drop(&mut self) {
        unsafe {
            destroy_uinput_device(self.keyboard_fd);
            destroy_uinput_device(self.stylus_fd);
            destroy_uinput_device(self.mouse_fd);
            destroy_uinput_device(self.touch_fd);
        };
    }
}

// Event Types
const ET_SYNC: c_int = 0x00;
const ET_KEY: c_int = 0x01;
const ET_RELATIVE: c_int = 0x02;
const ET_ABSOLUTE: c_int = 0x03;
const ET_MSC: c_int = 0x04;

// Event Codes
const EC_SYNC_REPORT: c_int = 0;

const EC_KEY_MOUSE_LEFT: c_int = 0x110;
const EC_KEY_MOUSE_RIGHT: c_int = 0x111;
const EC_KEY_MOUSE_MIDDLE: c_int = 0x112;
const EC_KEY_TOOL_PEN: c_int = 0x140;
const EC_KEY_TOOL_RUBBER: c_int = 0x141;
const EC_KEY_TOUCH: c_int = 0x14a;
const EC_KEY_TOOL_FINGER: c_int = 0x145;
const EC_KEY_TOOL_DOUBLETAP: c_int = 0x14d;
const EC_KEY_TOOL_TRIPLETAP: c_int = 0x14e;
const EC_KEY_TOOL_QUADTAP: c_int = 0x14f; /* Four fingers on trackpad */
const EC_KEY_TOOL_QUINTTAP: c_int = 0x148; /* Five fingers on trackpad */
//const EC_RELATIVE_X: c_int = 0x00;
//const EC_RELATIVE_Y: c_int = 0x01;

const EC_REL_HWHEEL: c_int = 0x06;
const EC_REL_WHEEL: c_int = 0x08;
const EC_REL_WHEEL_HI_RES: c_int = 0x0b;
const EC_REL_HWHEEL_HI_RES: c_int = 0x0c;

const EC_ABSOLUTE_X: c_int = 0x00;
const EC_ABSOLUTE_Y: c_int = 0x01;
const EC_ABSOLUTE_PRESSURE: c_int = 0x18;
const EC_ABSOLUTE_TILT_X: c_int = 0x1a;
const EC_ABSOLUTE_TILT_Y: c_int = 0x1b;
const EC_ABS_MT_SLOT: c_int = 0x2f; /* MT slot being modified */
const EC_ABS_MT_TOUCH_MAJOR: c_int = 0x30; /* Major axis of touching ellipse */
const EC_ABS_MT_TOUCH_MINOR: c_int = 0x31; /* Minor axis (omit if circular) */
const EC_ABS_MT_ORIENTATION: c_int = 0x34; /* Ellipse orientation */
const EC_ABS_MT_POSITION_X: c_int = 0x35; /* Center X touch position */
const EC_ABS_MT_POSITION_Y: c_int = 0x36; /* Center Y touch position */
const EC_ABS_MT_TRACKING_ID: c_int = 0x39; /* Unique ID of initiated contact */
const EC_ABS_MT_PRESSURE: c_int = 0x3a; /* Pressure on contact area */

const EC_MSC_TIMESTAMP: c_int = 0x05;
const EC_MSC_SERIAL: c_int = 0x00;
// Any fixed non-zero value: makes libinput see one persistent (unique) pen across strokes.
const STYLUS_SERIAL: c_int = 0x5745;

/// Keep the pen in proximity between strokes and hover to the next stroke's start, like a real
/// pen. A prox-out/prox-in on every lift makes Qt apps on Hyprland join strokes with straight
/// lines. `WEYLUS_PEN_LINGER=0` restores the old behaviour.
fn pen_linger() -> bool {
    static LINGER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LINGER.get_or_init(|| std::env::var("WEYLUS_PEN_LINGER").map_or(true, |v| v != "0"))
}

// This is choosen somewhat arbitrarily
// describes maximum value for ABS_X, ABS_Y, ABS_...
// This corresponds to PointerEvent values of 1.0
const ABS_MAX: f64 = 65535.0;

impl InputDevice for UInputDevice {
    fn send_wheel_event(&mut self, event: &WheelEvent) {
        if let Err(err) = self.capturable.before_input() {
            warn!("Failed to activate window, sending no input ({})", err);
            return;
        }

        fn direction(d: i32) -> i32 {
            match d.cmp(&0) {
                Ordering::Equal => 0,
                Ordering::Less => -1,
                Ordering::Greater => 1,
            }
        }

        self.send(
            self.mouse_fd,
            ET_RELATIVE,
            EC_REL_WHEEL,
            direction(event.dy),
        );
        self.send(
            self.mouse_fd,
            ET_RELATIVE,
            EC_REL_HWHEEL,
            direction(event.dx),
        );
        self.send(self.mouse_fd, ET_RELATIVE, EC_REL_WHEEL_HI_RES, event.dy);
        self.send(self.mouse_fd, ET_RELATIVE, EC_REL_HWHEEL_HI_RES, event.dx);

        self.send(
            self.mouse_fd,
            ET_MSC,
            EC_MSC_TIMESTAMP,
            (event.timestamp % (i32::MAX as u64 + 1)) as i32,
        );
        self.send(self.mouse_fd, ET_SYNC, EC_SYNC_REPORT, 0);
    }

    fn send_pointer_event(&mut self, event: &PointerEvent) {
        if let Err(err) = self.capturable.before_input() {
            warn!("Failed to activate window, sending no input ({})", err);
            return;
        }
        let (x, y, width, height) = match self.capturable.geometry().unwrap() {
            Geometry::Relative(x, y, width, height) => (x, y, width, height),
        };
        self.geometry.x = x;
        self.geometry.y = y;
        self.geometry.w = width;
        self.geometry.h = height;
        match event.pointer_type {
            PointerType::Touch => {
                // This is a workaround for browsers that send events when the pen is hovering but
                // do not send an event when the pen leaves the hovering range. If the pen is left
                // in this state touch rejection may stay active and touch won't work.
                // Therefore, we manually remove the pen after a short delay.
                if self.tool_pen_active
                    && !self.pen_touching
                    && (Instant::now() - self.last_pen_event) > Duration::from_millis(50)
                {
                    self.tool_pen_active = false;
                    self.send(self.stylus_fd, ET_KEY, EC_KEY_TOUCH, 0);
                    self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_PEN, 0);
                    self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_RUBBER, 0);
                    self.send(self.stylus_fd, ET_ABSOLUTE, EC_ABSOLUTE_PRESSURE, 0);
                    self.send(
                        self.stylus_fd,
                        ET_MSC,
                        EC_MSC_TIMESTAMP,
                        (event.timestamp % (i32::MAX as u64 + 1)) as i32,
                    );
                    self.send(self.stylus_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                }
                match event.event_type {
                    PointerEventType::Down
                    | PointerEventType::Move
                    | PointerEventType::Over
                    | PointerEventType::Enter => {
                        let slot: usize;
                        // check if this event is already assigned to one of our 10 multitouch slots
                        if let Some(s) = self.find_slot(event.pointer_id) {
                            slot = s;
                        } else {
                            // this event is not assigned to a slot, lets try to do so now
                            // find the first unused slot
                            if let Some(s) =
                                self.touches
                                    .iter()
                                    .enumerate()
                                    .find_map(|(slot, mt)| match mt {
                                        None => Some(slot),
                                        Some(_) => None,
                                    })
                            {
                                slot = s;
                                self.touches[slot] = Some(MultiTouch {
                                    id: event.pointer_id,
                                })
                            } else {
                                // out of slots, do nothing
                                return;
                            }
                        };
                        self.send(self.touch_fd, ET_ABSOLUTE, EC_ABS_MT_SLOT, slot as i32);
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABS_MT_TRACKING_ID,
                            slot as i32,
                        );

                        if let PointerEventType::Down = event.event_type {
                            self.send(self.touch_fd, ET_KEY, EC_KEY_TOUCH, 1);
                            match slot {
                                1 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_FINGER, 0),
                                2 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_DOUBLETAP, 0),
                                3 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_TRIPLETAP, 0),
                                4 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_QUADTAP, 0),
                                _ => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_QUINTTAP, 0),
                            }
                            match slot {
                                1 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_DOUBLETAP, 1),
                                2 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_TRIPLETAP, 1),
                                3 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_QUADTAP, 1),
                                4 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_QUINTTAP, 1),
                                _ => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_FINGER, 1),
                            }
                        }
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABS_MT_PRESSURE,
                            self.transform_pressure(event.pressure),
                        );
                        let major: i32;
                        let minor: i32;
                        let orientation = if event.height >= event.width {
                            major = self.transform_touch_size(event.height);
                            minor = self.transform_touch_size(event.width);
                            0
                        } else {
                            major = self.transform_touch_size(event.width);
                            minor = self.transform_touch_size(event.height);
                            1
                        };
                        self.send(self.touch_fd, ET_ABSOLUTE, EC_ABS_MT_TOUCH_MAJOR, major);
                        self.send(self.touch_fd, ET_ABSOLUTE, EC_ABS_MT_TOUCH_MINOR, minor);
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABS_MT_ORIENTATION,
                            orientation,
                        );
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABS_MT_POSITION_X,
                            self.transform_x(event.x),
                        );
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABS_MT_POSITION_Y,
                            self.transform_y(event.y),
                        );
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_X,
                            self.transform_x(event.x),
                        );
                        self.send(
                            self.touch_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_Y,
                            self.transform_y(event.y),
                        );
                        self.send(
                            self.touch_fd,
                            ET_MSC,
                            EC_MSC_TIMESTAMP,
                            (event.timestamp % (i32::MAX as u64 + 1)) as i32,
                        );
                        self.send(self.touch_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                    }
                    PointerEventType::Cancel
                    | PointerEventType::Up
                    | PointerEventType::Leave
                    | PointerEventType::Out => {
                        // remove from slot
                        if let Some(slot) = self.find_slot(event.pointer_id) {
                            self.send(self.touch_fd, ET_ABSOLUTE, EC_ABS_MT_SLOT, slot as i32);
                            self.send(self.touch_fd, ET_ABSOLUTE, EC_ABS_MT_TRACKING_ID, -1);
                            self.send(self.touch_fd, ET_KEY, EC_KEY_TOUCH, 0);
                            self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_FINGER, 0);
                            match slot {
                                1 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_DOUBLETAP, 0),
                                2 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_TRIPLETAP, 0),
                                3 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_QUADTAP, 0),
                                4 => self.send(self.touch_fd, ET_KEY, EC_KEY_TOOL_QUINTTAP, 0),
                                _ => (),
                            }
                            self.send(
                                self.touch_fd,
                                ET_MSC,
                                EC_MSC_TIMESTAMP,
                                (event.timestamp % (i32::MAX as u64 + 1)) as i32,
                            );
                            self.send(self.touch_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                            self.touches[slot] = None;
                        }
                    }
                };
            }
            PointerType::Pen => {
                self.last_pen_event = Instant::now();
                match event.event_type {
                    PointerEventType::Down
                    | PointerEventType::Move
                    | PointerEventType::Over
                    | PointerEventType::Enter => {
                        if let PointerEventType::Down = event.event_type {
                            if pen_linger() {
                                self.send_pen_hover_frame(event);
                            }
                            self.pen_touching = true;
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOUCH, 1);
                        }
                        if !self.tool_pen_active && !event.buttons.contains(Button::ERASER) {
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_PEN, 1);
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_RUBBER, 0);
                            self.tool_pen_active = true;
                        }
                        if let Button::ERASER = event.button {
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_PEN, 0);
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_RUBBER, 1);
                            self.tool_pen_active = false;
                        }
                        self.send(
                            self.stylus_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_X,
                            self.transform_x(event.x),
                        );
                        self.send(
                            self.stylus_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_Y,
                            self.transform_y(event.y),
                        );
                        self.send(
                            self.stylus_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_PRESSURE,
                            if self.pen_touching {
                                self.transform_pressure(event.pressure)
                            } else {
                                0
                            },
                        );
                        self.send(
                            self.stylus_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_TILT_X,
                            event.tilt_x,
                        );
                        self.send(
                            self.stylus_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_TILT_Y,
                            event.tilt_y,
                        );
                    }
                    PointerEventType::Up
                    | PointerEventType::Cancel
                    | PointerEventType::Leave
                    | PointerEventType::Out => {
                        let lift_only = pen_linger()
                            && matches!(
                                event.event_type,
                                PointerEventType::Up | PointerEventType::Cancel
                            );
                        if pen_linger() && !lift_only {
                            // the pen stays in proximity: no prox-out between strokes
                            return;
                        }
                        self.send(self.stylus_fd, ET_KEY, EC_KEY_TOUCH, 0);
                        self.send(self.stylus_fd, ET_ABSOLUTE, EC_ABSOLUTE_PRESSURE, 0);
                        if !lift_only {
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_PEN, 0);
                            self.send(self.stylus_fd, ET_KEY, EC_KEY_TOOL_RUBBER, 0);
                            self.tool_pen_active = false;
                        }
                        self.pen_touching = false;
                    }
                }
                self.send(self.stylus_fd, ET_MSC, EC_MSC_SERIAL, STYLUS_SERIAL);
                self.send(
                    self.stylus_fd,
                    ET_MSC,
                    EC_MSC_TIMESTAMP,
                    (event.timestamp % (i32::MAX as u64 + 1)) as i32,
                );
                self.send(self.stylus_fd, ET_SYNC, EC_SYNC_REPORT, 0);
            }
            PointerType::Mouse | PointerType::Unknown => {
                match event.event_type {
                    PointerEventType::Down
                    | PointerEventType::Move
                    | PointerEventType::Over
                    | PointerEventType::Enter => {
                        if let PointerEventType::Down = event.event_type {
                            match event.button {
                                Button::PRIMARY => {
                                    self.send(self.mouse_fd, ET_KEY, EC_KEY_MOUSE_LEFT, 1)
                                }
                                Button::SECONDARY => {
                                    self.send(self.mouse_fd, ET_KEY, EC_KEY_MOUSE_RIGHT, 1)
                                }
                                Button::AUXILARY => {
                                    self.send(self.mouse_fd, ET_KEY, EC_KEY_MOUSE_MIDDLE, 1)
                                }
                                _ => (),
                            }
                        }
                        self.send(
                            self.mouse_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_X,
                            self.transform_x(event.x),
                        );
                        self.send(
                            self.mouse_fd,
                            ET_ABSOLUTE,
                            EC_ABSOLUTE_Y,
                            self.transform_y(event.y),
                        );
                    }
                    PointerEventType::Up
                    | PointerEventType::Cancel
                    | PointerEventType::Leave
                    | PointerEventType::Out => match event.button {
                        Button::PRIMARY => self.send(self.mouse_fd, ET_KEY, EC_KEY_MOUSE_LEFT, 0),
                        Button::SECONDARY => {
                            self.send(self.mouse_fd, ET_KEY, EC_KEY_MOUSE_RIGHT, 0)
                        }
                        Button::AUXILARY => {
                            self.send(self.mouse_fd, ET_KEY, EC_KEY_MOUSE_MIDDLE, 0)
                        }
                        _ => (),
                    },
                }
                self.send(
                    self.mouse_fd,
                    ET_MSC,
                    EC_MSC_TIMESTAMP,
                    (event.timestamp % (i32::MAX as u64 + 1)) as i32,
                );
                self.send(self.mouse_fd, ET_SYNC, EC_SYNC_REPORT, 0);
            }
        }
    }

    fn send_keyboard_event(&mut self, event: &KeyboardEvent) {
        use crate::input::uinput_keys::*;
        if let Err(err) = self.capturable.before_input() {
            warn!("Failed to activate window, sending no input ({})", err);
            return;
        }
        fn map_key(code: &str, location: &KeyboardLocation) -> c_int {
            match (code, location) {
                ("Escape", _) => KEY_ESC,
                ("Digit0", KeyboardLocation::Numpad) => KEY_KP0,
                ("Digit1", KeyboardLocation::Numpad) => KEY_KP1,
                ("Digit2", KeyboardLocation::Numpad) => KEY_KP2,
                ("Digit3", KeyboardLocation::Numpad) => KEY_KP3,
                ("Digit4", KeyboardLocation::Numpad) => KEY_KP4,
                ("Digit5", KeyboardLocation::Numpad) => KEY_KP5,
                ("Digit6", KeyboardLocation::Numpad) => KEY_KP6,
                ("Digit7", KeyboardLocation::Numpad) => KEY_KP7,
                ("Digit8", KeyboardLocation::Numpad) => KEY_KP8,
                ("Digit9", KeyboardLocation::Numpad) => KEY_KP9,
                ("Minus", KeyboardLocation::Numpad) => KEY_KPMINUS,
                ("Equal", KeyboardLocation::Numpad) => KEY_KPEQUAL,
                ("Enter", KeyboardLocation::Numpad) => KEY_KPENTER,
                ("Digit0", _) => KEY_0,
                ("Digit1", _) => KEY_1,
                ("Digit2", _) => KEY_2,
                ("Digit3", _) => KEY_3,
                ("Digit4", _) => KEY_4,
                ("Digit5", _) => KEY_5,
                ("Digit6", _) => KEY_6,
                ("Digit7", _) => KEY_7,
                ("Digit8", _) => KEY_8,
                ("Digit9", _) => KEY_9,
                ("Minus", _) => KEY_MINUS,
                ("Equal", _) => KEY_EQUAL,
                ("Enter", _) => KEY_ENTER,
                ("Backspace", _) => KEY_BACKSPACE,
                ("Tab", _) => KEY_TAB,
                ("KeyA", _) => KEY_A,
                ("KeyB", _) => KEY_B,
                ("KeyC", _) => KEY_C,
                ("KeyD", _) => KEY_D,
                ("KeyE", _) => KEY_E,
                ("KeyF", _) => KEY_F,
                ("KeyG", _) => KEY_G,
                ("KeyH", _) => KEY_H,
                ("KeyI", _) => KEY_I,
                ("KeyJ", _) => KEY_J,
                ("KeyK", _) => KEY_K,
                ("KeyL", _) => KEY_L,
                ("KeyM", _) => KEY_M,
                ("KeyN", _) => KEY_N,
                ("KeyO", _) => KEY_O,
                ("KeyP", _) => KEY_P,
                ("KeyQ", _) => KEY_Q,
                ("KeyR", _) => KEY_R,
                ("KeyS", _) => KEY_S,
                ("KeyT", _) => KEY_T,
                ("KeyU", _) => KEY_U,
                ("KeyV", _) => KEY_V,
                ("KeyW", _) => KEY_W,
                ("KeyX", _) => KEY_X,
                ("KeyY", _) => KEY_Y,
                ("KeyZ", _) => KEY_Z,
                ("BracketLeft", _) => KEY_LEFTBRACE,
                ("BracketRight", _) => KEY_RIGHTBRACE,
                ("Semicolon", _) => KEY_SEMICOLON,
                ("Quote", _) => KEY_APOSTROPHE,
                ("Backquote", _) => KEY_GRAVE,
                ("Backslash", _) => KEY_BACKSLASH,
                ("Comma", _) => KEY_COMMA,
                ("Period", _) => KEY_DOT,
                ("Slash", _) => KEY_SLASH,
                ("Space", _) => KEY_SPACE,
                ("CapsLock", _) => KEY_CAPSLOCK,
                ("NumpadMultiply", _) => KEY_KPASTERISK,
                ("F1", _) => KEY_F1,
                ("F2", _) => KEY_F2,
                ("F3", _) => KEY_F3,
                ("F4", _) => KEY_F4,
                ("F5", _) => KEY_F5,
                ("F6", _) => KEY_F6,
                ("F7", _) => KEY_F7,
                ("F8", _) => KEY_F8,
                ("F9", _) => KEY_F9,
                ("F10", _) => KEY_F10,
                ("F11", _) => KEY_F11,
                ("F12", _) => KEY_F12,
                ("F13", _) => KEY_F13,
                ("F14", _) => KEY_F14,
                ("F15", _) => KEY_F15,
                ("F16", _) => KEY_F16,
                ("F17", _) => KEY_F17,
                ("F18", _) => KEY_F18,
                ("F19", _) => KEY_F19,
                ("F20", _) => KEY_F20,
                ("F21", _) => KEY_F21,
                ("F22", _) => KEY_F22,
                ("F23", _) => KEY_F23,
                ("F24", _) => KEY_F24,
                ("NumLock", _) => KEY_NUMLOCK,
                ("ScrollLock", _) => KEY_SCROLLLOCK,
                ("Numpad0", _) => KEY_KP0,
                ("Numpad1", _) => KEY_KP1,
                ("Numpad2", _) => KEY_KP2,
                ("Numpad3", _) => KEY_KP3,
                ("Numpad4", _) => KEY_KP4,
                ("Numpad5", _) => KEY_KP5,
                ("Numpad6", _) => KEY_KP6,
                ("Numpad7", _) => KEY_KP7,
                ("Numpad8", _) => KEY_KP8,
                ("Numpad9", _) => KEY_KP9,
                ("NumpadSubtract", _) => KEY_KPMINUS,
                ("NumpadAdd", _) => KEY_KPPLUS,
                // ("NumpadDecimal", _) => ?,
                ("IntlBackslash", _) => KEY_102ND,
                ("IntlRo", _) => KEY_RO,
                ("NumpadEnter", _) => KEY_KPENTER,
                ("NumpadDivide", _) => KEY_KPSLASH,
                ("NumpadEqual", _) => KEY_KPEQUAL,
                ("NumpadComma", _) => KEY_KPCOMMA,
                ("NumpadParenLeft", _) => KEY_KPLEFTPAREN,
                ("NumpadParenRight", _) => KEY_KPRIGHTPAREN,
                // ("NumpadChangeSign", _) => ?,
                // ("Convert", _) => ?,
                ("KanaMode", _) => KEY_KATAKANA,
                // ("NonConvert", _) => ?,
                ("PrintScreen", _) => KEY_SYSRQ,
                ("Home", _) => KEY_HOME,
                ("ArrowUp", _) => KEY_UP,
                ("PageUp", _) => KEY_PAGEUP,
                ("ArrowLeft", _) => KEY_LEFT,
                ("ArrowRight", _) => KEY_RIGHT,
                ("End", _) => KEY_END,
                ("ArrowDown", _) => KEY_DOWN,
                ("PageDown", _) => KEY_PAGEDOWN,
                ("Insert", _) => KEY_INSERT,
                ("Delete", _) => KEY_DELETE,
                ("VolumeMute", _) | ("AudioVolumeMute", _) => KEY_MUTE,
                ("VolumeDown", _) | ("AudioVolumeDown", _) => KEY_VOLUMEDOWN,
                ("VolumeUp", _) | ("AudioVolumeUp", _) => KEY_VOLUMEUP,
                ("Pause", _) => KEY_PAUSE,

                ("Lang1", _) => KEY_HANGUEL,
                ("Lang2", _) => KEY_HANJA,
                ("IntlYen", _) => KEY_YEN,
                ("OSLeft", _) => KEY_LEFTMETA,
                ("OSRight", _) => KEY_RIGHTMETA,
                ("ContextMenu", _) => KEY_MENU,
                // ("BrowserStop", _) => ?,
                ("Cancel", _) => KEY_CANCEL,
                ("Again", _) => KEY_AGAIN,
                ("Props", _) => KEY_PROPS,
                ("Undo", _) => KEY_UNDO,
                // ("Select", _) => ?,
                ("Copy", _) => KEY_COPY,
                ("Open", _) => KEY_OPEN,
                ("Paste", _) => KEY_PASTE,
                ("Find", _) => KEY_FIND,
                ("Cut", _) => KEY_CUT,
                ("Help", _) => KEY_HELP,
                // ("LaunchApp2", _) => ?,
                // ("LaunchApp1", _) => ,
                ("LaunchMail", _) => KEY_MAIL,
                // ("BrowserFavorites", _) => ?,
                // ("BrowserBack", _) => ?,
                // ("BrowserForward", _) => ?,
                ("Eject", _) => KEY_EJECTCD,
                ("MediaTrackNext", _) => KEY_NEXTSONG,
                ("MediaPlayPause", _) => KEY_PLAYPAUSE,
                ("MediaTrackPrevious", _) => KEY_PREVIOUSSONG,
                ("MediaStop", _) => KEY_STOPCD,
                ("MediaSelect", _) | ("LaunchMediaPlayer", _) => KEY_MEDIA,
                // ("BrowserHome", _) => ?,
                // ("BrowserRefresh", _) => ?,
                // ("BrowserSearch", _) => ?,
                ("Power", _) => KEY_POWER,
                ("Sleep", _) => KEY_SLEEP,
                ("WakeUp", _) => KEY_WAKEUP,
                ("ControlLeft", _) => KEY_LEFTCTRL,
                ("ControlRight", _) => KEY_RIGHTCTRL,
                ("AltLeft", _) => KEY_LEFTALT,
                ("AltRight", _) => KEY_RIGHTALT,
                ("MetaLeft", _) => KEY_LEFTMETA,
                ("MetaRight", _) => KEY_RIGHTMETA,
                ("ShiftLeft", _) => KEY_LEFTSHIFT,
                ("ShiftRight", _) => KEY_RIGHTSHIFT,
                _ => KEY_UNKNOWN,
            }
        }

        let key_code: c_int = map_key(&event.code, &event.location);
        let state: c_int = match event.event_type {
            KeyboardEventType::Up => 0,
            KeyboardEventType::Down => 1,
            KeyboardEventType::Repeat => 2,
        };

        if key_code == KEY_UNKNOWN {
            if let KeyboardEventType::Down = event.event_type {
                if !event.key.is_empty() {
                    // If the key is unknow try inserting the unicode character directly
                    // to do so use CTRL + SHIFT + U + UTF16 HEX of the unicode point.
                    let unicode_keys = event
                        .key
                        .encode_utf16()
                        .map(|b| format!("{:X}", b))
                        .collect::<Vec<String>>()
                        .concat();

                    debug!(
                        "Got unknown key: {} code: {}, trying to insert unicode using ctrl + \
                        shift + u + {}!",
                        event.code, event.key, unicode_keys
                    );

                    self.send(self.keyboard_fd, ET_KEY, KEY_LEFTCTRL, 1);
                    self.send(self.keyboard_fd, ET_KEY, KEY_LEFTSHIFT, 1);
                    self.send(self.keyboard_fd, ET_KEY, KEY_U, 1);
                    self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                    for c in unicode_keys.chars() {
                        let key_code = if c.is_alphabetic() {
                            map_key(&format!("Key{}", c), &KeyboardLocation::Standard)
                        } else {
                            map_key(&format!("Digit{}", c), &KeyboardLocation::Standard)
                        };

                        self.send(self.keyboard_fd, ET_KEY, key_code, 1);
                        self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                        self.send(self.keyboard_fd, ET_KEY, key_code, 0);
                        self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                    }
                    self.send(self.keyboard_fd, ET_KEY, KEY_LEFTCTRL, 0);
                    self.send(self.keyboard_fd, ET_KEY, KEY_LEFTSHIFT, 0);
                    self.send(self.keyboard_fd, ET_KEY, KEY_U, 0);
                    self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
                }
            } else {
                debug!(
                    "Got unknow key: code: {} key: {}, ignoring event.",
                    event.code, event.key
                );
            }
            return;
        }

        if event.ctrl {
            self.send(self.keyboard_fd, ET_KEY, KEY_LEFTCTRL, state);
            self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
        }
        if event.alt {
            self.send(self.keyboard_fd, ET_KEY, KEY_LEFTALT, state);
            self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
        }
        if event.meta {
            self.send(self.keyboard_fd, ET_KEY, KEY_LEFTMETA, state);
            self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
        }
        if event.shift {
            self.send(self.keyboard_fd, ET_KEY, KEY_LEFTSHIFT, state);
            self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
        }

        self.send(self.keyboard_fd, ET_KEY, key_code, state);
        self.send(self.keyboard_fd, ET_SYNC, EC_SYNC_REPORT, 0);
    }

    fn set_capturable(&mut self, capturable: Box<dyn Capturable>) {
        self.capturable = capturable;
    }
}
