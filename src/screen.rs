//! The two screens, drawn from a snapshot and its histories.
//!
//! `Screen::render` draws a page onto a frame and returns the
//! buttons on it, each a rectangle and the action a tap there
//! means. The layout is designed at 1424 by 280, the 10-inch bar
//! panel, and scales with the frame's height. Temperatures are
//! shown in Fahrenheit for the table and Celsius for the
//! engineering, since the bath is set in Fahrenheit and the chips
//! are read in Celsius.

use sparklines::{Marker, Sparkline};
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Transform};

use crate::control::{self, Fault, Limits};
use crate::history::{Histories, History};
use crate::image::{self, Image};
use crate::state::{Board, Mode, State};
use crate::text::{Face, Fonts, Rgb, Style};

/// Graphite surfaces and the lifted accent red of the Mujina site
/// in dark mode.
pub const BACKGROUND: Rgb = [0x0e, 0x0f, 0x12];
pub const PANEL: Rgb = [0x16, 0x17, 0x1c];
pub const BORDER: Rgb = [0x27, 0x29, 0x31];
pub const TEXT: Rgb = [0xe7, 0xe8, 0xec];
pub const DIM: Rgb = [0x99, 0x9b, 0xa4];
pub const LINE: Rgb = [0xb5, 0xb7, 0xbf];
pub const ACCENT: Rgb = [0xd0, 0x55, 0x55];

/// The height the layout is designed at. The panel is 1424 wide.
const DESIGN_HEIGHT: f32 = 280.0;

/// How far a setpoint button moves the setpoint: one degree
/// Fahrenheit, the unit on the table.
pub const SETPOINT_STEP_C: f64 = 5.0 / 9.0;

/// The least each sparkline's vertical range spans, so steady
/// readings draw flat instead of their noise filling the box: two
/// degrees Fahrenheit for the water, a board's worth of hash rate,
/// a third of a board's power.
const TEMPERATURE_SPAN_C: f64 = 2.0 * 5.0 / 9.0;
const HASHRATE_SPAN_THS: f64 = 1.0;
const POWER_SPAN_W: f64 = 25.0;

/// Which page is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Main,
    Boards,
}

impl Page {
    pub fn other(self) -> Self {
        match self {
            Page::Main => Page::Boards,
            Page::Boards => Page::Main,
        }
    }
}

/// What a tap on a button asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SetpointUp,
    SetpointDown,
    TogglePage,
}

/// A rectangle in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Area {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// A tappable rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub area: Area,
    pub action: Action,
}

/// The action under a point, if any.
pub fn hit(buttons: &[Button], x: i32, y: i32) -> Option<Action> {
    buttons
        .iter()
        .find(|b| b.area.contains(x, y))
        .map(|b| b.action)
}

/// The fonts and images the screens draw with, loaded once.
pub struct Screen {
    fonts: Fonts,
    badger: Image,
    limits: Limits,
}

impl Screen {
    pub fn new(limits: Limits) -> Self {
        Self {
            fonts: Fonts::load(),
            badger: Image::from_png(image::BADGER_HEAD),
            limits,
        }
    }

    /// Draws `page` over the whole frame and returns its buttons.
    pub fn render(
        &self,
        frame: &mut Pixmap,
        page: Page,
        state: &State,
        histories: &Histories,
    ) -> Vec<Button> {
        let mut c = Canvas::new(frame, &self.fonts);
        c.fill(
            Area {
                x: 0,
                y: 0,
                width: c.width,
                height: c.height,
            },
            BACKGROUND,
        );
        let page_button = match page {
            Page::Main => "details \u{203A}",
            Page::Boards => "\u{2039} back",
        };
        let mut buttons = vec![self.header(&mut c, state, page_button)];
        match page {
            Page::Main => self.main(&mut c, state, histories, &mut buttons),
            Page::Boards => self.boards(&mut c, state),
        }
        buttons
    }

