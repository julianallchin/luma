//! The tri-mode color arg: inherit / override / mix, with an HSV picker.
//!
//! # The wire encodes the mode in alpha; the API does not
//!
//! A stored color arg is `(rgb, a)` where the alpha channel carries the
//! *mode*: `a ≤ 0` inherit, `a ≥ 1` override, anything between is a mix whose
//! amount is the alpha itself (the web reference:
//! `src/features/track-editor/components/inspector-panel.tsx`). That encoding
//! is confined to [`ColorArg::decode`] / [`ColorArg::encode`] — everything
//! above them speaks [`ColorMode`], so no widget, host or test ever compares
//! an alpha against a threshold again. Parse, don't validate: `decode` is
//! total, and `encode` clamps a degenerate mix amount back into the open
//! interval rather than letting it silently change mode on the wire.
//!
//! # Hue is working state, not value
//!
//! RGB is the value; HSV is how a picker addresses it — and the mapping loses
//! hue at zero saturation and saturation at zero value. A picker that
//! re-derived HSV from RGB each frame would snap its hue slider to red the
//! moment saturation hits zero. So [`ColorArgEditor`] is an entity: it keeps
//! the working [`Hsv`] across edits and only collapses it to RGB in the value
//! it emits. An external [`ColorArgEditor::set_value`] resyncs the working
//! state, accepting that loss — the host is telling us the value changed under
//! us, and a stale hue would be a lie.

use gpui::prelude::*;
use gpui::{
    div, linear_color_stop, linear_gradient, px, App, Context, Div, ElementId, EventEmitter, Hsla,
    Rgba, SharedString, Window,
};

use crate::drag::DragGhost;
use crate::ladder;
use crate::node::{AgentNode, Instrument, Role};

use super::select::luma_arg_select;
use super::{drag_fraction, OwnedDrag};
use crate::CONTROL_HEIGHT;

// -- the model ---------------------------------------------------------------

/// How a color arg applies against what it would inherit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorMode {
    /// Use the inherited color; this arg's rgb is retained but dormant.
    Inherit,
    /// Use this arg's rgb outright.
    Override,
    /// Blend toward this arg's rgb by the carried amount, exclusive `0..1`
    /// on the wire — [`ColorArg::encode`] holds that boundary.
    Mix(f32),
}

/// A color arg as a widget or host handles it: an rgb (each channel `0..=1`)
/// plus a [`ColorMode`]. The rgb is meaningful in every mode — switching
/// inherit → override must restore the color that was there before.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorArg {
    pub rgb: [f32; 3],
    pub mode: ColorMode,
}

/// Where a mix amount lands when a stored alpha would collapse it into
/// inherit/override, and the default when entering mix mode cold. The web
/// side's `0.5` fallback.
const MIX_DEFAULT: f32 = 0.5;

/// One 8-bit step: the closest a mix may sit to either mode boundary before
/// `encode` pushes it back inside.
const MIX_MARGIN: f32 = 1. / 255.;

impl ColorArg {
    /// Read a stored `(rgb, alpha)`. Total — every float means something.
    #[must_use]
    pub fn decode(rgb: [f32; 3], alpha: f32) -> Self {
        let mode = if alpha <= 0. {
            ColorMode::Inherit
        } else if alpha >= 1. {
            ColorMode::Override
        } else {
            ColorMode::Mix(alpha)
        };
        Self { rgb, mode }
    }

    /// The stored form. A mix amount is clamped into the open interval so the
    /// value that comes back from [`Self::decode`] is still a mix — an
    /// out-of-range amount may lose precision, never its mode.
    #[must_use]
    pub fn encode(&self) -> ([f32; 3], f32) {
        let alpha = match self.mode {
            ColorMode::Inherit => 0.,
            ColorMode::Override => 1.,
            ColorMode::Mix(amount) => amount.clamp(MIX_MARGIN, 1. - MIX_MARGIN),
        };
        (self.rgb, alpha)
    }

    /// This arg with another mode selected, keeping the rgb. Entering mix
    /// keeps a previous mix amount and otherwise starts at 0.5 —
    /// the reference's `setColorMode` semantics.
    #[must_use]
    pub fn with_mode(&self, index: usize) -> Self {
        let mode = match index {
            0 => ColorMode::Inherit,
            1 => ColorMode::Override,
            _ => ColorMode::Mix(match self.mode {
                ColorMode::Mix(amount) => amount,
                _ => MIX_DEFAULT,
            }),
        };
        Self { mode, ..*self }
    }
}

