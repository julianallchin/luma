//! The noise preview: what a noise node gives, drawn with the host's own
//! sampler — the patterns crate's noise — so it shows what playback does.
//!
//! Over time, three heads at 15 %, 50 % and 85 % along a line rig draw a line
//! each over 16 beats, 0 at the bottom and 1 at the top. Along the rig, 48
//! heads show their value at the playhead's beat of the clip, so the strip
//! shows what the lights show there. A setting that follows a wire of its
//! own is held at a fixed stand-in, and the preview says so.
use crate::rpx;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, hsla, point, size, App, Bounds, Context, Div, Hsla, PathBuilder, Pixels,
    SharedString, Window,
};

use super::number::format_value;
use crate::{
    ladder,
    node::{Instrument, Role},
};

/// Where the clip's transport is, each frame the preview draws.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transport {
    /// The clip's seed.
    pub seed: u64,
    /// Beats since the clip start, the time origin playback evaluates the
    /// noise from; 0 while the playhead is outside the clip.
    pub beat: f64,
    /// The transport runs, so the beat moves between notifies.
    pub playing: bool,
}

/// Reads the clip's transport.
pub type TransportSource = Rc<dyn Fn(&App) -> Transport>;

/// A noise node's settings as fixed numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Beats per turn of the noise clock.
    pub period: f64,
    /// The blob size, a share of the rig; `None` is one value for all heads.
    pub scale: Option<f64>,
    pub contrast: f64,
    /// The settings that follow a wire, each with the stand-in it is shown
    /// at, such as "period at 4 beats".
    pub held: Vec<String>,
}

/// The noise at a head: the settings, the clip's seed, the head's place
/// (U, V, Z, 0–1 over the rig) and the beats since the clip start. `None`
/// when the settings give no noise.
pub type Sampler = Rc<dyn Fn(&Settings, u64, [f64; 3], f64) -> Option<f64>>;

/// Beats the line graph spans.
const SPAN: f64 = 16.;
/// Heads along the rig strip.
const HEADS: usize = 48;
/// Where the graph's three heads sit along the rig.
const SAMPLED: [f64; 3] = [0.15, 0.5, 0.85];
/// Points per graph line.
const STEPS: usize = 128;
const GRAPH_H: f32 = 58.;
const RIG_H: f32 = 14.;
/// The longest wait between two drawn frames of the animation.
const FRAME: Duration = Duration::from_millis(33);

pub struct NoisePreview {
    id: SharedString,
    settings: Option<Settings>,
    sampler: Sampler,
    transport: TransportSource,
    drawn: Instant,
    watching: bool,
}

impl NoisePreview {
    pub fn new(id: impl Into<SharedString>, sampler: Sampler, transport: TransportSource) -> Self {
        Self {
            id: id.into(),
            settings: None,
            sampler,
            transport,
            drawn: Instant::now(),
            watching: false,
        }
    }

    /// A host-side write: the node's settings as stored now.
    pub fn set_value(&mut self, settings: Settings, cx: &mut Context<Self>) {
        if self.settings.as_ref() != Some(&settings) {
            self.settings = Some(settings);
            cx.notify();
        }
    }

    /// While the transport runs and the preview is drawn, redraw it each
    /// [`FRAME`]. Once the transport stops it draws once more, at the beat
    /// where it stopped, and asks for nothing: a paused track or a closed
    /// sheet costs no frames. A seek while paused redraws with the sheet.
    fn watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.watching {
            return;
        }
        self.watching = true;
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |preview, cx| {
                preview.watching = false;
                if !(preview.transport)(cx).playing || preview.drawn.elapsed() >= FRAME {
                    cx.notify();
                } else {
                    preview.watch(window, cx);
                }
            });
        });
    }
}

/// Head `i`'s place along the preview rig, 0–1.
fn place(i: usize) -> f64 {
    i as f64 / (HEADS - 1) as f64
}

/// A title in the verb/detail pattern: the name bright, the detail dim.
fn title(name: &'static str, detail: String) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(rpx(6.))
        .text_size(rpx(11.))
        .child(
            div()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ladder::foreground_alpha(0.7))
                .child(name),
        )
        .child(
            div()
                .text_color(ladder::foreground_alpha(0.4))
                .child(detail),
        )
}

/// The three graph lines: tints of the accent, lightest first.
fn line_color(index: usize) -> Hsla {
    let accent: Hsla = ladder::accent().into();
    let lift = [0.16, 0.04, -0.08][index % 3];
    hsla(accent.h, accent.s, (accent.l + lift).clamp(0., 1.), 0.95)
}

/// A head at `level`, 0–1: black to a warm white.
fn head_color(level: f64) -> Hsla {
    hsla(36. / 360., 0.55, level.clamp(0., 1.) as f32 * 0.86, 1.)
}