    /// The badger and the wordmark at top left; the page button,
    /// the mode, and any fault at top right. Returns the page
    /// button.
    fn header(&self, c: &mut Canvas, state: &State, page_label: &str) -> Button {
        let logo_h = c.px(32.0);
        let x = c.px(16.0);
        let y = c.px(5.0);
        self.badger.blit(c.frame, x, y, logo_h as u32);
        let wordmark_x = x + self.badger.width_at(logo_h as u32) as i32 + c.px(8.0);
        c.text(
            c.style(Face::Wordmark, 22.0, TEXT),
            wordmark_x,
            y + c.px(5.0),
            "mujina",
        );

        let button = Area {
            width: c.px(110.0),
            height: c.px(30.0),
            x: c.width - c.px(16.0) - c.px(110.0),
            y: c.px(6.0),
        };
        c.panel(button, c.px(7.0));
        c.text_centered(
            c.style(Face::Medium, 16.0, DIM),
            button.x + button.width / 2,
            button.y + c.px(6.0),
            page_label,
        );

        let label = c.style(Face::Medium, 16.0, DIM);
        let mut right = button.x - c.px(20.0);
        let text_y = button.y + c.px(6.0);
        let (mode, mode_color) = match state.mode {
            Mode::Auto => ("AUTO", DIM),
            Mode::Manual => ("MANUAL", ACCENT),
        };
        c.text_right(
            Style {
                color: mode_color,
                ..label
            },
            right,
            text_y,
            mode,
        );
        right -= c.measure(label, mode) + c.px(16.0);
        let status = match control::fault(state, &self.limits) {
            Some(Fault::NoBathReading) => Some("no bath reading"),
            Some(Fault::BathOverLimit(_)) => Some("bath over limit"),
            Some(Fault::MinerOffline) => Some("miner offline"),
            None => None,
        };
        if let Some(status) = status {
            c.text_right(
                Style {
                    color: ACCENT,
                    ..label
                },
                right,
                text_y,
                status,
            );
        }
        Button {
            area: button,
            action: Action::TogglePage,
        }
    }

