//! GPUI port of `<Slider>` (src/shared/components/ui/slider.tsx).
//!
//! Not a thumb-on-a-track slider: the app's is an Ableton-style value box —
//! a recessed `--input` slab with a `--primary` fill bar at `opacity-20`
//! covering value% of the *content* box, and the numeric value drawn over it
//! in 10px mono. The web version's range `<input>` is invisible, so the
//! captured frame is exactly these three layers.
//!
//! # The drag lives here, not at the call sites
//!
//! For a long time this function painted those three layers and nothing else —
//! the web control's interaction was the invisible `<input type=range>`, and
//! porting the picture did not port the behaviour. Every slider in the app was
//! therefore a picture of a value: the settings dialog said so in a comment,
//! and the renderer lab grew a pair of nudge buttons beside each one to make up
//! for it. Nothing had regressed; the drag had simply never been written.
//!
//! It is written *here* rather than at each call site because the arithmetic
//! (pointer to fraction to value, against the box's live width) is the same
//! everywhere and is the part that is easy to get subtly wrong. A caller says
//! what the value is, what it is bounded by, and what to do with a new one.
//!
//! # Absolute, and only while dragging
//!
//! The bar is a *fill* — its width is the value — so dragging maps the pointer
//! to a position in the box, not to a delta from where the drag began. What the
//! picture promises and what the pointer does are then the same statement.
//!
//! A press with no movement changes nothing. That is not a limitation to route
//! around: this control also sets Art-Net dimmer levels, and a stray click
//! landing on a slab should not slam a rig to full. Movement is consent.

use gpui::*;

use crate::arg::{drag_fraction, OwnedDrag};
use crate::drag::DragGhost;
use crate::ladder;
use crate::node::{Instrument, Role};

/// Height of the value box, and how it reads to the pointer.
const HEIGHT: f32 = 28.;

/// A slider drag in flight, and *which* slider is in it.
///
/// gpui routes a drag by the type of its payload, so every slider in the window
/// shares this one type and hears every slider's drag. The id is what makes a
/// listener sure the move belongs to it — without it, dragging one slider moves
/// all of them.
#[derive(Clone)]
struct SliderDrag {
    id: SharedString,
}

impl OwnedDrag for SliderDrag {
    fn owner(&self) -> &SharedString {
        &self.id
    }
}

/// The value box, live: `on_change` is called with the value the pointer is
/// asking for, already clamped into `min..=max`.
///
/// `id` must be unique among the sliders on screen — it is both the element's
/// id and how this slider recognises its own drag.
///
/// The value handed over is **absolute**, not a delta, because a drag that
/// outruns the box would otherwise wind up: the pointer keeps travelling, the
/// deltas keep arriving, and the control spends the first half of the journey
/// back doing nothing. Clamping a position cannot wind up.
pub fn luma_slider(
    id: impl Into<SharedString>,
    value: f32,
    min: f32,
    max: f32,
    width: f32,
    on_change: impl Fn(f32, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let fraction = ((value - min) / (max - min)).clamp(0., 1.);
    let moved = id.clone();
    slab(&id, value, fraction, width)
        .id(ElementId::Name(id.clone()))
        .cursor_ew_resize()
        .on_drag(SliderDrag { id }, |_, _, _, cx| {
            // The press belongs to the slider, not to whatever it sits on: a
            // lab panel over a 3D stage would otherwise start orbiting the
            // camera under the drag.
            cx.stop_propagation();
            cx.new(|_| DragGhost)
        })
        .on_drag_move(drag_fraction(
            moved,
            move |at, _: &SliderDrag, window, cx| {
                on_change(min + (max - min) * at.x, window, cx);
            },
        ))
}

/// The three layers, at rest.
fn slab(id: &SharedString, value: f32, fraction: f32, width: f32) -> Div {
    div()
        .relative()
        .overflow_hidden()
        .flex_shrink_0()
        .w(px(width))
        .h(px(HEIGHT))
        .border_1()
        .border_color(ladder::control_border())
        .bg(ladder::apex())
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(relative(fraction))
                .bg(ladder::primary())
                .opacity(0.2),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .px(px(8.))
                .flex()
                .items_center()
                // The nameable mono face — see [`crate::fonts::MONO`]: asking
                // for SF Mono by its marketing name matches nothing and falls
                // back to the proportional UI face, which is a readout whose
                // digits jitter as they change.
                .font_family(crate::fonts::MONO)
                .text_size(px(10.))
                // Foreground where the web side writes `--primary`: a hued
                // readout was the one numeric text in the panel that wasn't
                // white-on-ladder, and one canonical readout beats byte
                // parity here. A deliberate divergence from the reference —
                // the WebKit comparison shot will show it until the web
                // slider follows.
                .text_color(ladder::foreground())
                .child(format!("{value}"))
                // The number the box draws, published. A slider's own node
                // carries only its name, so without this a driver can move one
                // and has no way to read what it moved to — and a caller that
                // solved that by folding the value into the *slider's* label
                // would break every `find` that addresses it by name. The
                // reading is named for its control so two sliders showing the
                // same number stay tellable apart.
                .agent_node(Role::Text, format!("{id} = {value}")),
        )
}