impl Render for NoisePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.clone();
        let Some(settings) = self.settings.clone() else {
            return div().agent_node(Role::Card, format!("{id} preview"));
        };
        let transport = (self.transport)(cx);
        // Reduced motion still shows the playhead's beat; it only asks for
        // no frames of its own.
        if transport.playing && !crate::motion::reduced_motion(cx) {
            self.watch(window, cx);
        }
        self.drawn = Instant::now();
        let beat = transport.beat;
        let sampler = self.sampler.clone();
        let at = |i: usize, beats: f64| {
            sampler(&settings, transport.seed, [place(i), 0.5, 0.5], beats)
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0., 1.))
        };
        // The graph pages through time 16 beats at a time, with a cursor at
        // the preview's beat.
        let page = (beat / SPAN).floor() * SPAN;
        let cursor = ((beat - page) / SPAN) as f32;
        let lines: Vec<Vec<Option<f64>>> = SAMPLED
            .iter()
            .map(|share| {
                let head = (share * (HEADS - 1) as f64).round() as usize;
                (0..=STEPS)
                    .map(|k| at(head, page + SPAN * k as f64 / STEPS as f64))
                    .collect()
            })
            .collect();
        let heads: Vec<Option<f64>> = (0..HEADS).map(|i| at(i, beat)).collect();
        let held = settings.held.clone();
        let readable = heads.iter().all(Option::is_some);
        let graph = div()
            .relative()
            .w_full()
            .h(rpx(GRAPH_H))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        super::strip::paint_grid(window, bounds, SPAN, false);
                        for (index, line) in lines.iter().enumerate() {
                            paint_line(window, bounds, line, line_color(index));
                        }
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui::relative(cursor))
                    .ml(rpx(-0.5))
                    .w(rpx(1.))
                    .bg(ladder::foreground_alpha(0.35)),
            )
            .agent_node(Role::Text, format!("{id} over time"));
        let rig = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| paint_heads(window, bounds, &heads),
        )
        .w_full()
        .h(rpx(RIG_H))
        .agent_node(Role::Text, format!("{id} along the rig"));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(rpx(6.))
            .p(rpx(9.))
            .rounded(rpx(crate::radius::ROW))
            .border_1()
            .border_color(crate::glass::hairline(0.08))
            .bg(crate::glass::ink(0.03))
            .child(title(
                "Over time",
                format!("{} beats, 3 heads", format_value(SPAN)),
            ))
            .child(graph)
            .child(title("Along the rig", format!("{HEADS} heads")))
            .child(rig)
            .children((!readable).then(|| {
                div()
                    .text_size(rpx(11.))
                    .text_color(ladder::foreground_alpha(0.5))
                    .child("These settings give no noise to show")
            }))
            .children((!held.is_empty()).then(|| {
                div()
                    .text_size(rpx(11.))
                    .text_color(ladder::foreground_alpha(0.4))
                    .child(format!("Shown with {}", held.join(", ")))
            }))
            .agent_node(Role::Card, format!("{id} preview"))
    }
}

/// Stroke `values`, 0–1 with y up, evenly across `bounds`; a missing value
/// breaks the line.
fn paint_line(window: &mut Window, bounds: Bounds<Pixels>, values: &[Option<f64>], color: Hsla) {
    let last = values.len().saturating_sub(1).max(1) as f32;
    let mut path = PathBuilder::stroke(gpui::px(1.25 * crate::rem_scale(window)));
    let mut drawing = false;
    let mut any = false;
    for (k, value) in values.iter().enumerate() {
        let Some(v) = value else {
            drawing = false;
            continue;
        };
        let at = point(
            bounds.origin.x + bounds.size.width * (k as f32 / last),
            bounds.origin.y + bounds.size.height * (1. - *v as f32),
        );
        if drawing {
            path.line_to(at);
        } else {
            path.move_to(at);
            drawing = true;
        }
        any = true;
    }
    if any {
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

/// One cell per head across `bounds`, a pixel apart.
fn paint_heads(window: &mut Window, bounds: Bounds<Pixels>, heads: &[Option<f64>]) {
    let count = heads.len().max(1) as f32;
    let width = bounds.size.width / count;
    for (i, level) in heads.iter().enumerate() {
        let cell = Bounds {
            origin: point(bounds.origin.x + width * i as f32, bounds.origin.y),
            size: size(
                width - gpui::px(crate::rem_scale(window)),
                bounds.size.height,
            ),
        };
        let color = level.map_or(ladder::foreground_alpha(0.06), head_color);
        window.paint_quad(fill(cell, color).corner_radii(gpui::px(2. * crate::rem_scale(window))));
    }
}