    fn main(
        &self,
        c: &mut Canvas,
        state: &State,
        histories: &Histories,
        buttons: &mut Vec<Button>,
    ) {
        let top = c.px(46.0);
        let label = c.style(Face::Medium, 16.0, DIM);
        let small = c.style(Face::Medium, 16.0, DIM);

        // The setpoint and its buttons, in the left column.
        let x = c.px(16.0);
        c.text(label, x, top, "SETPOINT");
        c.text(
            c.style(Face::Bold, 72.0, TEXT),
            x - c.px(3.0),
            top + c.px(14.0),
            &fahrenheit(state.setpoint_c, 0),
        );
        c.text(
            c.style(Face::Medium, 18.0, DIM),
            x,
            top + c.px(96.0),
            &format!("{:.1} \u{00B0}C", state.setpoint_c),
        );
        let button_w = c.px(130.0);
        let glyph = c.style(Face::Bold, 64.0, TEXT);
        for (i, (text, action)) in [
            ("\u{2212}", Action::SetpointDown),
            ("+", Action::SetpointUp),
        ]
        .into_iter()
        .enumerate()
        {
            let area = Area {
                x: x + i as i32 * (button_w + c.px(14.0)),
                y: top + c.px(120.0),
                width: button_w,
                height: c.height - c.px(12.0) - (top + c.px(120.0)),
            };
            c.panel(area, c.px(10.0));
            let glyph_y = area.y + (area.height - glyph.px as i32) / 2 - c.px(5.0);
            c.text_centered(glyph, area.x + area.width / 2, glyph_y, text);
            buttons.push(Button { area, action });
        }

        // Three temperature columns, the bath with the setpoint as
        // its reference line.
        let column_w = c.px(220.0);
        let gap = c.px(20.0);
        let columns_x = c.px(320.0);
        let spark_top = top + c.px(90.0);
        let spark_h = c.height - c.px(12.0) - spark_top;
        let temps = [
            (
                "BATH",
                control::bath_reading(state),
                &histories.bath,
                Some(state.setpoint_c),
            ),
            ("INLET", probe(state, "inlet"), &histories.inlet, None),
            ("OUTLET", probe(state, "outlet"), &histories.outlet, None),
        ];
        for (i, (name, reading, history, reference)) in temps.into_iter().enumerate() {
            let cx = columns_x + i as i32 * (column_w + gap);
            c.text(label, cx, top, name);
            let value = reading
                .map(|t| fahrenheit(t, 1))
                .unwrap_or_else(|| "--".to_string());
            c.text(
                c.style(Face::Bold, 48.0, TEXT),
                cx - c.px(2.0),
                top + c.px(14.0),
                &value,
            );
            if let Some(t) = reading {
                c.text(small, cx, top + c.px(66.0), &format!("{t:.1} \u{00B0}C"));
            }
            c.sparkline(
                Area {
                    x: cx,
                    y: spark_top,
                    width: column_w,
                    height: spark_h,
                },
                history,
                TEMPERATURE_SPAN_C,
                reference,
                |v| fahrenheit(v, 1),
            );
        }

        // The miner at the right: hash rate and power side by side,
        // and under them the share the boards hold.
        let rx = columns_x + 3 * column_w + 2 * gap + c.px(10.0);
        let rw = c.width - c.px(16.0) - rx;
        let half = (rw - gap) / 2;
        let miner = &state.miner;
        let value = c.style(Face::Bold, 40.0, TEXT);
        let spark_top = top + c.px(62.0);
        let spark_h = c.px(88.0);

        c.text(label, rx, top, "HASHRATE");
        let hashrate = miner
            .hashrate_hs
            .map(|h| format!("{:.1} TH/s", h / 1e12))
            .unwrap_or_else(|| "--".to_string());
        c.text(value, rx - c.px(2.0), top + c.px(14.0), &hashrate);
        c.sparkline(
            Area {
                x: rx,
                y: spark_top,
                width: half,
                height: spark_h,
            },
            &histories.hashrate_ths,
            HASHRATE_SPAN_THS,
            None,
            |v| format!("{v:.1}"),
        );

        let px = rx + half + gap;
        c.text(label, px, top, "POWER");
        let power = miner
            .power_w
            .map(|w| format!("{w:.0} W"))
            .unwrap_or_else(|| "--".to_string());
        c.text(value, px - c.px(2.0), top + c.px(14.0), &power);
        c.sparkline(
            Area {
                x: px,
                y: spark_top,
                width: half,
                height: spark_h,
            },
            &histories.power_w,
            POWER_SPAN_W,
            None,
            |v| format!("{v:.0}"),
        );

        // The share bar: what the miner holds in red, and a mark
        // where the request stands.
        let bar = Area {
            x: rx,
            y: top + c.px(166.0),
            width: rw,
            height: c.px(12.0),
        };
        c.fill(bar, PANEL);
        if let Some(held) = miner.power_fraction {
            c.fill(
                Area {
                    width: (rw as f64 * held.clamp(0.0, 1.0)) as i32,
                    ..bar
                },
                ACCENT,
            );
        }
        if let Some(asked) = state.power_fraction {
            let mark_x = rx + (rw as f64 * asked.clamp(0.0, 1.0)) as i32;
            c.fill(
                Area {
                    x: mark_x - c.px(1.0),
                    y: bar.y - c.px(3.0),
                    width: c.px(3.0),
                    height: bar.height + c.px(6.0),
                },
                TEXT,
            );
        }
        let share = match (state.power_fraction, miner.power_fraction) {
            (Some(asked), Some(held)) => {
                format!("{:.0}% asked, {:.0}% held", asked * 100.0, held * 100.0)
            }
            (None, Some(held)) => format!("{:.0}% held", held * 100.0),
            _ => "share unknown".to_string(),
        };
        let capped = miner.power_ceiling.is_some_and(|ceiling| ceiling < 1.0);
        let (share, color) = if capped {
            (format!("{share}, capped by heat"), ACCENT)
        } else {
            (share, DIM)
        };
        let note = c.style(Face::Medium, 14.0, DIM);
        c.text(Style { color, ..note }, rx, bar.y + c.px(18.0), &share);
        let chip = miner
            .chip_temperature_c
            .map(|t| format!("chip {t:.0} \u{00B0}C"))
            .unwrap_or_else(|| "chip --".to_string());
        c.text_right(note, rx + rw, bar.y + c.px(18.0), &chip);
    }

