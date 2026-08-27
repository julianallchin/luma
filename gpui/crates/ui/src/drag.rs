//! The empty drag ghost every value-dragging control shares.
//!
//! gpui's `on_drag` insists on a view to carry under the pointer. A slider, a
//! gradient stop or an SV reticle is not a card being carried anywhere — the
//! control repaints in place and a visible ghost would be a second cursor — so
//! all of them hand gpui the same nothing. One nothing, not one per module.

use gpui::{div, Context, Render, Window};

/// The thing gpui drags. Empty on purpose — see the module docs.
pub(crate) struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        div()
    }
}