/// The selector labels, in [`ColorArg::with_mode`]'s index order — spelled
/// once so the two cannot disagree.
const MODES: [&str; 3] = ["Inherit", "Override", "Mix"];

/// Which selector row a mode shows as.
fn mode_index(mode: ColorMode) -> usize {
    match mode {
        ColorMode::Inherit => 0,
        ColorMode::Override => 1,
        ColorMode::Mix(_) => 2,
    }
}

// -- hsv ---------------------------------------------------------------------

/// A picker-space color: hue in degrees `0..360`, saturation and value `0..=1`.
///
/// Not [`gpui::Hsla`] — that is HSL, and an SV square addressed in HSL warps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    #[must_use]
    pub fn to_rgb(self) -> [f32; 3] {
        let h = self.h.rem_euclid(360.) / 60.;
        let c = self.v * self.s;
        let x = c * (1. - (h % 2. - 1.).abs());
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.),
            1 => (x, c, 0.),
            2 => (0., c, x),
            3 => (0., x, c),
            4 => (x, 0., c),
            _ => (c, 0., x),
        };
        let m = self.v - c;
        [r + m, g + m, b + m]
    }

    /// An achromatic rgb reports hue 0 and zero saturation; that ambiguity is
    /// why an editor keeps its own [`Hsv`] — see the module docs.
    #[must_use]
    pub fn from_rgb(rgb: [f32; 3]) -> Self {
        let [r, g, b] = rgb;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let h = if delta == 0. {
            0.
        } else if max == r {
            60. * ((g - b) / delta).rem_euclid(6.)
        } else if max == g {
            60. * ((b - r) / delta + 2.)
        } else {
            60. * ((r - g) / delta + 4.)
        };
        let s = if max == 0. { 0. } else { delta / max };
        Self { h, s, v: max }
    }
}

fn paint(rgb: [f32; 3], alpha: f32) -> Rgba {
    Rgba {
        r: rgb[0],
        g: rgb[1],
        b: rgb[2],
        a: alpha,
    }
}

// -- the picker --------------------------------------------------------------

/// Picker plate width; the SV square and hue strip both span it.
const PICKER_WIDTH: f32 = 192.;
const SV_HEIGHT: f32 = 112.;
const HUE_HEIGHT: f32 = 12.;

/// A drag in flight on an SV square, routed by id like a slider's.
#[derive(Clone)]
struct SvDrag {
    id: SharedString,
}

impl OwnedDrag for SvDrag {
    fn owner(&self) -> &SharedString {
        &self.id
    }
}

/// A drag in flight on a hue strip.
#[derive(Clone)]
struct HueDrag {
    id: SharedString,
}

impl OwnedDrag for HueDrag {
    fn owner(&self) -> &SharedString {
        &self.id
    }
}

/// The HSV picker: an SV square over a hue strip. Stateless — the caller owns
/// the [`Hsv`] (see the module docs on hue stability) and hears every change
/// the pointer asks for, already clamped.
pub fn luma_hsv_picker(
    id: impl Into<SharedString>,
    hsv: Hsv,
    on_change: impl Fn(Hsv, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let id = id.into();
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(sv_square(&id, hsv, on_change.clone()))
        .child(hue_strip(&id, hsv, on_change))
}

/// The SV plane at the current hue: a white→hue wash left to right under a
/// transparent→black wash top to bottom, with a reticle at `(s, 1−v)`. Two
/// 2-stop gradients compose the standard picker square exactly.
fn sv_square(
    id: &SharedString,
    hsv: Hsv,
    on_change: impl Fn(Hsv, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let hue = paint(
        Hsv {
            h: hsv.h,
            s: 1.,
            v: 1.,
        }
        .to_rgb(),
        1.,
    );
    let drag_id = id.clone();
    let moved = id.clone();
    div()
        .id(ElementId::Name(format!("{id}:sv").into()))
        .relative()
        .w(px(PICKER_WIDTH))
        .h(px(SV_HEIGHT))
        .border_1()
        .border_color(ladder::control_border())
        .bg(linear_gradient(
            90.,
            linear_color_stop(gpui::white(), 0.),
            linear_color_stop(hue, 1.),
        ))
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(
                Hsla {
                    h: 0.,
                    s: 0.,
                    l: 0.,
                    a: 0.,
                },
                0.,
            ),
            linear_color_stop(gpui::black(), 1.),
        )))
        .child(
            // The reticle: an 8px open square, offset to center on the point.
            div()
                .absolute()
                .left(gpui::relative(hsv.s))
                .top(gpui::relative(1. - hsv.v))
                .ml(px(-4.))
                .mt(px(-4.))
                .size(px(8.))
                .border_1()
                .border_color(gpui::white()),
        )
        .on_drag(SvDrag { id: drag_id }, |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| DragGhost)
        })
        .on_drag_move(drag_fraction(moved, move |at, _: &SvDrag, window, cx| {
            on_change(
                Hsv {
                    s: at.x,
                    v: 1. - at.y,
                    ..hsv
                },
                window,
                cx,
            );
        }))
        .agent_node(Role::Slider, format!("{id}:sv"))
}

