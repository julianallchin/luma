//! The noise preview: what a Noise source gives, drawn with the patterns
//! crate's own [`noise_value`], so it shows what playback does.
//!
//! Over time, three heads at 15 %, 50 % and 85 % along a line rig draw a line
//! each over 16 beats, Low at the bottom and High at the top. Along the rig,
//! 48 heads show their level at the playhead's beat of the clip, so the strip
//! shows what the lights show there. A clump grain groups the heads as a rig
//! would. A setting that follows a source of its own is held at a fixed
//! stand-in, and the preview says so.
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, hsla, point, px, size, App, Bounds, Context, Div, Hsla, PathBuilder, Pixels,
    SharedString, Window,
};
use luma_patterns::{noise_value, NoiseSettings, NoiseSource, Value};

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
    source: Option<NoiseSource>,
    /// The input's path as the form names it: the default noise key.
    path: String,
    /// Values are levels 0–1, so a head shows its value as it is rather than
    /// its place between Low and High.
    levels: bool,
    transport: TransportSource,
    drawn: Instant,
    watching: bool,
}

impl NoisePreview {
    pub fn new(
        id: impl Into<SharedString>,
        path: impl Into<String>,
        levels: bool,
        transport: TransportSource,
    ) -> Self {
        Self {
            id: id.into(),
            source: None,
            path: path.into(),
            levels,
            transport,
            drawn: Instant::now(),
            watching: false,
        }
    }

    /// A host-side write: the source as stored now, or `None` when it does
    /// not read as a Noise source.
    pub fn set_value(&mut self, source: Option<NoiseSource>, cx: &mut Context<Self>) {
        if self.source != source {
            self.source = source;
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

/// `source`'s numeric settings as fixed numbers, and the names of those that
/// follow a source of their own with the stand-in each is shown at.
fn settings(source: &NoiseSource) -> (NoiseSettings, Vec<String>) {
    let mut held = Vec::new();
    let mut fixed = |name: &str, value: &Value, stand_in: f64, unit: &str| {
        value.scalar_value().unwrap_or_else(|| {
            held.push(format!("{name} at {}{unit}", format_value(stand_in)));
            stand_in
        })
    };
    let settings = NoiseSettings {
        speed: fixed("Speed", &source.speed, 4., " beats"),
        scale: source
            .scale
            .as_ref()
            .map_or(0.5, |scale| fixed("Scale", scale, 0.5, "")),
        contrast: fixed("Contrast", &source.contrast, 0., ""),
        range: [
            fixed("Low", &source.range[0], 0., ""),
            fixed("High", &source.range[1], 1., ""),
        ],
    };
    (settings, held)
}

/// The preview rig: head `i`'s grain unit, the unit's first head and its
/// place along the rig. Positions span the unit centres, as a rig's do.
fn unit_of(i: usize, grain: usize) -> (usize, f64) {
    let size = grain.max(1);
    let first = i - i % size;
    let units = HEADS.div_ceil(size);
    let unit = first / size;
    let place = if units > 1 {
        unit as f64 / (units - 1) as f64
    } else {
        0.5
    };
    (first, place)
}

struct Reading {
    settings: NoiseSettings,
    source: NoiseSource,
    path: String,
    seed: u64,
}

impl Reading {
    /// Head `i`'s value at `beats`, or `None` when the noise cannot be read.
    fn at(&self, i: usize, beats: f64) -> Option<f64> {
        let (first, place) = unit_of(i, self.source.grain.size());
        noise_value(
            &self.source,
            self.settings,
            &self.path,
            self.seed,
            &format!("preview:{first}"),
            [place, 0.5],
            beats,
        )
        .ok()
        .filter(|v| v.is_finite())
    }
}

/// A title in the verb/detail pattern: the name bright, the detail dim.
fn title(name: &'static str, detail: String) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(6.))
        .text_size(px(11.))
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
        let Some(source) = self.source.clone() else {
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
        let (settings, held) = settings(&source);
        let reading = Rc::new(Reading {
            settings,
            source,
            path: self.path.clone(),
            seed: transport.seed,
        });
        // The graph pages through time 16 beats at a time, with a cursor at
        // the preview's beat.
        let page = (beat / SPAN).floor() * SPAN;
        let cursor = ((beat - page) / SPAN) as f32;
        let [low, high] = settings.range;
        let (bottom, top) = (low.min(high), low.max(high));
        let height = |v: f64| {
            if top - bottom < 1e-9 {
                0.5
            } else {
                ((v - bottom) / (top - bottom)).clamp(0., 1.)
            }
        };
        let lines: Vec<Vec<Option<f64>>> = SAMPLED
            .iter()
            .map(|at| {
                let head = (at * (HEADS - 1) as f64).round() as usize;
                (0..=STEPS)
                    .map(|k| {
                        reading
                            .at(head, page + SPAN * k as f64 / STEPS as f64)
                            .map(height)
                    })
                    .collect()
            })
            .collect();
        let levels = self.levels;
        let heads: Vec<Option<f64>> = (0..HEADS)
            .map(|i| {
                reading
                    .at(i, beat)
                    .map(|v| if levels { v.clamp(0., 1.) } else { height(v) })
            })
            .collect();
        let readable = heads.iter().all(Option::is_some);
        let graph = div()
            .relative()
            .w_full()
            .h(px(GRAPH_H))
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
                    .ml(px(-0.5))
                    .w(px(1.))
                    .bg(ladder::foreground_alpha(0.35)),
            )
            .agent_node(Role::Text, format!("{id} over time"));
        let rig = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| paint_heads(window, bounds, &heads),
        )
        .w_full()
        .h(px(RIG_H))
        .agent_node(Role::Text, format!("{id} along the rig"));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(9.))
            .rounded(px(crate::radius::ROW))
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
                    .text_size(px(11.))
                    .text_color(ladder::foreground_alpha(0.5))
                    .child("These settings give no noise to show")
            }))
            .children((!held.is_empty()).then(|| {
                div()
                    .text_size(px(11.))
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
    let mut path = PathBuilder::stroke(px(1.25));
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
            size: size(width - px(1.), bounds.size.height),
        };
        let color = level.map_or(ladder::foreground_alpha(0.06), head_color);
        window.paint_quad(fill(cell, color).corner_radii(px(2.)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clump heads share a unit and a place; the places span the rig.
    #[test]
    fn clumps_group_the_preview_heads() {
        assert_eq!(unit_of(0, 1), (0, 0.));
        assert_eq!(unit_of(HEADS - 1, 1), (HEADS - 1, 1.));
        assert_eq!(unit_of(5, 4), unit_of(4, 4));
        assert_ne!(unit_of(8, 4), unit_of(4, 4));
        assert_eq!(unit_of(HEADS - 1, 8).1, 1.);
    }
}
