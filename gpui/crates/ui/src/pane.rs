//! A region of the shell whose width animates, and the handle that drags it.
//!
//! Both edge regions share this: the sidebar and the workspace panel. Each
//! wants the same two behaviours — a width that eases between two values, and
//! a seam the mouse can pull — so they are stated once here rather than twice
//! in the shell. Where a region's gutter to its neighbour goes is the shell's
//! business, not this module's.
//!
//! # Why the tween is evaluated by hand
//!
//! Not `with_animation`: gpui keys an animation element's start time by its
//! full global element-id path, so a wrapper that mounts or remounts (a tab
//! swap, or an ancestor animation keyed by a fresh epoch) silently replays the
//! tween from t=0. Evaluating it manually keeps the element tree's shape
//! constant — a finished or absent tween is exactly the steady state, however
//! the tree around it remounts.
//!
//! # Why the content is laid out at the target width
//!
//! [`pane`] clips a **fixed-width inner** inside a container whose width is the
//! animated one. The content is therefore laid out at its final width for the
//! whole transition, so a sliding panel reveals text rather than re-wrapping it
//! forty times on the way in.

use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::drag::DragGhost;
use crate::motion::{self, SURFACE};

/// A width that is on its way from one value to another.
///
/// Held by the region rather than by the element, because the element is
/// rebuilt every frame and the tween's start instant must outlive that.
#[derive(Debug, Clone, Copy)]
pub struct PaneWidth {
    /// Where the region rests when nothing is moving. Also where a finished
    /// tween lands, so the two can never disagree.
    target: f32,
    moving: Option<Tween>,
}

#[derive(Debug, Clone, Copy)]
struct Tween {
    from: f32,
    started: Instant,
}

impl PaneWidth {
    /// A region resting at `width`.
    #[must_use]
    pub fn new(width: f32) -> Self {
        Self {
            target: width,
            moving: None,
        }
    }

    /// Send the region to `to`, starting from wherever it is *now* — so
    /// reversing mid-slide reverses from the visible width rather than
    /// snapping back to where the last slide began.
    ///
    /// Already heading there is not a new slide: this is safe to call every
    /// frame with a target derived from the shell's state, which is how the
    /// toggles avoid keeping animation bookkeeping of their own.
    ///
    /// Under reduced motion the region simply *is* at `to`.
    pub fn retarget(&mut self, to: f32, cx: &App) {
        if (self.target - to).abs() < f32::EPSILON {
            return;
        }
        if motion::reduced_motion(cx) {
            self.set(to);
            return;
        }
        let from = self.current();
        self.target = to;
        self.moving = Some(Tween {
            from,
            started: Instant::now(),
        });
    }

    /// Where the region is heading. Answering "is it open" from this rather
    /// than from a second boolean is what stops the two disagreeing mid-slide.
    #[must_use]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Set the width with no animation — a drag, which is already continuous.
    pub fn set(&mut self, width: f32) {
        self.target = width;
        self.moving = None;
    }

    /// Whether the region is at rest rather than part-way through a slide.
    ///
    /// What it is for: a width that is *derived* from something else already
    /// moving (a panel sized against the sliding sidebar) must be [`set`], not
    /// [`retarget`]ed — a tween whose destination changes every frame restarts
    /// every frame, so it trails its own source and lands in a lurch. A caller
    /// with such a width asks this, and only animates the toggle.
    ///
    /// [`set`]: Self::set
    /// [`retarget`]: Self::retarget
    #[must_use]
    pub fn settled(&self) -> bool {
        self.moving.is_none()
    }

    /// This frame's width, without asking for another frame. Callers that are
    /// rendering want [`Self::eval`]; this is for arithmetic (a takeover panel
    /// sized against the sidebar's live width).
    #[must_use]
    pub fn current(&self) -> f32 {
        let Some(Tween { from, started }) = self.moving else {
            return self.target;
        };
        motion::lerp(from, self.target, motion::exit_progress(&SURFACE, started))
    }

    /// This frame's width, asking for the next frame while still moving.
    ///
    /// The request is the reason this takes a `Window`: a manually evaluated
    /// tween has nothing else driving redraws, so a pane that did not ask
    /// would freeze part-way open until something else happened to notify.
    pub fn eval(&mut self, window: &mut Window) -> Pixels {
        let width = self.current();
        if (width - self.target).abs() > 0.5 {
            window.request_animation_frame();
        } else {
            self.moving = None;
        }
        px(width)
    }
}

