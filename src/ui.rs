//! The display, a client of the state task that draws the screens
//! and acts on taps.
//!
//! `run` opens the output, samples the histories, redraws the
//! current page twice a second, and turns each tap into a command:
//! the setpoint buttons move the setpoint by one degree Fahrenheit
//! through the same command the API uses, and the corner button
//! flips the page.

use std::time::Duration;

use anyhow::{Context, Result};
use tiny_skia::Pixmap;
use tokio::sync::mpsc;

use crate::control::Limits;
use crate::display::{self, Output, Size};
use crate::history::{self, Histories};
use crate::screen::{self, Action, Button, Page, SETPOINT_STEP_C, Screen};
use crate::state::Client;
use crate::touch;

/// How often the page is redrawn.
const REDRAW: Duration = Duration::from_millis(500);

/// Runs the display until the state task is gone or the output
/// fails.
pub async fn run(
    client: Client,
    output: Output,
    fallback: Size,
    limits: Limits,
    first_page: Page,
) -> Result<()> {
    let (mut sink, size) = display::open(&output, fallback).context("open the display")?;
    tracing::info!(?output, size.width, size.height, "display");
    let screen = Screen::new(limits);
    let mut frame = Pixmap::new(size.width, size.height).context("allocate the frame")?;

    let (taps_tx, mut taps) = mpsc::channel(8);
    let touch = tokio::spawn(touch::run(taps_tx, size));

    let mut histories = Histories::default();
    let mut page = first_page;
    let mut buttons: Vec<Button> = Vec::new();
    let mut sampler = tokio::time::interval(history::SAMPLE);
    let mut redraw = tokio::time::interval(REDRAW);
    loop {
        tokio::select! {
            _ = sampler.tick() => {
                histories.sample(&client.state());
            }
            _ = redraw.tick() => {
                let state = client.state();
                buttons = screen.render(&mut frame, page, &state, &histories);
                let outcome = tokio::task::block_in_place(|| sink.present(&frame));
                if let Err(e) = outcome {
                    touch.abort();
                    return Err(e.context("present the frame"));
                }
            }
            tap = taps.recv() => {
                let Some(tap) = tap else {
                    touch.abort();
                    anyhow::bail!("touch task stopped");
                };
                match screen::hit(&buttons, tap.x, tap.y) {
                    Some(Action::SetpointUp) => nudge(&client, SETPOINT_STEP_C).await,
                    Some(Action::SetpointDown) => nudge(&client, -SETPOINT_STEP_C).await,
                    Some(Action::TogglePage) => page = page.other(),
                    None => {}
                }
            }
        }
    }
}

/// Moves the setpoint by `delta` degrees Celsius. A refusal is the
/// state task's to make and is only logged here.
async fn nudge(client: &Client, delta: f64) {
    let target = client.state().setpoint_c + delta;
    if let Err(e) = client.set_setpoint(target).await {
        tracing::warn!("setpoint nudge refused: {e:#}");
    }
}
