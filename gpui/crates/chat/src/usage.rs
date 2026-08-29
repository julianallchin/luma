//! How full the context window is, under the composer.
//!
//! # What it measures
//!
//! One number: the *last request's* prompt against the model's window. Not a
//! running total over the thread — a turn resends the whole conversation, so
//! the newest request's prompt already **is** the thread, and summing steps
//! would count every earlier turn once per turn that followed it.
//!
//! The prompt is [`RequestUsage::prompt_tokens`], not `input_tokens`: cached
//! prefix tokens are context the model read, and on a warm thread they are
//! nearly all of it. See that method for why a gauge fed from `input_tokens`
//! reads near-empty right up to the limit.
//!
//! # Where it comes from
//!
//! The transcript, and nothing else. `luma_lib::agent::apply` writes each
//! step's accounting into the durable `data-pi-message` part, and
//! [`Transcript::last_request`] reads the newest one back — so a thread
//! reopened tomorrow shows exactly what it showed while it streamed, with no
//! state kept beside the transcript to fall out of step with it.
//!
//! # Why a ring
//!
//! It is a *background* fact. A ring at the text's own height says "there is a
//! ceiling and you are here" in the space of a glyph, and says nothing louder
//! until it has something louder to say — which is [`HOT`], where the fill
//! takes the danger hue.

use gpui::{
    div, prelude::*, px, AnyElement, Entity, Hsla, PathBuilder, Pixels, Point, SharedString, Window,
};
use luma_lib::agent::RequestUsage;
use luma_ui::float;
use luma_ui::node::{Instrument as _, Role as NodeRole};

use crate::theme::{self, Theme};
use crate::AgentChat;

/// The gauge's box. A glyph's height: it reads as punctuation on the status
/// line, not as a control sitting on it.
pub const DIAMETER: f32 = 14.0;
/// Track and fill share one width — a fill heavier than its track reads as a
/// slider handle, and this is not draggable.
const STROKE: f32 = 1.5;
/// Where the fill stops being a fact and starts being a warning. The one place
/// the gauge spends a hue.
const HOT: f32 = 0.8;
/// Segments in the full circle. Enough that the polyline's corners fall below
/// a physical pixel at this diameter.
const SEGMENTS: usize = 48;

/// The ring, and the hover that discloses [`open_card`].
///
/// Whether the card is open is held by the panel rather than derived from a
/// hover fade: the card is a disclosure, and a disclosure that decays on a
/// timer would flicker shut while the pointer sat still. It is also why the
/// card is not a child of this element — it hangs off the footer instead, and
/// only the flag travels between them.
pub fn gauge(request: &RequestUsage, chat: &Entity<AgentChat>, theme: &Theme) -> impl IntoElement {
    let fraction = request.fraction();
    let hovered = chat.clone();
    let label: SharedString = match fraction {
        Some(fraction) => format!("Context {}%", percent(fraction)).into(),
        // No window means no gauge reading, and the label says so rather than
        // naming a percentage of nothing.
        None => "Context usage".into(),
    };
    div()
        .id("chat-context-gauge")
        .size(px(DIAMETER))
        .flex_none()
        .on_hover(move |over, _, cx| {
            let over = *over;
            hovered.update(cx, |chat, cx| chat.set_usage_open(over, cx));
        })
        .child(ring(fraction, theme))
        .agent_node(NodeRole::Text, label)
}

/// The card the ring discloses, hung above the footer it is placed in.
///
/// Placed against the *footer* — composer and status strip as one block —
/// rather than against the ring, which is the only reason the card clears the
/// composer at all: a 300px card hung off a 14px trigger inside the strip has
/// nowhere to go but over the field above it, whatever corner it anchors to.
/// The ring stays the hover trigger; only the rect the card is measured
/// against moves. Its caller renders it as a child of that footer.
///
/// The clearance is zero because there is nothing to clear: the argument buys
/// a flipped card its way off the element it hangs from, and the footer is
/// docked to the window's bottom edge, so a card that cannot fit above it has
/// no room below it either — gpui snaps it rather than switching sides.
pub fn open_card(request: &RequestUsage, theme: &Theme) -> AnyElement {
    // Hover discloses it and hover takes it away — see [`float::Dismiss`].
    float::anchored_above(
        "chat-context-card",
        0.0,
        float::Dismiss::Never,
        card(request, theme),
    )
}