    /// The boards page: one row per board with the chain's
    /// electricals, and a totals row.
    fn boards(&self, c: &mut Canvas, state: &State) {
        let top = c.px(46.0);
        let x = c.px(16.0);
        let label = c.style(Face::Medium, 16.0, DIM);
        let value = c.style(Face::Bold, 24.0, TEXT);
        let total = c.style(Face::Bold, 24.0, DIM);

        c.text(label, x, top, "BOARD");
        for column in COLUMNS {
            c.text_right(label, c.px(column.right), top, column.label);
        }
        let row_h = c.px(36.0);
        let rule = |c: &mut Canvas, y: i32| {
            c.fill(
                Area {
                    x,
                    y: y - c.px(4.0),
                    width: c.width - 2 * x,
                    height: c.px(1.0),
                },
                BORDER,
            );
        };
        let mut y = top + c.px(24.0);
        rule(c, y);
        let boards = &state.miner.boards;
        for board in boards {
            let name = board
                .name
                .rsplit_once('-')
                .map(|(_, serial)| serial)
                .unwrap_or(&board.name);
            c.text(value, x, y + c.px(6.0), name);
            for column in COLUMNS {
                let text = column.format((column.field)(board));
                c.text_right(value, c.px(column.right), y + c.px(6.0), &text);
            }
            y += row_h;
        }
        if boards.is_empty() {
            let message = if state.miner.online {
                "the miner reports no boards"
            } else {
                "miner offline"
            };
            c.text(c.style(Face::Medium, 24.0, DIM), x, y + c.px(6.0), message);
            y += row_h;
        }
        rule(c, y);
        let miner = &state.miner;
        c.text(total, x, y + c.px(6.0), "TOTAL");
        for (column, value) in [
            (&COLUMNS[0], miner.chip_temperature_c),
            (&COLUMNS[6], miner.power_w),
            (&COLUMNS[7], miner.hashrate_hs.map(|h| h / 1e12)),
        ] {
            c.text_right(
                total,
                c.px(column.right),
                y + c.px(8.0),
                &column.format(value),
            );
        }
    }
}

/// One column of the boards table: its heading, its right edge in
/// design pixels, the board field it shows, and its decimals.
struct Column {
    label: &'static str,
    right: f32,
    field: fn(&Board) -> Option<f64>,
    decimals: usize,
}

impl Column {
    fn format(&self, value: Option<f64>) -> String {
        value
            .map(|v| format!("{v:.*}", self.decimals))
            .unwrap_or_else(|| "--".to_string())
    }
}

const COLUMNS: &[Column] = &[
    Column {
        label: "CHIP \u{00B0}C",
        right: 430.0,
        field: |b| b.chip_temperature_c,
        decimals: 0,
    },
    Column {
        label: "BOARD \u{00B0}C",
        right: 560.0,
        field: |b| b.board_temperature_c,
        decimals: 0,
    },
    Column {
        label: "REG \u{00B0}C",
        right: 690.0,
        field: |b| b.regulator_temperature_c,
        decimals: 0,
    },
    Column {
        label: "IN V",
        right: 810.0,
        field: |b| b.input_voltage_v,
        decimals: 1,
    },
    Column {
        label: "CORE V",
        right: 1060.0,
        field: |b| b.voltage_v,
        decimals: 3,
    },
    Column {
        label: "A",
        right: 940.0,
        field: |b| b.current_a,
        decimals: 1,
    },
    Column {
        label: "W",
        right: 1180.0,
        field: |b| b.power_w,
        decimals: 0,
    },
    Column {
        label: "TH/s",
        right: 1310.0,
        field: |b| b.hashrate_hs.map(|h| h / 1e12),
        decimals: 2,
    },
];

/// Degrees Fahrenheit with the sign, from Celsius.
fn fahrenheit(celsius: f64, decimals: usize) -> String {
    format!("{:.*}\u{00B0}F", decimals, celsius * 9.0 / 5.0 + 32.0)
}

fn probe(state: &State, name: &str) -> Option<f64> {
    state
        .probes
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.celsius)
}

/// A frame with the fonts and the scale from the design height.
struct Canvas<'a> {
    frame: &'a mut Pixmap,
    fonts: &'a Fonts,
    width: i32,
    height: i32,
    scale: f32,
}