/// The hue axis as six 2-stop segments — the rainbow a single gpui gradient
/// cannot draw — with a 2px cursor at the current hue.
fn hue_strip(
    id: &SharedString,
    hsv: Hsv,
    on_change: impl Fn(Hsv, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let segment = |from_deg: f32| {
        let stop = |h: f32| {
            linear_color_stop(
                paint(Hsv { h, s: 1., v: 1. }.to_rgb(), 1.),
                if h == from_deg { 0. } else { 1. },
            )
        };
        div()
            .flex_1()
            .h_full()
            .bg(linear_gradient(90., stop(from_deg), stop(from_deg + 60.)))
    };
    let drag_id = id.clone();
    let moved = id.clone();
    div()
        .id(ElementId::Name(format!("{id}:hue").into()))
        .relative()
        .flex()
        .w(px(PICKER_WIDTH))
        .h(px(HUE_HEIGHT))
        .border_1()
        .border_color(ladder::control_border())
        .children([0., 60., 120., 180., 240., 300.].map(segment))
        .child(
            div()
                .absolute()
                .left(gpui::relative(hsv.h.rem_euclid(360.) / 360.))
                .top_0()
                .ml(px(-1.))
                .w(px(2.))
                .h_full()
                .bg(gpui::white()),
        )
        .on_drag(HueDrag { id: drag_id }, |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| DragGhost)
        })
        .on_drag_move(drag_fraction(moved, move |at, _: &HueDrag, window, cx| {
            on_change(
                Hsv {
                    h: 360. * at.x,
                    ..hsv
                },
                window,
                cx,
            );
        }))
        .agent_node(Role::Slider, format!("{id}:hue"))
}

// -- the editor entity -------------------------------------------------------

/// What the editor tells its host: the arg changed, by any path — mode picked,
/// SV dragged, hue dragged, mix slid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorArgEvent {
    Changed(ColorArg),
}

/// The tri-mode cell: mode selector beside a swatch, and — while open — the
/// picker plate. Owns the working [`Hsv`] and its two menus' open flags;
/// emits [`ColorArgEvent::Changed`] with the committed [`ColorArg`] only.
pub struct ColorArgEditor {
    id: SharedString,
    value: ColorArg,
    /// Working picker state; collapses to `value.rgb` on every edit but never
    /// re-derives from it while editing — see the module docs.
    hsv: Hsv,
    mode_menu_open: bool,
    picker_open: bool,
}

impl EventEmitter<ColorArgEvent> for ColorArgEditor {}

impl ColorArgEditor {
    pub fn new(id: impl Into<SharedString>, value: ColorArg, _: &mut Context<Self>) -> Self {
        Self {
            id: id.into(),
            value,
            hsv: Hsv::from_rgb(value.rgb),
            mode_menu_open: false,
            picker_open: false,
        }
    }

    #[must_use]
    pub fn value(&self) -> ColorArg {
        self.value
    }

    /// A host-side write — an external change landed, so the working hue
    /// resyncs (and goes achromatic-ambiguous if the new rgb is grey; that is
    /// the honest reading of a value that moved under the editor).
    pub fn set_value(&mut self, value: ColorArg, cx: &mut Context<Self>) {
        if self.value == value {
            return;
        }
        self.value = value;
        self.hsv = Hsv::from_rgb(value.rgb);
        cx.notify();
    }

