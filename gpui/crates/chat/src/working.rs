//! What a turn looks like while it is still thinking.
//!
//! # Why it is a trailer and not a status bar
//!
//! Comet puts this under the row the turn is writing into, so the indicator
//! sits exactly where the reply will land and scrolls with it. A strip pinned
//! over the composer is a second place to look, and it is the wrong one: the
//! eye is already on the tail of the transcript waiting for text.
//!
//! There is therefore **one** working affordance in the panel. The status strip
//! keeps only its resting context line — a strip that also spun would be two
//! answers to "is it running?" that could disagree.
//!
//! # The spinner
//!
//! Comet's gradient matrix: a 3×3 grid of round cells tinted per row from a
//! sunrise gradient, each pulsing on a phase offset taken from its distance to
//! the wave origin, so the wave reads as travelling up and out. The pulse shape
//! and the shared 30fps clock are [`luma_ui::motion`]'s — the clock matters,
//! because the repeating-animation form of this pinned the whole window at the
//! display's refresh rate for as long as one turn was live.
//!
//! Driving the clock is also what makes [`elapsed`] tick: the lease repaints
//! the panel while a spinner is mounted, so the timer needs no second timer.

use std::time::Instant;

use gpui::{div, prelude::*, px, AnyElement, EntityId, SharedString};
use luma_ui::motion;
use luma_ui::node::{Instrument as _, Role as NodeRole};

use crate::theme::{self, Theme};

/// What the turn is doing, in the only two tenses the panel can tell apart.
///
/// The distinction is not cosmetic: until the model's first row exists there is
/// nothing to time, and a timer counting the round trip out would be measuring
/// the network and calling it thought.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Working {
    /// The prompt is on its way and no assistant row has opened yet.
    Sending,
    /// The model is answering. Timed from the moment the turn started.
    Thinking,
}

/// The working indicator, as one row's trailer.
#[derive(Clone, Copy, Debug)]
pub struct Trailer {
    pub state: Working,
    /// When the turn began — the timer's origin, not this row's.
    pub since: Instant,
    /// Which word the rotation starts on, so two threads working at once do
    /// not chant in unison. Derived from the thread id.
    pub seed: u64,
}

/// The rotating vocabulary. Twenty words is enough that a long turn does not
/// visibly loop, and the point of them is to say "still working, not stuck"
/// without claiming to know what the model is doing.
pub const FLAVOUR_WORDS: [&str; 20] = [
    "Thinking",
    "Pondering",
    "Scheming",
    "Brewing",
    "Weaving",
    "Tinkering",
    "Musing",
    "Composing",
    "Sifting",
    "Untangling",
    "Distilling",
    "Sketching",
    "Plotting",
    "Riffing",
    "Combobulating",
    "Percolating",
    "Marinating",
    "Noodling",
    "Puzzling",
    "Conjuring",
];

/// How long each word holds before the next.
pub const FLAVOUR_ROTATE_SECS: u64 = 7;

/// The word for a turn `elapsed_secs` old.
#[must_use]
pub fn flavour_word(seed: u64, elapsed_secs: u64) -> &'static str {
    let step = elapsed_secs / FLAVOUR_ROTATE_SECS;
    let count = FLAVOUR_WORDS.len() as u64;
    FLAVOUR_WORDS[(seed.wrapping_add(step) % count) as usize]
}