impl<'a> Canvas<'a> {
    fn new(frame: &'a mut Pixmap, fonts: &'a Fonts) -> Self {
        let width = frame.width() as i32;
        let height = frame.height() as i32;
        Self {
            frame,
            fonts,
            width,
            height,
            scale: height as f32 / DESIGN_HEIGHT,
        }
    }

    /// A design length in frame pixels.
    fn px(&self, design: f32) -> i32 {
        (design * self.scale).round() as i32
    }

    /// A text style with its design size scaled to the frame.
    fn style(&self, face: Face, design_px: f32, color: Rgb) -> Style {
        Style {
            face,
            px: design_px * self.scale,
            color,
        }
    }

    fn fill(&mut self, area: Area, color: Rgb) {
        let Some(rect) = Rect::from_xywh(
            area.x as f32,
            area.y as f32,
            area.width.max(0) as f32,
            area.height.max(0) as f32,
        ) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(rgb(color));
        self.frame
            .fill_rect(rect, &paint, Transform::identity(), None);
    }

    /// A rounded panel with a one-pixel border.
    fn panel(&mut self, area: Area, radius: i32) {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        for (inset, color) in [(0.0, BORDER), (1.0, PANEL)] {
            let Some(path) = rounded_rect(
                area.x as f32 + inset,
                area.y as f32 + inset,
                area.width as f32 - 2.0 * inset,
                area.height as f32 - 2.0 * inset,
                radius as f32 - inset,
            ) else {
                continue;
            };
            paint.set_color(rgb(color));
            self.frame.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn text(&mut self, style: Style, x: i32, y: i32, text: &str) {
        self.fonts.draw(self.frame, style, x, y, text);
    }

    fn text_right(&mut self, style: Style, right: i32, y: i32, text: &str) {
        self.fonts.draw_right(self.frame, style, right, y, text);
    }

    fn text_centered(&mut self, style: Style, cx: i32, y: i32, text: &str) {
        self.fonts.draw_centered(self.frame, style, cx, y, text);
    }

    fn measure(&self, style: Style, text: &str) -> i32 {
        self.fonts.measure(style.face, text, style.px) as i32
    }

    /// A sparkline of the history in the area, never zoomed in past
    /// `min_span` data units, with the time it covers at top left,
    /// the latest value labeled at its marker, and a reference line
    /// labeled at the right edge.
    fn sparkline(
        &mut self,
        area: Area,
        history: &History,
        min_span: f64,
        reference: Option<f64>,
        label: impl Fn(f64) -> String,
    ) {
        let samples = history.samples();
        let Some(last) = samples.iter().rev().find_map(|v| *v) else {
            return;
        };
        let mut spark = Sparkline::new(samples)
            .min_span(min_span)
            .y_padding(0.25)
            .marker(Marker::Current);
        if let Some(r) = reference {
            spark = spark.reference_line(r);
        }
        let pixel = spark
            .pixel()
            .line_color(rgb(LINE))
            .stroke_width(self.scale * 2.0)
            .marker_radius(self.scale * 3.5)
            .current_marker_color(rgb(TEXT))
            .reference_line_color(Color::from_rgba8(ACCENT[0], ACCENT[1], ACCENT[2], 200));
        // Leave room at the right for the value labels.
        let plot_w = (area.width - self.px(48.0)) as f32;
        let (x, y, h) = (area.x as f32, area.y as f32, area.height as f32);
        pixel.render(self.frame, x, y, plot_w, h);

        let small = self.style(Face::Medium, 13.0, TEXT);
        let label_x = area.x + plot_w as i32 + self.px(6.0);
        let half = (small.px / 2.0) as i32;
        if let Some((lo, hi)) = pixel.effective_range() {
            let span = hi - lo;
            let last_y = pixel.value_to_y(last, lo, span, y, h) as i32;
            self.text(small, label_x, last_y - half, &label(last));
            if let Some(r) = reference {
                let ref_y = pixel.value_to_y(r, lo, span, y, h) as i32;
                // Stay clear of the latest value's label.
                if (last_y - ref_y).abs() > small.px as i32 {
                    let accent = Style {
                        color: ACCENT,
                        ..small
                    };
                    self.text(accent, label_x, ref_y - half, &label(r));
                }
            }
        }
        // The span fits under a tall plot; a short one goes without.
        if area.height >= self.px(70.0) {
            let span = history.span_secs();
            let span_text = if span < 60.0 {
                format!("{}s", span as u32)
            } else {
                format!("{}m", (span / 60.0) as u32)
            };
            let dim = Style {
                color: DIM,
                ..small
            };
            self.text(dim, area.x, area.y, &span_text);
        }
    }
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn rgb(c: Rgb) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], 255)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Miner;

