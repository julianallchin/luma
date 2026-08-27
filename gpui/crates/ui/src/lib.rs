//! Luma's design system, in GPUI.
//!
//! The UI contract (CLAUDE.md, "UI design system") is a brutalist instrument
//! panel: a grey ladder carries the whole hierarchy, corners are square,
//! nothing animates, and there is exactly *one* style per control. This crate
//! is that contract expressed once for the native stack, so the screenshot
//! harness and the real app cannot render two different buttons.
//!
//! Which *way* the ladder points is the one thing this stack does not share
//! with the web app: here the content ground is the darkest plane and chrome is
//! raised above it. See [`ladder`], which is where that lives and why.
//!
//! # Interface
//!
//! Every control is a free function returning a [`gpui::Div`] the caller
//! composes into its own layout:
//!
//! ```ignore
//! use luma_ui::{ladder, luma_button, Enabled};
//!
//! div().bg(ladder::background()).child(luma_button("Import Tracks", Enabled::Yes))
//! ```
//!
//! Returning `Div` rather than a component type is deliberate: these controls
//! hold no state, so there is nothing for a component to own, and a `Div` is
//! the one type every gpui layout already composes. A control that eventually
//! *does* need state (a real text input, an open dropdown menu) will grow an
//! `Entity`-backed sibling; until then, paying for one would buy nothing.
//!
//! [`ladder`] is the single source of truth for surface color. A hardcoded
//! `rgb(0x…)` anywhere above this module is a bug — see the module docs.
//! [`paint`] is the same for text on a screen that draws its own surface.
//!
//! # What is not here
//!
//! The ports cover the *resting appearance* of each control, which is what
//! the harness compares against WebKit. `luma_input` renders a value, it does
//! not edit one; a field that a person actually types into is
//! [`text_input::TextInput`], which is an entity rather than a free function
//! because an editor is exactly the case the note above reserves — it owns a
//! caret, a selection and an undo history. `<Select>`'s open menu is a
//! float ([`float::popover_card`] rows of [`luma_select_item`], hung by
//! [`float::anchored_below`]); `luma_dropdown` still renders its closed
//! trigger only.
//!
//! [`luma_slider`] used to be on that list and no longer is: it drags, and it
//! does so without owning state, because gpui's drag payload carries the
//! identity and the event carries the box — see its module docs.

pub mod arg;
pub mod dialog;
pub mod float;
pub mod fonts;
pub mod glass;
pub mod ladder;
pub mod motion;
pub mod node;
pub mod paint;
pub mod pane;
pub mod radius;
pub mod runtime;
pub mod split;
pub mod text_input;

mod button;
mod checkbox;
mod drag;
mod dropdown;
mod input;
mod select;
mod slider;
mod text;
mod toggle;

/// The key context a focused field declares while it is taking typed text.
///
/// It lives here — not in the app's keymap — because it is half of a pair: a
/// binding predicate on one side, a `key_context` on an element on the other,
/// and the two are written in different crates now that the chat panel has a
/// real text field. A name spelled differently in the two places is a binding
/// that silently never fires, so there is one spelling and both sides import
/// it.
pub const TEXT_INPUT: &str = "TextInput";

/// The height of every control that sits in a row of controls: a slab
/// trigger, a drafted number field, a float [`float::picker_chip`], a group
/// expression field.
///
/// Crate-level rather than any one tier's, because the tiers have to agree
/// about it — a chip and the field beside it share a baseline or the row
/// reads as two rows. It was spelled `24.` in three modules before this
/// existed, which is exactly how a baseline drifts.
pub const CONTROL_HEIGHT: f32 = 24.;

pub use button::{luma_button, slab, Enabled};
pub use checkbox::luma_checkbox;
pub use dropdown::luma_dropdown;
pub use input::luma_input;
pub use select::{luma_select, luma_select_item, luma_selector};
pub use slider::luma_slider;
pub use text::{plate, silkscreen, silkscreen_in};
pub use toggle::{luma_toggle, luma_toggle_group, luma_toggle_segment};