/// A region at `width`, clipping `inner` laid out at `content` — see the
/// module docs for why the two differ during a slide.
///
/// The content also fades with the slide ([`motion::reveal_opacity`]), and the
/// openness it fades on is exactly `width / content` — the tween this function
/// is already holding both ends of. That is why the fade lives here and not at
/// the call sites: a caller would have to be handed the same two numbers to
/// recompute the same ratio, and a caller that forgot would be the one region
/// whose content pops.
pub fn pane(width: Pixels, content: Pixels, inner: AnyElement) -> Div {
    let target = f32::from(content);
    let openness = if target > 0.0 {
        f32::from(width) / target
    } else {
        1.0
    };
    div().h_full().flex_none().overflow_hidden().w(width).child(
        div()
            .h_full()
            .w(content)
            .flex()
            .flex_col()
            // Only while it is actually arriving: a resting region should
            // carry no opacity at all rather than a redundant ×1.
            .when(openness < 1.0, |content| {
                content.opacity(motion::reveal_opacity(openness))
            })
            .child(inner),
    )
}

/// Which boundary a seam is: the axis it *moves along*, not the one it spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seam {
    /// A vertical rule between two side-by-side regions, dragged left/right.
    Vertical,
    /// A horizontal rule between two stacked regions, dragged up/down.
    Horizontal,
}

/// A seam the mouse can pull, floating over the boundary at zero layout size,
/// centred on the rule at `at` — an offset along the seam's own axis, in the
/// coordinates of the region the seam divides.
///
/// gpui's drag-and-drop rather than a mouse-move listener: the drag survives
/// the pointer leaving the 5px strip, which a hover-scoped listener would not.
/// The marker type `M` is what tells the root's `on_drag_move` *which* seam is
/// moving, so every seam shares this one implementation.
///
/// Double-click resets — `reset` is a plain fn pointer because there is exactly
/// one behaviour per seam and a closure would only be a place to capture state
/// this element must not hold.
///
/// # Who owns the pointer
///
/// A grip has to be thicker than the rule it pulls — a 1px target is not
/// aimable — so it overhangs both neighbours. gpui hit-tests in **paint order
/// only**: every hitbox under the pointer is hovered at once unless one of
/// them says otherwise, so an overhanging grip and the surface beneath it both
/// took the same press. That is one bug, not two: pressing the stage/editor
/// seam orbited the stage *and* the same press on the workspace seam reached
/// the panel behind it.
///
/// Two halves make the grip the sole owner of its strip, and both are
/// necessary:
///
/// - [`InteractiveElement::block_mouse_except_scroll`] takes the pointer from
///   everything painted *behind* it. Scroll is deliberately still let through:
///   a 5px strip is not a scrollable thing, and a wheel that died on the seam
///   would be a dead band across whatever it divides.
/// - The caller mounts this as the **last child of the region it divides**,
///   absolutely positioned at `at`, which is what makes "behind it" mean both
///   neighbours rather than only the one that happened to paint first. Mounted
///   inside the rule instead, the grip would own the pane above it and share
///   the pane below it.
///
/// Last within its *region*, not deferred to the top of the window: an overlay
/// (a dialog, a menu) is painted after the shell and must keep the pointer it
/// covers, seam included.
pub fn resize_handle<M: 'static, V: Render>(
    id: &'static str,
    seam: Seam,
    at: f32,
    marker: fn() -> M,
    reset: fn(&mut V, &mut Context<V>),
    hover: Hsla,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let lead = px(at - HANDLE_WIDTH / 2.0);
    div()
        .id(id)
        .flex_none()
        .absolute()
        .map(|handle| match seam {
            Seam::Vertical => handle
                .w(px(HANDLE_WIDTH))
                .h_full()
                .top_0()
                .left(lead)
                .cursor_col_resize(),
            Seam::Horizontal => handle
                .h(px(HANDLE_WIDTH))
                .w_full()
                .left_0()
                .top(lead)
                .cursor_row_resize(),
        })
        .block_mouse_except_scroll()
        .hover(move |s| s.bg(hover))
        .on_drag(marker(), |_, _: Point<Pixels>, _, cx| {
            cx.stop_propagation();
            cx.new(|_| DragGhost)
        })
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                if event.click_count == 2 {
                    reset(this, cx);
                    cx.notify();
                }
            }),
        )
}

/// How thick the grab strip is, across whichever axis it spans. Narrow enough
/// to read as a seam, wide enough to hit without aiming.
pub const HANDLE_WIDTH: f32 = 5.0;

/// A fade band at one edge of a scrolling region, so content dissolves under
/// the chrome above or below it rather than being cut off by it.
///
/// `top` and `bottom` are sized independently because the two edges of the
/// thread column meet different chrome: the titlebar overlays one and the
/// composer stack the other.
pub fn edge_fade(band: f32, color: Hsla, top: bool) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(band))
        .when(top, |el| el.top_0())
        .when(!top, |el| el.bottom_0())
        .bg(linear_gradient(
            if top { 180. } else { 0. },
            linear_color_stop(color, 0.),
            linear_color_stop(color.opacity(0.), 1.),
        ))
}