    fn warm_state() -> State {
        let mut state = State::new(51.1, &["bath", "inlet", "outlet"]);
        for (i, t) in [40.0, 39.7, 41.5].into_iter().enumerate() {
            state.probes[i].celsius = Some(t);
        }
        state.power_fraction = Some(0.6);
        state.miner = Miner {
            online: true,
            hashrate_hs: Some(7.5e12),
            power_w: Some(150.0),
            chip_temperature_c: Some(62.0),
            power_fraction: Some(0.6),
            boards: vec![Board {
                name: "emberone-00-2d714701".into(),
                hashrate_hs: Some(3.75e12),
                chip_temperature_c: Some(62.0),
                voltage_v: Some(1.2),
                current_a: Some(22.5),
                power_w: Some(27.0),
                input_voltage_v: Some(12.1),
                regulator_temperature_c: Some(55.0),
                board_temperature_c: Some(43.5),
                power_fraction: Some(0.6),
                power_ceiling: Some(0.6),
            }],
            power_ceiling: Some(0.6),
        };
        state
    }

    fn histories_for(state: &State, samples: usize) -> Histories {
        let mut histories = Histories::default();
        let mut s = state.clone();
        for i in 0..samples {
            s.probes[0].celsius = Some(30.0 + i as f64 * 0.1);
            s.miner.hashrate_hs = Some(7.0e12 + i as f64 * 1e10);
            histories.sample(&s);
        }
        histories
    }

    #[test]
    fn the_main_page_has_the_page_button_and_the_setpoint_buttons() {
        let screen = Screen::new(Limits::default());
        let state = warm_state();
        let histories = histories_for(&state, 100);
        let mut frame = Pixmap::new(1424, 280).unwrap();
        let buttons = screen.render(&mut frame, Page::Main, &state, &histories);

        let actions: Vec<Action> = buttons.iter().map(|b| b.action).collect();
        assert_eq!(
            actions,
            [Action::TogglePage, Action::SetpointDown, Action::SetpointUp]
        );
        for b in &buttons {
            let a = b.area;
            assert!(a.x >= 0 && a.y >= 0 && a.x + a.width <= 1424 && a.y + a.height <= 280);
        }
        let down = buttons[1].area;
        assert_eq!(
            hit(&buttons, down.x + 5, down.y + 5),
            Some(Action::SetpointDown)
        );
        assert_eq!(hit(&buttons, 700, 150), None);
    }

    #[test]
    fn the_boards_page_lists_the_boards_and_goes_back() {
        let screen = Screen::new(Limits::default());
        let state = warm_state();
        let mut frame = Pixmap::new(1424, 280).unwrap();
        let buttons = screen.render(&mut frame, Page::Boards, &state, &Histories::default());
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].action, Action::TogglePage);
        assert_eq!(Page::Boards.other(), Page::Main);
    }

    #[test]
    fn renders_an_empty_state_and_other_sizes_without_panicking() {
        let screen = Screen::new(Limits::default());
        let state = State::new(51.1, &["bath"]);
        for (w, h) in [
            (1424, 280),
            (1280, 400),
            (1920, 480),
            (800, 480),
            (320, 100),
        ] {
            let mut frame = Pixmap::new(w, h).unwrap();
            screen.render(&mut frame, Page::Main, &state, &Histories::default());
            screen.render(&mut frame, Page::Boards, &state, &Histories::default());
        }
    }

    #[test]
    fn a_setpoint_step_is_one_degree_fahrenheit() {
        assert_eq!(fahrenheit(51.1, 0), "124\u{00B0}F");
        assert_eq!(fahrenheit(51.1 + SETPOINT_STEP_C, 0), "125\u{00B0}F");
    }
}
