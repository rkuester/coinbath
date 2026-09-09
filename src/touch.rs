//! Taps from the touchscreen, read over evdev.
//!
//! `Decoder` turns the raw event stream of a multitouch panel into
//! `Tap`s in frame pixels: one tap per finger-down, at the first
//! position reported for it. `run` finds the panel by name, decodes
//! its events, and sends the taps down a channel, and looks for the
//! panel again if it is missing or goes away.

use std::time::Duration;

use anyhow::{Context, Result};
use evdev::{AbsoluteAxisCode, Device, EventType, KeyCode};
use tokio::sync::mpsc;

use crate::display::Size;

/// The panel's name in evdev. The 10-inch bar screen's controller
/// reports itself as "wch.cn TouchScreen"; the node numbers move,
/// so the name is the handle.
pub const DEVICE_NAME: &str = "TouchScreen";

/// How long to wait before looking for the panel again.
const RETRY: Duration = Duration::from_secs(10);

/// One finger down, in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tap {
    pub x: i32,
    pub y: i32,
}

/// The raw range of an axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axis {
    pub min: i32,
    pub max: i32,
}

impl Axis {
    /// Maps a raw value onto `0..length`.
    fn scale(&self, value: i32, length: u32) -> i32 {
        let span = (self.max - self.min).max(1) as f64;
        let fraction = (value - self.min) as f64 / span;
        ((fraction * length as f64) as i32).clamp(0, length as i32 - 1)
    }
}

/// The state machine over raw events.
///
/// A tap is reported when a touch begins, at the position in
/// force at the end of that report, so the first report of a new
/// finger carries the tap and dragging afterward does nothing. A
/// touch begins with BTN_TOUCH going to 1, or with a tracking id
/// that is not -1 on a panel that sends no BTN_TOUCH.
#[derive(Debug, Clone)]
pub struct Decoder {
    frame: Size,
    x_axis: Axis,
    y_axis: Axis,
    x: i32,
    y: i32,
    down: bool,
    began: bool,
}

impl Decoder {
    pub fn new(frame: Size, x_axis: Axis, y_axis: Axis) -> Self {
        Self {
            frame,
            x_axis,
            y_axis,
            x: 0,
            y: 0,
            down: false,
            began: false,
        }
    }