/// The ring itself: a track at full circle, a fill from twelve o'clock.
///
/// Painted rather than composed from elements because an arc is not a box —
/// and painted with no tween: a gauge that animated toward its new value would
/// be showing a number that was never true.
fn ring(fraction: Option<f32>, theme: &Theme) -> impl IntoElement {
    // Track `border_strong`, fill `text`: the pair has to read as *two* values
    // at a 1.5px stroke on a 14px circle, and at that width a muted fill sits
    // close enough to the track that the arc's end goes hunting. The fill is
    // ink, so it takes the foreground; the track is structure, so it stays on
    // the border family.
    //
    // `border_strong`, not `border`: a hairline meant to separate two lit
    // surfaces disappears entirely when it is the only thing on a dark ground,
    // and a gauge whose empty half is invisible cannot be read as a fraction —
    // it reads as an arc of unknown span. Measured at 1.5px on the panel's
    // ground, `border` (8% white) lands within noise of the ground itself.
    let track = theme.border_strong;
    let fill = fraction.map(|fraction| {
        (
            fraction,
            if fraction >= HOT {
                theme.danger
            } else {
                theme.text
            },
        )
    });
    gpui::canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let radius = (DIAMETER - STROKE) / 2.0;
            let center = gpui::point(
                bounds.origin.x + px(DIAMETER / 2.0),
                bounds.origin.y + px(DIAMETER / 2.0),
            );
            stroke(window, arc(center, radius, 1.0), track);
            if let Some((fraction, color)) = fill {
                if fraction > 0.0 {
                    stroke(window, arc(center, radius, fraction), color);
                }
            }
        },
    )
    .size(px(DIAMETER))
}

/// Points along `turns` of a circle from twelve o'clock, clockwise — the
/// direction a reader expects a dial to fill.
fn arc(center: Point<Pixels>, radius: f32, turns: f32) -> Vec<Point<Pixels>> {
    let turns = turns.clamp(0.0, 1.0);
    // At least one segment, so a fraction too small to round to a segment
    // still paints a mark rather than vanishing.
    let steps = ((SEGMENTS as f32 * turns).ceil() as usize).max(1);
    (0..=steps)
        .map(|step| {
            let theta = std::f32::consts::TAU * turns * (step as f32 / steps as f32)
                - std::f32::consts::FRAC_PI_2;
            gpui::point(
                center.x + px(radius * theta.cos()),
                center.y + px(radius * theta.sin()),
            )
        })
        .collect()
}