    /// Open or close the picker plate — exposed so a fixture can capture the
    /// open state deterministically.
    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.picker_open = open;
        cx.notify();
    }

    fn commit(&mut self, value: ColorArg, cx: &mut Context<Self>) {
        self.value = value;
        cx.emit(ColorArgEvent::Changed(value));
        cx.notify();
    }

    fn swatch(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let inherit = self.value.mode == ColorMode::Inherit;
        let base = div()
            .flex_shrink_0()
            .size(px(CONTROL_HEIGHT))
            .border_1()
            .border_color(ladder::control_border());
        if inherit {
            // Dormant: the color is retained but not in effect, so the swatch
            // shows the control's resting fill, dimmed and inert.
            return base
                .bg(ladder::control())
                .opacity(ladder::DISABLED_OPACITY)
                .agent_node(Role::Button, format!("{} swatch", self.id))
                .agent_disabled(true)
                .into_any_element();
        }
        let this = cx.entity();
        base.bg(paint(self.value.rgb, 1.))
            .id(ElementId::Name(format!("{}:swatch", self.id).into()))
            .on_click(move |_, _, cx| {
                this.update(cx, |editor, cx| {
                    editor.picker_open = !editor.picker_open;
                    cx.notify();
                });
            })
            .agent_node(Role::Button, format!("{} swatch", self.id))
            .into_any_element()
    }

    fn picker_plate(&self, cx: &Context<Self>) -> Div {
        let this = cx.entity();
        let mode = self.value.mode;
        let picker = luma_hsv_picker(self.id.clone(), self.hsv, move |hsv, _, cx| {
            this.update(cx, |editor, cx| {
                editor.hsv = hsv;
                let value = ColorArg {
                    rgb: hsv.to_rgb(),
                    ..editor.value
                };
                editor.commit(value, cx);
            });
        });
        // The plate is a float, so it wears the float tier's card — the
        // instrument styling stays on the controls inside it.
        let plate = crate::float::popover_card()
            .gap(px(6.))
            .p(px(8.))
            .child(picker);
        if let ColorMode::Mix(amount) = mode {
            let this = cx.entity();
            plate.child(crate::luma_slider(
                format!("{}:mix", self.id),
                amount,
                0.,
                1.,
                PICKER_WIDTH,
                move |mix, _, cx| {
                    this.update(cx, |editor, cx| {
                        let value = ColorArg {
                            mode: ColorMode::Mix(mix),
                            ..editor.value
                        };
                        editor.commit(value, cx);
                    });
                },
            ))
        } else {
            plate
        }
    }
}

