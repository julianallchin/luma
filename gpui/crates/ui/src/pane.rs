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

use crate::motion::{self, RESIZE};

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

    /// This frame's width, without asking for another frame. Callers that are
    /// rendering want [`Self::eval`]; this is for arithmetic (a takeover panel
    /// sized against the sidebar's live width).
    #[must_use]
    pub fn current(&self) -> f32 {
        let Some(Tween { from, started }) = self.moving else {
            return self.target;
        };
        let span = RESIZE.total().mul_f32(motion::speed_scale());
        let raw = started.elapsed().as_secs_f32() / span.as_secs_f32();
        if raw >= 1.0 {
            return self.target;
        }
        motion::lerp(from, self.target, RESIZE.progress(raw))
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
pub fn pane(width: Pixels, content: Pixels, inner: AnyElement) -> Div {
    div()
        .h_full()
        .flex_none()
        .overflow_hidden()
        .w(width)
        .child(div().h_full().w(content).flex().flex_col().child(inner))
}

/// A seam the mouse can pull, floating over the boundary at zero layout width.
///
/// gpui's drag-and-drop rather than a mouse-move listener: the drag survives
/// the pointer leaving the 5px strip, which a hover-scoped listener would not.
/// The marker type `M` is what tells the root's `on_drag_move` *which* seam is
/// moving, so the two seams share this one implementation.
///
/// Double-click resets — `reset` is a plain fn pointer because there is exactly
/// one behaviour per seam and a closure would only be a place to capture state
/// this element must not hold.
pub fn resize_handle<M: 'static, V: Render>(
    id: &'static str,
    marker: fn() -> M,
    reset: fn(&mut V, &mut Context<V>),
    hover: Hsla,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(HANDLE_WIDTH))
        .h_full()
        .flex_none()
        .cursor_col_resize()
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

/// How wide the grab strip is. Narrow enough to read as a seam, wide enough to
/// hit without aiming.
pub const HANDLE_WIDTH: f32 = 5.0;

/// The thing gpui drags. Empty on purpose: the seam is not a card being
/// carried anywhere, and a visible ghost would be a second cursor.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

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