fn stroke(window: &mut Window, points: Vec<Point<Pixels>>, color: Hsla) {
    let Some((first, rest)) = points.split_first() else {
        return;
    };
    let mut path = PathBuilder::stroke(px(STROKE));
    path.move_to(*first);
    for point in rest {
        path.line_to(*point);
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// What the ring is a picture of, in full.
///
/// Every row the provider reported, in the order the money is spent: what went
/// in, what came back out of the cache, what went into it, what came back. The
/// window and the model are last because they are the *denominator* — a reader
/// who only wants the fraction already has it, in the ring they are pointing at.
fn card(request: &RequestUsage, theme: &Theme) -> gpui::AnyElement {
    let usage = request.usage;
    let mut rows = vec![
        ("Input", tokens(usage.input_tokens)),
        ("Cache read", tokens(usage.cache_read_input_tokens)),
        ("Cache write", tokens(usage.cache_creation_input_tokens)),
        ("Output", tokens(usage.output_tokens)),
        ("Prompt", tokens(request.prompt_tokens())),
    ];
    rows.push((
        "Window",
        request
            .model
            .map_or_else(|| "—".to_string(), |model| tokens(model.context_window())),
    ));
    rows.push((
        "Model",
        request
            .model
            .map_or_else(|| "unknown".to_string(), |model| model.key().to_string()),
    ));
    if let Some(duration) = request.duration {
        rows.push(("Took", elapsed(duration)));
    }

    float::popover_card()
        .min_w(px(196.0))
        .p(px(theme::SPACE_MD))
        .gap(px(theme::SPACE_XS))
        .children(
            rows.into_iter()
                .map(|(label, value)| row(label, &value, theme)),
        )
        .into_any_element()
}

/// One label/value pair: silkscreen on the left, a monospace value hard right.
///
/// Right-aligned and mono together are what let the eye read the column as
/// numbers — a proportional face puts every digit at its own width, so a stack
/// of counts stops lining up at the thousands.
fn row(label: &'static str, value: &str, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::SPACE_LG))
        .child(luma_ui::silkscreen_in(
            label.to_uppercase(),
            theme.text_faint,
        ))
        .child(
            div()
                .font_family(theme.font_mono.clone())
                .text_size(px(11.0))
                .text_color(theme.text)
                .child(SharedString::from(value.to_string()))
                // Named `LABEL VALUE` so one automation node carries the pair:
                // a driver reading a bare "626,800" out of the tree cannot say
                // which row it fell out of.
                .agent_node(
                    NodeRole::Text,
                    SharedString::from(format!("{} {value}", label.to_uppercase())),
                ),
        )
}

/// Grouped in threes. A six-figure token count read as one run of digits is a
/// number nobody checks.
fn tokens(count: impl Into<u64>) -> String {
    let digits = count.into().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Sub-second in milliseconds, past that in seconds to one decimal — the two
/// scales a single request actually lands in.
fn elapsed(duration: std::time::Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1_000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", duration.as_secs_f32())
    }
}

/// The ring's fraction as whole percent, floored: a gauge that rounded 99.6%
/// up to 100 would say "full" while the next turn still fits.
fn percent(fraction: f32) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = (fraction.clamp(0.0, 1.0) * 100.0) as u32;
    percent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_grouped_in_threes_from_the_right() {
        assert_eq!(tokens(0_u64), "0");
        assert_eq!(tokens(999_u64), "999");
        assert_eq!(tokens(1_000_u64), "1,000");
        assert_eq!(tokens(176_048_u64), "176,048");
        assert_eq!(tokens(1_000_000_u64), "1,000,000");
    }

    /// The two scales, and the boundary between them.
    #[test]
    fn a_duration_reads_in_the_unit_it_landed_in() {
        assert_eq!(elapsed(std::time::Duration::from_millis(7)), "7 ms");
        assert_eq!(elapsed(std::time::Duration::from_millis(999)), "999 ms");
        assert_eq!(elapsed(std::time::Duration::from_millis(1_000)), "1.0 s");
        assert_eq!(elapsed(std::time::Duration::from_millis(7_500)), "7.5 s");
    }

    /// Never rounds up into a claim the window cannot support.
    #[test]
    fn the_percent_floors() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.996), 99);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(1.4), 100);
    }

    /// The arc walks clockwise from twelve o'clock, and a fraction too small
    /// to fill a segment still paints one — a gauge that vanished at 1% would
    /// be indistinguishable from a gauge that had no reading at all.
    #[test]
    fn the_arc_starts_at_the_top_and_never_collapses_to_nothing() {
        let center = gpui::point(px(0.0), px(0.0));
        let quarter = arc(center, 10.0, 0.25);
        let first = quarter.first().expect("an arc has a start");
        assert!(f32::from(first.x).abs() < 0.001, "starts on the vertical");
        assert!(f32::from(first.y) < 0.0, "starts above the centre");
        let last = quarter.last().expect("an arc has an end");
        assert!(f32::from(last.x) > 0.0, "a quarter turn ends to the right");
        assert!(arc(center, 10.0, 0.0001).len() >= 2, "still a line");
    }
}
