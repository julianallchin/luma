//! A deck as a strip, the way a DJ's booth screen shows one: what is loaded
//! and where it is on the left, and a scrolling waveform on the right with
//! the playhead fixed in the middle, the beats and bars ticked across it.
//!
//! Positions come from a poll every 100 ms. Between polls a playing deck runs
//! on from the last reading, so the waveform scrolls smoothly; the paint asks
//! for the next frame while any deck plays.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{canvas, div, fill, point, px, size, Bounds, Hsla, Pixels, Window};
use luma_lib::models::beat_grid::BeatGrid;
use luma_lib::models::perform::{DeckMatch, PerformDeck};
use luma_lib::models::waveforms::{BandEnvelopes, TrackWaveform};
use luma_ui::node::{Instrument as _, Role};
use luma_ui::{glass, ladder};

/// Seconds of track either side of the playhead.
const SPAN: f32 = 6.;
const STRIP_HEIGHT: f32 = 64.;
const INFO_WIDTH: f32 = 220.;

/// What a strip draws for one library track, read once.
pub(crate) struct TrackView {
    bands: Option<BandEnvelopes>,
    duration: f32,
    beats: Option<BeatGrid>,
}

impl TrackView {
    pub(crate) fn new(waveform: Option<TrackWaveform>, beats: Option<BeatGrid>) -> Self {
        let (bands, duration) = match waveform {
            Some(waveform) => (
                waveform.bands.or(waveform.preview_bands),
                waveform.duration_seconds as f32,
            ),
            None => (None, 0.),
        };
        Self {
            bands,
            duration,
            beats,
        }
    }

    /// Bar and beat at `t`, both from 1, when the track has a grid.
    fn bar_beat(&self, t: f32) -> Option<(usize, usize)> {
        let grid = self.beats.as_ref()?;
        let bar = grid.downbeats.iter().take_while(|at| **at <= t).count();
        let last_downbeat = grid
            .downbeats
            .iter()
            .rev()
            .find(|at| **at <= t)
            .copied()
            .unwrap_or(f32::MIN);
        let beat = grid
            .beats
            .iter()
            .filter(|at| **at >= last_downbeat - 1e-3 && **at <= t)
            .count();
        Some((bar.max(1), beat.max(1)))
    }
}

/// The library track a deck is driving, when it matched one.
pub(crate) fn track_id(deck: &PerformDeck) -> Option<&str> {
    match &deck.track {
        DeckMatch::Matched { track_id, .. } => Some(track_id),
        _ => None,
    }
}

/// One deck's strip. `polled_at` is when its position was read; `view` is
/// the matched track's waveform and grid, once loaded.
pub(crate) fn strip(
    deck: &PerformDeck,
    polled_at: Instant,
    view: Option<std::rc::Rc<TrackView>>,
    controls: Option<gpui::AnyElement>,
) -> impl IntoElement {
    let (status, live) = match &deck.track {
        DeckMatch::Unknown => ("Nothing loaded".to_string(), false),
        DeckMatch::Matching => ("Matching…".to_string(), false),
        DeckMatch::Matched {
            has_score: true, ..
        } => ("Score driving".to_string(), true),
        DeckMatch::Matched { .. } => ("No score in this venue".to_string(), false),
        DeckMatch::Unmatched => ("Not in your library".to_string(), false),
        DeckMatch::Failed { message } => (message.clone(), false),
    };
    let position = deck.position as f32;
    let playing = deck.playing;
    let title = if deck.title.is_empty() {
        "—".to_string()
    } else {
        deck.title.clone()
    };
    let seconds = position.max(0.) as u64;
    let mut readout = format!("{}:{:02}", seconds / 60, seconds % 60);
    if deck.bpm > 0. {
        readout.push_str(&format!("   {:.1} BPM", deck.bpm));
    }
    if let Some((bar, beat)) = view.as_ref().and_then(|view| view.bar_beat(position)) {
        readout.push_str(&format!("   {bar}.{beat}"));
    }
    let name = format!("Deck {}", deck.id);
    let info = div()
        .w(px(INFO_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(10.5))
                .text_color(ladder::foreground_alpha(0.5))
                .child(name.clone())
                .child(div().size(px(6.)).rounded_full().bg(if live {
                    Hsla::from(ladder::primary())
                } else {
                    glass::hairline(0.2)
                }))
                .child(status.clone()),
        )
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ladder::foreground())
                .truncate()
                .child(title.clone()),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.55))
                .truncate()
                .child(deck.artist.clone()),
        )
        .child(
            div()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.8))
                .child(format!("{} {readout}", if playing { "▶" } else { "❚❚" })),
        )
        .children(controls);
    let wave = canvas(
        move |_, _, _| (),
        move |bounds, (), window, _| {
            let now = if playing {
                position + polled_at.elapsed().as_secs_f32()
            } else {
                position
            };
            paint_waveform(bounds, view.as_deref(), now, window);
            if playing {
                window.request_animation_frame();
            }
        },
    )
    .flex_1()
    .h(px(STRIP_HEIGHT));
    div()
        .w_full()
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(14.))
        .rounded(px(luma_ui::radius::ROW))
        .bg(glass::card_bg())
        .border_1()
        .border_color(if live {
            Hsla::from(ladder::primary()).opacity(0.6)
        } else {
            glass::hairline(0.08)
        })
        .child(info)
        .child(
            div()
                .flex_1()
                .h(px(STRIP_HEIGHT))
                .rounded(px(4.))
                .overflow_hidden()
                .bg(gpui::black().opacity(0.35))
                .child(wave),
        )
        .agent_node(
            Role::Card,
            format!("{name}: {title} · {readout} · {status}"),
        )
}