    /// Feeds one raw event and returns a tap when a report ends one
    /// that began a touch.
    pub fn feed(&mut self, event_type: EventType, code: u16, value: i32) -> Option<Tap> {
        match event_type {
            EventType::ABSOLUTE => {
                let code = AbsoluteAxisCode(code);
                if code == AbsoluteAxisCode::ABS_MT_POSITION_X || code == AbsoluteAxisCode::ABS_X {
                    self.x = self.x_axis.scale(value, self.frame.width);
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_Y
                    || code == AbsoluteAxisCode::ABS_Y
                {
                    self.y = self.y_axis.scale(value, self.frame.height);
                } else if code == AbsoluteAxisCode::ABS_MT_TRACKING_ID {
                    self.set_down(value != -1);
                }
                None
            }
            EventType::KEY if KeyCode(code) == KeyCode::BTN_TOUCH => {
                self.set_down(value != 0);
                None
            }
            EventType::SYNCHRONIZATION => {
                let tap = self.began.then_some(Tap {
                    x: self.x,
                    y: self.y,
                });
                self.began = false;
                tap
            }
            _ => None,
        }
    }

    fn set_down(&mut self, down: bool) {
        if down && !self.down {
            self.began = true;
        }
        self.down = down;
    }
}

/// Reads taps from the panel until the receiver is gone.
///
/// Without a panel the display still works; the taps are the only
/// thing missing. A missing or lost panel is logged once, and
/// looked for again every few seconds.
pub async fn run(taps: mpsc::Sender<Tap>, frame: Size) -> Result<()> {
    let mut missing_logged = false;
    loop {
        match find_panel() {
            Some((path, device)) => {
                tracing::info!(path, name = device.name().unwrap_or(""), "touch panel");
                missing_logged = false;
                if let Err(e) = read_taps(device, &taps, frame).await {
                    tracing::warn!("touch panel lost: {e:#}");
                }
                if taps.is_closed() {
                    return Ok(());
                }
            }
            None => {
                if !missing_logged {
                    tracing::warn!(name = DEVICE_NAME, "no touch panel; the buttons need one");
                    missing_logged = true;
                }
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// The first evdev device whose name contains the panel's.
fn find_panel() -> Option<(String, Device)> {
    evdev::enumerate()
        .filter(|(_, d)| d.name().is_some_and(|n| n.contains(DEVICE_NAME)))
        .filter(|(_, d)| {
            d.supported_absolute_axes()
                .is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_X))
        })
        .map(|(p, d)| (p.display().to_string(), d))
        .next()
}

async fn read_taps(device: Device, taps: &mpsc::Sender<Tap>, frame: Size) -> Result<()> {
    let abs = device.get_abs_state().context("read axis ranges")?;
    let axis = |code: AbsoluteAxisCode| {
        let info = abs[code.0 as usize];
        Axis {
            min: info.minimum,
            max: info.maximum,
        }
    };
    let mut decoder = Decoder::new(
        frame,
        axis(AbsoluteAxisCode::ABS_MT_POSITION_X),
        axis(AbsoluteAxisCode::ABS_MT_POSITION_Y),
    );
    let mut events = device.into_event_stream().context("open event stream")?;
    loop {
        let event = events.next_event().await.context("read event")?;
        if let Some(tap) = decoder.feed(event.event_type(), event.code(), event.value()) {
            tracing::debug!(tap.x, tap.y, "tap");
            if taps.send(tap).await.is_err() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYN: u16 = 0;
    const X: u16 = AbsoluteAxisCode::ABS_MT_POSITION_X.0;
    const Y: u16 = AbsoluteAxisCode::ABS_MT_POSITION_Y.0;
    const ID: u16 = AbsoluteAxisCode::ABS_MT_TRACKING_ID.0;
    const TOUCH: u16 = KeyCode::BTN_TOUCH.0;

    fn decoder() -> Decoder {
        Decoder::new(
            Size {
                width: 1280,
                height: 400,
            },
            Axis { min: 0, max: 4095 },
            Axis { min: 0, max: 4095 },
        )
    }

    #[test]
    fn a_touch_is_one_tap_at_its_first_position_scaled_to_the_frame() {
        let mut d = decoder();
        assert_eq!(d.feed(EventType::ABSOLUTE, ID, 7), None);
        assert_eq!(d.feed(EventType::ABSOLUTE, X, 2048), None);
        assert_eq!(d.feed(EventType::ABSOLUTE, Y, 4095), None);
        assert_eq!(d.feed(EventType::KEY, TOUCH, 1), None);
        assert_eq!(
            d.feed(EventType::SYNCHRONIZATION, SYN, 0),
            Some(Tap { x: 640, y: 399 })
        );

        // Dragging reports positions with no new touch.
        assert_eq!(d.feed(EventType::ABSOLUTE, X, 3000), None);
        assert_eq!(d.feed(EventType::SYNCHRONIZATION, SYN, 0), None);

        // Lifting and touching again is a new tap.
        d.feed(EventType::ABSOLUTE, ID, -1);
        d.feed(EventType::KEY, TOUCH, 0);
        assert_eq!(d.feed(EventType::SYNCHRONIZATION, SYN, 0), None);
        d.feed(EventType::ABSOLUTE, ID, 8);
        d.feed(EventType::ABSOLUTE, X, 0);
        d.feed(EventType::ABSOLUTE, Y, 0);
        d.feed(EventType::KEY, TOUCH, 1);
        assert_eq!(
            d.feed(EventType::SYNCHRONIZATION, SYN, 0),
            Some(Tap { x: 0, y: 0 })
        );
    }

    #[test]
    fn a_panel_without_btn_touch_still_taps() {
        let mut d = decoder();
        d.feed(EventType::ABSOLUTE, ID, 1);
        d.feed(EventType::ABSOLUTE, X, 1024);
        d.feed(EventType::ABSOLUTE, Y, 2048);
        assert_eq!(
            d.feed(EventType::SYNCHRONIZATION, SYN, 0),
            Some(Tap { x: 320, y: 200 })
        );
    }

    #[test]
    fn an_axis_with_an_offset_range_maps_its_ends_to_the_frame() {
        let axis = Axis { min: 100, max: 300 };
        assert_eq!(axis.scale(100, 1280), 0);
        assert_eq!(axis.scale(300, 1280), 1279);
        assert_eq!(axis.scale(200, 1280), 640);
        assert_eq!(axis.scale(-5, 1280), 0);
    }
}