impl Render for ColorArgEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toggle = cx.entity();
        let pick = cx.entity();
        let id = format!("{}:mode", self.id);
        let value = MODES[mode_index(self.value.mode)];
        let open = self.mode_menu_open;
        let on_toggle = move |_: &mut Window, cx: &mut App| {
            toggle.update(cx, |editor, cx| {
                editor.mode_menu_open = !editor.mode_menu_open;
                cx.notify();
            });
        };
        let on_pick = move |index: usize, _: &mut Window, cx: &mut App| {
            pick.update(cx, |editor, cx| {
                editor.mode_menu_open = false;
                let value = editor.value.with_mode(index);
                editor.commit(value, cx);
            });
        };
        let select = luma_arg_select(id, value, &MODES, open, on_toggle, on_pick);
        let plate = (self.picker_open && self.value.mode != ColorMode::Inherit).then(|| {
            crate::float::anchored_below(
                format!("{}:picker", self.id),
                CONTROL_HEIGHT,
                self.picker_plate(cx).into_any_element(),
            )
        });
        div()
            .relative()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(select)
            .child(self.swatch(cx))
            .children(plate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire's three regimes, decoded: the boundaries belong to the pure
    /// modes and everything strictly between is a mix carrying its alpha.
    #[test]
    fn alpha_decodes_by_regime() {
        let rgb = [0.2, 0.4, 0.6];
        assert_eq!(ColorArg::decode(rgb, 0.).mode, ColorMode::Inherit);
        assert_eq!(ColorArg::decode(rgb, -0.5).mode, ColorMode::Inherit);
        assert_eq!(ColorArg::decode(rgb, 1.).mode, ColorMode::Override);
        assert_eq!(ColorArg::decode(rgb, 1.5).mode, ColorMode::Override);
        assert_eq!(ColorArg::decode(rgb, 0.35).mode, ColorMode::Mix(0.35));
    }

    /// decode∘encode is the identity on every in-range value: the rgb always,
    /// the mode always, the mix amount whenever it is honestly a mix.
    #[test]
    fn the_wire_round_trips() {
        let rgb = [0.9, 0.1, 0.5];
        for arg in [
            ColorArg {
                rgb,
                mode: ColorMode::Inherit,
            },
            ColorArg {
                rgb,
                mode: ColorMode::Override,
            },
            ColorArg {
                rgb,
                mode: ColorMode::Mix(0.5),
            },
            ColorArg {
                rgb,
                mode: ColorMode::Mix(0.996),
            },
        ] {
            let (stored_rgb, alpha) = arg.encode();
            assert_eq!(ColorArg::decode(stored_rgb, alpha), arg);
        }
    }

    /// A degenerate mix amount may lose precision on the wire but never its
    /// mode: encode pushes it inside the open interval instead of letting the
    /// boundary silently flip it to inherit or override.
    #[test]
    fn a_degenerate_mix_stays_a_mix() {
        let rgb = [0., 0., 0.];
        for amount in [0., -1., 1., 2.] {
            let (_, alpha) = ColorArg {
                rgb,
                mode: ColorMode::Mix(amount),
            }
            .encode();
            assert!(
                matches!(ColorArg::decode(rgb, alpha).mode, ColorMode::Mix(_)),
                "mix({amount}) came back as {:?}",
                ColorArg::decode(rgb, alpha).mode
            );
        }
    }

    /// Mode switching keeps the rgb, and entering mix keeps a previous amount
    /// or starts at the reference's 0.5.
    #[test]
    fn with_mode_keeps_the_color_and_the_mix_memory() {
        let rgb = [0.3, 0.6, 0.9];
        let mixed = ColorArg {
            rgb,
            mode: ColorMode::Mix(0.25),
        };
        let inherited = mixed.with_mode(0);
        assert_eq!(inherited.rgb, rgb);
        assert_eq!(inherited.mode, ColorMode::Inherit);
        // Coming back to mix from inherit forgets the amount — the wire
        // dropped it — and lands on the default.
        assert_eq!(inherited.with_mode(2).mode, ColorMode::Mix(MIX_DEFAULT));
        // But a live mix keeps its amount across a re-pick.
        assert_eq!(mixed.with_mode(2).mode, ColorMode::Mix(0.25));
        assert_eq!(mixed.with_mode(1).mode, ColorMode::Override);
    }

    /// HSV→RGB on the corner colors, exactly.
    #[test]
    fn hsv_hits_the_corners() {
        let cases = [
            (
                Hsv {
                    h: 0.,
                    s: 1.,
                    v: 1.,
                },
                [1., 0., 0.],
            ),
            (
                Hsv {
                    h: 120.,
                    s: 1.,
                    v: 1.,
                },
                [0., 1., 0.],
            ),
            (
                Hsv {
                    h: 240.,
                    s: 1.,
                    v: 1.,
                },
                [0., 0., 1.],
            ),
            (
                Hsv {
                    h: 0.,
                    s: 0.,
                    v: 1.,
                },
                [1., 1., 1.],
            ),
            (
                Hsv {
                    h: 0.,
                    s: 0.,
                    v: 0.,
                },
                [0., 0., 0.],
            ),
        ];
        for (hsv, rgb) in cases {
            assert_eq!(hsv.to_rgb(), rgb);
        }
    }

    /// RGB→HSV→RGB round-trips within float noise on arbitrary colors.
    #[test]
    fn rgb_survives_the_picker_space() {
        for rgb in [[0.2, 0.4, 0.6], [0.9, 0.1, 0.5], [0.33, 0.33, 0.33]] {
            let back = Hsv::from_rgb(rgb).to_rgb();
            for (a, b) in rgb.iter().zip(back.iter()) {
                assert!((a - b).abs() < 1e-5, "{rgb:?} came back as {back:?}");
            }
        }
    }
}