/// The waveform around `now`: band envelopes mirrored about the middle,
/// beats as faint ticks, bars as brighter ones, the playhead fixed at the
/// centre.
fn paint_waveform(bounds: Bounds<Pixels>, view: Option<&TrackView>, now: f32, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let mid_y = f32::from(bounds.origin.y) + height / 2.;
    let left = f32::from(bounds.origin.x);
    let seconds_per_px = 2. * SPAN / width.max(1.);
    let x_of = |t: f32| left + (t - (now - SPAN)) / seconds_per_px;

    if let Some(view) = view {
        if let (Some(bands), true) = (&view.bands, view.duration > 0.) {
            let layers: [(&Vec<f32>, Hsla); 3] = [
                (&bands.low, ladder::waveform_low().into()),
                (&bands.mid, ladder::waveform_mid().into()),
                (&bands.high, ladder::waveform_high().into()),
            ];
            let step = 2.;
            let mut x = 0.;
            while x < width {
                let t = now - SPAN + x * seconds_per_px;
                if t >= 0. && t < view.duration {
                    for (band, color) in layers {
                        if band.is_empty() {
                            continue;
                        }
                        let index = ((t / view.duration) * band.len() as f32) as usize;
                        let value = band[index.min(band.len() - 1)].clamp(0., 1.);
                        let half = value * height * 0.46;
                        window.paint_quad(fill(
                            Bounds::new(
                                point(px(left + x), px(mid_y - half)),
                                size(px(step - 0.5), px(half * 2.)),
                            ),
                            color.opacity(0.85),
                        ));
                    }
                }
                x += step;
            }
        }
        if let Some(grid) = &view.beats {
            let window_start = now - SPAN;
            let window_end = now + SPAN;
            for at in grid
                .beats
                .iter()
                .filter(|at| **at >= window_start && **at <= window_end)
            {
                let bar = grid.downbeats.iter().any(|down| (down - at).abs() < 1e-3);
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(x_of(*at)), bounds.origin.y),
                        size(px(1.), bounds.size.height),
                    ),
                    gpui::white().opacity(if bar { 0.35 } else { 0.1 }),
                ));
            }
        }
    }
    // The playhead.
    window.paint_quad(fill(
        Bounds::new(
            point(px(left + width / 2. - 1.), bounds.origin.y),
            size(px(2.), bounds.size.height),
        ),
        Hsla::from(ladder::primary()),
    ));
}

/// The tapped tempo as a strip, for a set with no deck to follow: a live
/// band. Its BPM and bar, a light per beat of the bar that runs on between
/// polls, and `controls` (tap, nudges).
pub(crate) fn tempo_strip(
    tempo: luma_lib::models::perform::Tempo,
    polled_at: Instant,
    controls: gpui::AnyElement,
) -> impl IntoElement {
    let beats = canvas(
        move |_, _, _| (),
        move |bounds, (), window, _| {
            let beat = tempo.beat + polled_at.elapsed().as_secs_f64() * tempo.bpm / 60.;
            let in_bar = beat.rem_euclid(4.);
            let lit = in_bar.floor() as usize;
            // The light fades through the beat, so the pulse reads at a glance.
            let fade = 1. - in_bar.fract() as f32;
            let side = f32::from(bounds.size.height);
            for index in 0..4 {
                let x = f32::from(bounds.origin.x) + index as f32 * (side + 6.);
                let color = if index == lit {
                    Hsla::from(ladder::primary()).opacity(0.35 + 0.65 * fade)
                } else {
                    glass::hairline(0.15)
                };
                window.paint_quad(
                    fill(
                        Bounds::new(point(px(x), bounds.origin.y), size(px(side), px(side))),
                        color,
                    )
                    .corner_radii(px(3.)),
                );
            }
            window.request_animation_frame();
        },
    )
    .w(px(4. * 14. + 3. * 6.))
    .h(px(14.));
    let bar = (tempo.beat / 4.).floor() as i64 + 1;
    let readout = format!("{:.1} BPM", tempo.bpm);
    div()
        .w_full()
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(14.))
        .rounded(px(luma_ui::radius::ROW))
        .bg(glass::card_bg())
        .border_1()
        .border_color(glass::hairline(0.08))
        .child(
            div()
                .w(px(INFO_WIDTH))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(ladder::foreground_alpha(0.5))
                        .child("Live · tapped tempo"),
                )
                .child(
                    div()
                        .font_family(luma_ui::fonts::MONO)
                        .text_size(px(15.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(ladder::foreground())
                        .child(format!("{readout}   bar {bar}")),
                ),
        )
        .child(beats)
        .child(div().flex_1())
        .child(controls)
        .agent_node(Role::Card, format!("Tapped tempo: {readout}"))
}
