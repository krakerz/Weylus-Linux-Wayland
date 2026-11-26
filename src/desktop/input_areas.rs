use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Emitter, Manager};
use tracing::warn;

use crate::protocol::{CustomInputAreas, Rect};

pub(crate) static APP: OnceLock<AppHandle> = OnceLock::new();
static PENDING: Mutex<Option<mpsc::Sender<CustomInputAreas>>> = Mutex::new(None);
/// Prefill for the next request.
static LAST: Mutex<CustomInputAreas> = Mutex::new(CustomInputAreas {
    mouse: None,
    touch: None,
    pen: None,
});

/// Asks the desktop window for areas; the answer goes through `sender`.
pub fn request_input_areas(sender: mpsc::Sender<CustomInputAreas>) {
    let Some(app) = APP.get() else {
        warn!(
            "Choosing custom input areas needs the desktop window; start Weylus without --no-gui."
        );
        return;
    };
    *PENDING.lock().expect("lock poisoned") = Some(sender);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
    let last = *LAST.lock().expect("lock poisoned");
    let _ = app.emit("input-areas-requested", last);
}

/// Fractions of the screen: `0 <= x, y < 1`, `w, h > 0`, clamped to stay inside the screen.
fn validate(areas: CustomInputAreas) -> Result<CustomInputAreas, String> {
    fn rect(name: &str, rect: Option<Rect>) -> Result<Option<Rect>, String> {
        let Some(Rect { x, y, w, h }) = rect else {
            return Ok(None);
        };
        let finite = [x, y, w, h].iter().all(|v| v.is_finite());
        if !finite || !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) || w <= 0.0 || h <= 0.0 {
            return Err(format!("Invalid area for {name}"));
        }
        Ok(Some(Rect {
            x,
            y,
            w: w.min(1.0 - x),
            h: h.min(1.0 - y),
        }))
    }
    Ok(CustomInputAreas {
        mouse: rect("mouse", areas.mouse)?,
        touch: rect("touch", areas.touch)?,
        pen: rect("pen", areas.pen)?,
    })
}

pub(crate) fn submit(areas: CustomInputAreas) -> Result<(), String> {
    let areas = validate(areas)?;
    *LAST.lock().expect("lock poisoned") = areas;
    match PENDING.lock().expect("lock poisoned").take() {
        Some(sender) => {
            if sender.send(areas).is_err() {
                warn!("The client that asked for custom input areas is gone.");
            }
        }
        None => warn!("No custom input areas were requested."),
    }
    Ok(())
}

pub(crate) fn cancel() {
    PENDING.lock().expect("lock poisoned").take();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn areas(rect: Option<Rect>) -> CustomInputAreas {
        CustomInputAreas {
            mouse: rect,
            touch: None,
            pen: None,
        }
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Option<Rect> {
        Some(Rect { x, y, w, h })
    }

    #[test]
    fn none_passes_through() {
        assert_eq!(validate(areas(None)).unwrap(), areas(None));
    }

    #[test]
    fn size_is_clamped_to_the_screen() {
        let valid = validate(areas(rect(0.5, 0.25, 0.8, 0.9))).unwrap();
        assert_eq!(valid.mouse, rect(0.5, 0.25, 0.5, 0.75));
    }

    #[test]
    fn rejects_non_finite_negative_and_empty() {
        for bad in [
            rect(f64::NAN, 0.0, 0.5, 0.5),
            rect(0.0, 0.0, f64::INFINITY, 0.5),
            rect(-0.1, 0.0, 0.5, 0.5),
            rect(0.0, 1.0, 0.5, 0.5),
            rect(0.0, 0.0, 0.0, 0.5),
            rect(0.0, 0.0, 0.5, -1.0),
        ] {
            assert!(validate(areas(bad)).is_err(), "{bad:?}");
        }
    }
}
