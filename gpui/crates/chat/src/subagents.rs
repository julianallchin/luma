//! Delegation, as the reader sees it: an identicon, and a count.
//!
//! # Why an identicon and not an icon
//!
//! Every subagent runs the same tool, so a tool mark would draw the same
//! picture for all of them. What a reader needs to tell apart is *which
//! delegation* — a chip in the transcript, a row in the dialog and a face on
//! the floating pill are three sightings of one child, and the identicon is
//! what makes them recognisably the same one. It is seeded by the parent call
//! id rather than by the child thread id because the call id exists from the
//! first frame, before a child thread has been created; the two surfaces would
//! otherwise draw different faces for the same subagent while it started.
//!
//! # Live, never durable
//!
//! [`SubagentSnapshot`] arrives as [`TurnEvent::Subagent`](luma_lib::agent::TurnEvent)
//! and is never persisted — everything durable about a child is already a row
//! in the child's own thread. So the pill is a statement about *right now*: it
//! counts what is in flight, and it is absent when nothing is. Reopening a
//! thread mid-run shows the transcript's chips with no pill until the next
//! snapshot arrives, which is the honest reading of live state rather than a
//! remembered one.

use gpui::{div, prelude::*, px, AnyElement, Entity, Hsla, SharedString};
use luma_lib::agent::subagent::{SubagentPhase, SubagentSnapshot};
use luma_ui::node::{Instrument as _, Role as NodeRole};

use crate::theme::{self, Theme};
use crate::AgentChat;

/// The identicon's edge in a transcript chip and a dialog row. Matches
/// React's `size-4`, which is what the pill's geometry was measured against.
pub const AVATAR: f32 = 16.0;

/// Cells across the identicon. Odd, so the mirror axis is a column rather than
/// a seam between two.
const GRID: usize = 5;

/// The floating pill's height, and the gap between the faces on it.
const PILL_HEIGHT: f32 = 26.0;
const FACE_GAP: f32 = 4.0;
/// How many faces the pill shows before it stops counting in pictures. Past a
/// few the row is a smear, and the number beside them already says how many.
const MAX_FACES: usize = 4;

/// One subagent's face: a 5×5 grid mirrored about its centre column, seeded by
/// `seed`.
///
/// Painted rather than composed from elements because twenty-five nested divs
/// per chip is twenty-five layout boxes for a picture that is 16 pixels wide.
pub fn avatar(seed: &str, edge: f32) -> AnyElement {
    let hash = fnv1a(seed);
    let tint = tint(hash);
    div()
        .size(px(edge))
        .flex_none()
        .child(gpui::canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let cell = bounds.size.width / GRID as f32;
                for row in 0..GRID {
                    for column in 0..GRID {
                        // The mirror: the right half reads the left half's bit,
                        // which is what makes an identicon read as a face
                        // rather than as noise.
                        let source = column.min(GRID - 1 - column);
                        if (hash >> (row * (GRID / 2 + 1) + source)) & 1 == 0 {
                            continue;
                        }
                        let origin = gpui::point(
                            bounds.origin.x + cell * column as f32,
                            bounds.origin.y + cell * row as f32,
                        );
                        window.paint_quad(gpui::fill(
                            gpui::Bounds {
                                origin,
                                size: gpui::size(cell, cell),
                            },
                            tint,
                        ));
                    }
                }
            },
        ))
        .into_any_element()
}

/// FNV-1a over the seed's bytes. A hash, not a cipher: what it has to be is
/// stable across processes, which `DefaultHasher` explicitly is not.
fn fnv1a(seed: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The one hue in the panel that means *identity* rather than status.
///
/// Muted and mid-lightness so a face reads as a mark on the ground rather than
/// as a warning: the danger hue has to stay the loudest thing a chip can say.
fn tint(hash: u64) -> Hsla {
    Hsla {
        h: (hash >> 24 & 0xffff) as f32 / 65_535.0,
        s: 0.34,
        l: 0.62,
        a: 1.0,
    }
}

/// How many children are still in flight. `Merging` counts: a workspace being
/// published is work the reader is still waiting on.
#[must_use]
pub fn working(snapshots: &[SubagentSnapshot]) -> usize {
    snapshots
        .iter()
        .filter(|snapshot| {
            matches!(
                snapshot.phase,
                SubagentPhase::Running | SubagentPhase::Merging
            )
        })
        .count()
}

/// What the pill says, spelled once because the dialog's empty state and the
/// automation tree both read it.
#[must_use]
pub fn count_label(count: usize) -> SharedString {
    if count == 1 {
        "1 subagent working".into()
    } else {
        format!("{count} subagents working").into()
    }
}

/// The floating pill above the composer, or nothing when no child is running.
///
/// Hung off the *footer* by its caller, the way the context card is
/// ([`crate::usage::open_card`]): composer and status strip are one block, and
/// a pill measured against the strip alone would sit over the field.
pub fn pill(
    snapshots: &[SubagentSnapshot],
    chat: &Entity<AgentChat>,
    theme: &Theme,
) -> Option<AnyElement> {
    let count = working(snapshots);
    if count == 0 {
        return None;
    }
    let label = count_label(count);
    let opened = chat.clone();
    let faces = snapshots
        .iter()
        .filter(|snapshot| {
            matches!(
                snapshot.phase,
                SubagentPhase::Running | SubagentPhase::Merging
            )
        })
        .take(MAX_FACES)
        .map(|snapshot| avatar(&snapshot.call_id, AVATAR))
        .collect::<Vec<_>>();
    Some(luma_ui::float::anchored_above(
        "chat-subagents-pill",
        0.0,
        // A pill, not a menu: it is on this layer for its geometry and the
        // pointer has no say in its life.
        luma_ui::float::Dismiss::Never,
        div()
            .id("chat-subagents-pill-button")
            .h(px(PILL_HEIGHT))
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .gap(px(theme::SPACE_SM))
            .px(px(theme::SPACE_SM))
            // Square and unanimated: the pill is an instrument reading, and
            // the ladder's answer to depth is a value step, not a radius.
            .bg(theme::wash(0.10))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|style| style.bg(theme::wash(0.14)))
            .on_click(move |_, _, cx| {
                opened.update(cx, |this, cx| this.request_subagents(None, cx));
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .gap(px(FACE_GAP))
                    .children(faces),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(label.clone()),
            )
            .agent_node(NodeRole::Button, label)
            .into_any_element(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(call: &str, phase: SubagentPhase) -> SubagentSnapshot {
        SubagentSnapshot {
            child_thread_id: format!("child-{call}"),
            call_id: call.to_string(),
            description: "Fitting the ramp".into(),
            phase,
            activity: None,
        }
    }

    #[test]
    fn only_unfinished_children_are_counted() {
        let snapshots = [
            snapshot("a", SubagentPhase::Running),
            snapshot("b", SubagentPhase::Merging),
            snapshot("c", SubagentPhase::Completed),
            snapshot("d", SubagentPhase::Failed),
        ];
        assert_eq!(working(&snapshots), 2);
        assert_eq!(count_label(working(&snapshots)), "2 subagents working");
        assert_eq!(count_label(1), "1 subagent working");
    }

    /// The face is the only thing tying three surfaces to one child, so its
    /// seeding has to be stable across processes — see [`fnv1a`].
    #[test]
    fn a_seed_always_draws_the_same_face() {
        assert_eq!(fnv1a("call-1"), fnv1a("call-1"));
        assert_ne!(fnv1a("call-1"), fnv1a("call-2"));
    }
}
