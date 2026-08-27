//! The corner vocabulary — every radius this app is allowed to round to.
//!
//! # Panes are square, floats are round
//!
//! A pane is *structure*: it tiles the window, its edges are shared with its
//! neighbours, and a rounded edge on a shared boundary leaves a notch of the
//! plane below showing through. Panes therefore stay square, and depth between
//! two of them is a seam ([`crate::ladder::trim`]) rather than a corner.
//!
//! A float is *detached*: a menu, a popover, a dialog card, a chip. Nothing is
//! tiled against it, so its outline is its own — and a rounded outline is what
//! says "this is a separate object sitting on top" without spending a shadow.
//!
//! # Mixed corners
//!
//! When something is a float on some edges and a pane on others — a header
//! strip fused to the top of a card, a rail welded to a card's left edge — the
//! **free** edges round and the **meeting** edges stay sharp. A strip inside a
//! [`MODAL`] card rounds its two outer corners to [`MODAL`] and leaves the two
//! that touch the card's body at zero. Never round a meeting edge "a little";
//! the pair of surfaces has one boundary and it is either a corner or a seam.
//!
//! # The ladder
//!
//! Seven steps, and a new corner picks one rather than inventing a number.
//! They ascend with the size of the thing: a key cap is smaller than a button,
//! a button smaller than a row, a row smaller than the card it sits in.

/// The smallest plate there is — an inline-code wash, an album-art thumbnail,
/// a checkbox, a close-hotspot: anything smaller than a key cap.
pub const CHIP: f32 = 4.0;

/// Key caps and the smallest inline chips — the footer legend's `⌘`, `esc`.
pub const CAP: f32 = 5.0;

/// Small square controls: icon buttons, segmented toggles, skeleton
/// placeholders. Comet's `CONTROL_RADIUS`.
pub const CONTROL: f32 = 6.0;

/// Menu rows, text-button chips, search-field frames — anything the pointer
/// lands on inside a float.
pub const ROW: f32 = 8.0;

/// An in-pane card or section panel. Comet's `PANEL_RADIUS`.
pub const PANEL: f32 = 10.0;

/// A floating card: popover, menu surface, dropdown. Comet's popover
/// `CARD_RADIUS`.
pub const CARD: f32 = 12.0;

/// The modal palette's card — the add-track and change-venue dialogs. Two
/// steps above [`ROW`] so a full-bleed row inside the card still reads as
/// nested rather than flush.
pub const MODAL: f32 = 14.0;

/// The largest float there is: a message bubble, a lightbox frame.
pub const BUBBLE: f32 = 16.0;

/// The capsule: half the composer plate's 52px rest height, so the one control
/// the eye lands on reads as a pill rather than as a card.
pub const PILL: f32 = 26.0;

/// Every step, once — the guard the tests hold new radii against.
pub const LADDER: &[f32] = &[CHIP, CAP, CONTROL, ROW, PANEL, CARD, MODAL, BUBBLE, PILL];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_ascends_without_repeating_a_step() {
        for pair in LADDER.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?} is out of order or duplicated");
        }
    }

    /// The vocabulary is closed on purpose: a radius that is not on the ladder
    /// is a design decision, not a literal, and this is where it gets made.
    /// The middle seven are comet's; [`CHIP`] and [`PILL`] are Luma's own two
    /// decisions, made here rather than as literals at their call sites.
    #[test]
    fn the_vocabulary_is_closed() {
        assert_eq!(LADDER, &[4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 26.0]);
    }
}
