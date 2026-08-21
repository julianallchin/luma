//! Luma's design system, in GPUI.
//!
//! The web app's UI contract (CLAUDE.md, "UI design system") is a brutalist
//! instrument panel: a six-step grey ladder carries the whole hierarchy, depth
//! comes from stacked planes separated by darker trim, corners are square,
//! nothing animates, and there is exactly *one* style per control. This crate
//! is that contract expressed once for the native stack, so the screenshot
//! harness and the real app cannot render two different buttons.
//!
//! # Interface
//!
//! Every control is a free function returning a [`gpui::Div`] the caller
//! composes into its own layout:
//!
//! ```ignore
//! use luma_ui::{ladder, luma_button};
//!
//! div().bg(ladder::background()).child(luma_button("Import Tracks", false))
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
//! the harness compares against WebKit. Carets and text selection are not
//! ported — `luma_input` renders a value, it does not edit one — and neither
//! is a slider's drag. `<Select>`'s open menu is here ([`luma_select_menu`]);
//! `luma_dropdown` still renders its closed trigger only.

pub mod fonts;
pub mod ladder;
pub mod node;
pub mod paint;

mod button;
mod checkbox;
mod dropdown;
mod input;
mod select;
mod slider;
mod toggle;

pub use button::{luma_button, slab};
pub use checkbox::luma_checkbox;
pub use dropdown::luma_dropdown;
pub use input::luma_input;
pub use select::{luma_select, luma_select_item, luma_select_menu, luma_selector};
pub use slider::luma_slider;
pub use toggle::{luma_toggle, luma_toggle_group, luma_toggle_segment};