/// A stable per-thread seed. FNV-1a, because the only property wanted is that
/// two thread ids rarely start on the same word.
#[must_use]
pub fn flavour_seed(thread_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in thread_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Elapsed time, in the shortest form that stays honest.
#[must_use]
pub fn elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// The grid's side. Square, so the wave has a diagonal to travel.
const MATRIX_SIDE: usize = 3;

/// The sunrise gradient, sampled once per row: cool blue → amber → pink.
///
/// The one place the chat paints a hue. It is not on the grey ladder and must
/// not be: the indicator's whole job is to be the thing that moves and is not
/// the surface, and three greys pulsing on a near-black ground read as dirt.
const ROW_TINTS: [u32; MATRIX_SIDE] = [0x00B6_D3EF, 0x00ED_B185, 0x00F8_88A0];

/// One cell's edge.
const CELL: f32 = 2.5;

/// Comet's gradient matrix, on the shared pulse clock.
///
/// `view` leases the clock: the panel repaints at 30fps while this is mounted
/// and schedules nothing once it is gone.
fn spinner(view: EntityId, cx: &mut gpui::App) -> impl IntoElement {
    let delta = motion::pulse_delta(&motion::GRADIENT_SPIN, view, cx);
    let center = (MATRIX_SIDE as f32 - 1.0) / 2.0;
    let max = MATRIX_SIDE as f32 - 1.0 + center;
    div()
        .flex()
        .flex_col()
        .gap(px(CELL / 2.0))
        .children((0..MATRIX_SIDE).map(move |row| {
            let tint: gpui::Hsla = gpui::rgb(ROW_TINTS[row]).into();
            div()
                .flex()
                .flex_row()
                .gap(px(CELL / 2.0))
                .children((0..MATRIX_SIDE).map(move |col| {
                    // Distance from the wave origin — the bottom centre —
                    // normalized into this cell's phase offset.
                    let distance =
                        MATRIX_SIDE as f32 - 1.0 - row as f32 + (col as f32 - center).abs();
                    let phase = distance / (max + 1.0);
                    div()
                        .size(px(CELL))
                        .rounded(px(CELL / 2.0))
                        .bg(tint)
                        .opacity(motion::gspin_opacity(delta + phase, motion::GSPIN_DIM))
                }))
        }))
}

/// The name the automation tree reports for each state.
///
/// The *state*, never the flavour word: a driver waiting for a turn to finish
/// has to name the condition, and a name that rotated every seven seconds would
/// make that impossible to write. The rotating word is presentation and carries
/// no node — the same split as the send button, whose node says "Stop" while it
/// paints a square.
impl Working {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Working::Sending => "Sending",
            Working::Thinking => "Working",
        }
    }
}

/// The trailer: spinner, word, and — once there is something to time — how
/// long it has been going.
pub fn trailer(trailer: &Trailer, theme: &Theme, view: EntityId, cx: &mut gpui::App) -> AnyElement {
    let secs = trailer.since.elapsed().as_secs();
    let (word, timer) = match trailer.state {
        Working::Sending => ("Sending".to_string(), None),
        Working::Thinking => (
            format!("{}…", flavour_word(trailer.seed, secs)),
            Some(elapsed(secs)),
        ),
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_SM))
        .pt(px(theme::SPACE_SM))
        .text_size(px(12.0))
        .child(spinner(view, cx))
        .child(
            div()
                .text_color(theme.text_muted)
                .child(SharedString::from(word)),
        )
        .children(timer.map(|timer| {
            div()
                .text_color(theme.text_faint)
                .child(SharedString::from(timer))
        }))
        .agent_node(NodeRole::Text, trailer.state.label())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_holds_for_its_rotation_and_then_moves_on() {
        let seed = flavour_seed("thread-1");
        assert_eq!(flavour_word(seed, 0), flavour_word(seed, 6));
        assert_ne!(flavour_word(seed, 0), flavour_word(seed, 7));
        // …and it wraps rather than running off the end.
        let long = FLAVOUR_ROTATE_SECS * FLAVOUR_WORDS.len() as u64;
        assert_eq!(flavour_word(seed, 0), flavour_word(seed, long));
    }

    /// Two threads working at once should not chant the same word.
    #[test]
    fn the_seed_separates_threads() {
        assert_ne!(flavour_seed("a"), flavour_seed("b"));
    }

    /// The name a driver waits on does not rotate with the word it paints.
    #[test]
    fn the_reported_state_is_stable_while_the_word_turns() {
        let seed = flavour_seed("thread-1");
        assert_ne!(flavour_word(seed, 0), flavour_word(seed, 7));
        assert_eq!(Working::Thinking.label(), "Working");
        assert_eq!(Working::Sending.label(), "Sending");
    }

    #[test]
    fn the_timer_grows_a_minutes_place() {
        assert_eq!(elapsed(0), "0s");
        assert_eq!(elapsed(59), "59s");
        assert_eq!(elapsed(60), "1m 0s");
        assert_eq!(elapsed(3661), "61m 1s");
    }
}
