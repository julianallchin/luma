//! The side sheet: a panel that slides in over one edge of a region and
//! leaves the rest of it alone.
//!
//! # It is not a dialog, and the difference is the whole point
//!
//! [`crate::dialog::Host`] owns a *plane*: a scrim over the shell, a focus
//! trap, and an occluder across the whole viewport. That is right for a card
//! that has to be answered before anything else can happen, and wrong for an
//! inspector — an inspector is read *while* working the thing it inspects, so
//! the timeline underneath has to keep taking clicks, keys and wheel the whole
//! time the sheet is up. So there is no scrim, no trap, and the only occluder
//! is the sheet's own box: a click inside it belongs to the sheet, a click
//! anywhere else belongs to whatever is under the pointer.
//!
//! What follows from that is the *retarget*: a sheet whose subject changes
//! does not close and reopen, it simply draws something else. A dialog cannot
//! do that — its identity is the card — which is why "click another clip"
//! would bounce through an exit and an entrance if this were built on
//! [`crate::dialog::Popup`].
//!
//! # The slide is a pane width
//!
//! Openness is [`crate::pane::PaneWidth`] holding *how many pixels of the
//! sheet are on screen*, and everything else is derived from it: the sheet is
//! present while that is above zero, and interactive only while its
//! [`crate::pane::PaneWidth::target`] is the full width. A tween rather than an
//! [`gpui::Animation`] for the reason that module states — gpui keys an
//! animation's clock by its element-id path, so a remount replays it — and
//! reusing it rather than restating it means reversal mid-slide, the reduced
//! motion policy and the wall-clock evaluation are all already decided.
//!
//! That also gives the exit for free: the state behind a leaving sheet is what
//! its owner chooses to keep, and the tween's own presence is the signal for
//! how long to keep it. A leaving sheet is a **ghost** — painted, untouchable.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Pixels, SharedString};

use crate::node::{Instrument, Role};
use crate::{glass, radius};

/// How wide a sheet is. One width for every sheet, from the same band as the
/// spec's other side surfaces — wide enough for a labelled control per row,
/// narrow enough that the region behind it is still worth looking at.
pub const WIDTH: f32 = 320.0;

/// The sheet's inner gutter. Its content is a column of full-width rows, so
/// this is the only horizontal measurement any of them need.
pub const PAD: f32 = 16.0;

/// How wide a full-bleed control inside a sheet is: the width, less both
/// gutters and the rim the gutters sit inside.
pub const CONTENT_WIDTH: f32 = WIDTH - 2.0 * PAD - 1.0;

/// The frame around a sheet's content — see the module docs.
///
/// A struct rather than an argument list for the same reason
/// [`crate::dialog::Host`] is one: none of these are the content, they are the box,
/// and a call site reading `Sheet { interactive: false, .. }` can see which
/// box it built.
pub struct Sheet {
    /// What the sheet is called in the agent tree.
    pub label: SharedString,
    /// The sheet's own width — what the content is laid out at, for the whole
    /// slide, so text is revealed rather than re-wrapped forty times.
    pub width: f32,
    /// How much of that is on screen this frame:
    /// [`crate::pane::PaneWidth::eval`].
    pub revealed: Pixels,
    /// False for a sheet on its way out. Every hitbox below is gated on it: a
    /// user who clears the selection and immediately clicks where the sheet
    /// was must land on the timeline, not on the corpse.
    pub interactive: bool,
}

impl Sheet {
    /// Paint the sheet over the **right** edge of the positioned ancestor it
    /// is a child of, with `body` laid out at the full width.
    #[must_use]
    pub fn render(self, body: AnyElement) -> AnyElement {
        // Round the free edge, leave the meeting edges sharp (`radius`'s
        // mixed-corner rule): the sheet is flush to its region's right, top
        // and bottom, and only its left side is an outline of its own.
        let card = div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded_tl(px(radius::MODAL))
            .rounded_bl(px(radius::MODAL))
            .bg(glass::overlay())
            .border_l_1()
            .border_color(glass::hairline(0.10))
            .child(body);
        let card = if self.interactive {
            // The sheet's box is the *only* thing it takes off the region
            // behind it. Without this a click on a control in the sheet would
            // also reach the timeline underneath — which clears the selection,
            // and would therefore close the sheet on every edit.
            card.occlude()
        } else {
            card
        };
        // No backdrop blur, and that follows from the fill rather than being a
        // separate decision: a blur is how a *translucent* surface says it is
        // translucent, and behind [`glass::overlay`]'s full coverage there is
        // nothing to see through it — the pass would cost an offscreen target
        // and a gaussian every frame to change no pixel. The dialog card is
        // the other case: a modal is large enough that some of the backdrop
        // showing through is the point. An inspector is not; it is read
        // against the timeline it edits, and the timeline is the busiest
        // surface in the app to put 12px labels over.

        // The slide, as a clip rather than a transform: a right-anchored
        // window onto a full-width card. gpui divs have no translate, and a
        // clip is what the card's own rounded edge needs anyway — it rides in
        // with the card instead of being drawn at a moving offset.
        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(self.revealed)
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(self.width))
                    .child(card),
            )
            .agent_node(Role::Card, self.label)
            .into_any_element()
    }
}
