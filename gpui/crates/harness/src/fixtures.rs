use gpui::*;

use luma_ui::arg::color::{ColorArg, ColorArgEditor, ColorMode};
use luma_ui::arg::expression::GroupExpressionEditor;
use luma_ui::arg::gradient::{Gradient, GradientStop};
use luma_ui::arg::number::DraftedNumber;
use luma_ui::Enabled;
use luma_ui::{
    luma_button, luma_checkbox, luma_dropdown, luma_input, luma_select, luma_selector, luma_slider,
    luma_toggle, luma_toggle_group,
};

/// How a fixture produces its content.
///
/// `Static` covers the crate's usual stateless controls. `View` exists for the
/// entity-backed widgets (a drafted number field, the expression editor): an
/// entity must be created once and *rendered* every frame, where a static
/// fixture is rebuilt from nothing every frame — giving those a `Static` slot
/// would re-create the entity per frame and reset the very state the fixture
/// pins.
pub enum Build {
    Static(fn() -> AnyElement),
    View(fn(&mut Window, &mut App) -> AnyView),
}

/// One fixture = one component in one deterministic state, identified by an
/// id shared with the web harness (src/harness/fixtures.tsx). Both renderers
/// must render the same state for the same id — that is the whole contract.
/// (The `arg-*` fixtures are gpui-first: the strip widgets have no web
/// harness twin yet, so they pin *this* renderer's resting states.)
///
/// `width`/`height` are the window size in points: content plus the same
/// 24px padding the web harness wraps fixtures in.
pub struct Fixture {
    pub id: &'static str,
    pub width: f32,
    pub height: f32,
    pub build: Build,
}

pub fn all() -> Vec<Fixture> {
    vec![
        Fixture {
            id: "button",
            width: 160.,
            height: 72.,
            build: Build::Static(|| luma_button("Import Tracks", Enabled::Yes).into_any_element()),
        },
        Fixture {
            id: "button-disabled",
            width: 160.,
            height: 72.,
            build: Build::Static(|| luma_button("Import Tracks", Enabled::No).into_any_element()),
        },
        Fixture {
            id: "button-row",
            width: 310.,
            height: 72.,
            build: Build::Static(|| {
                div()
                    .flex()
                    .gap(px(8.))
                    .child(luma_button("Save", Enabled::Yes))
                    .child(luma_button("Cancel", Enabled::Yes))
                    .child(luma_button("Delete Track", Enabled::Yes))
                    .into_any_element()
            }),
        },
        Fixture {
            id: "select",
            width: 208.,
            height: 72.,
            build: Build::Static(|| luma_select("Opus 5", 160.).into_any_element()),
        },
        Fixture {
            id: "selector",
            width: 134.,
            height: 72.,
            build: Build::Static(|| {
                luma_selector("Bars", &["Bars", "Beats", "Seconds"]).into_any_element()
            }),
        },
        Fixture {
            // Resting, unfocused input showing its placeholder. The port is the
            // component's *appearance*; gpui-component's stateful `TextInput`
            // needs an `Entity<InputState>`, which the `fn() -> AnyElement`
            // fixture contract can't build — and the captured frame has no
            // caret or selection to compare anyway.
            id: "input",
            width: 208.,
            height: 72.,
            build: Build::Static(|| luma_input("Track name", true, 160.).into_any_element()),
        },
        Fixture {
            id: "checkbox-row",
            width: 80.,
            height: 60.,
            build: Build::Static(|| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(luma_checkbox(true))
                    .child(luma_checkbox(false))
                    .into_any_element()
            }),
        },
        Fixture {
            // Action menu, closed — only the self-sizing trigger is captured.
            // The trigger is as wide as the widest item ("Import From
            // Rekordbox"), which is the geometry the port has to reproduce.
            id: "dropdown-closed",
            width: 208.,
            height: 72.,
            build: Build::Static(|| {
                luma_dropdown(
                    "Actions",
                    &["Import From Rekordbox", "Reanalyze", "Sign Out"],
                )
                .into_any_element()
            }),
        },
        Fixture {
            id: "toggle-pressed",
            width: 93.,
            height: 72.,
            build: Build::Static(|| luma_toggle("Loop", true).into_any_element()),
        },
        Fixture {
            id: "toggle-unpressed",
            width: 93.,
            height: 72.,
            build: Build::Static(|| luma_toggle("Loop", false).into_any_element()),
        },
        Fixture {
            id: "toggle-group",
            width: 206.,
            height: 72.,
            build: Build::Static(|| {
                luma_toggle_group("Beats", &["Bars", "Beats", "Seconds"]).into_any_element()
            }),
        },
        Fixture {
            // 40 of 0..100 → fill bar covers 40% of the track.
            id: "slider",
            width: 304.,
            height: 76.,
            // The writer is empty because this captures the *resting* frame and
            // nothing here drags: a fixture has no state for a value to move
            // in, and the comparison is against a WebKit screenshot of the same
            // three layers.
            build: Build::Static(|| {
                luma_slider("slider", 40., 0., 100., 256., |_, _, _| {}).into_any_element()
            }),
        },
        // -- the pattern-arg widget kit (luma_ui::arg) -----------------------
        Fixture {
            // A populated cell and the ghost cell side by side: the pair must
            // occupy identical boxes, which is the strip's no-layout-shift
            // contract made visible in one frame.
            id: "arg-cell-ghost",
            width: 320.,
            height: 88.,
            build: Build::Static(|| {
                div()
                    .flex()
                    .items_end()
                    .gap(px(12.))
                    .child(luma_ui::arg::arg_cell(
                        "Blend",
                        luma_selector("Multiply", &["Multiply", "Replace", "Screen"]),
                    ))
                    .child(luma_ui::arg::arg_cell_ghost("Blend", 96.))
                    .into_any_element()
            }),
        },
        Fixture {
            // One menu with every RowState live at once: rest, the selected
            // value (fill + ring + check), and the keyboard cursor on a
            // *different* row (plate, no ring). A single-state shot cannot
            // show that the two lifts are different paints; this one pins
            // exactly that.
            id: "menu-row-states",
            width: 240.,
            height: 160.,
            build: Build::Static(|| {
                use luma_ui::float::RowState;
                luma_ui::float::popover_card()
                    .min_w(px(200.))
                    .child(luma_ui::luma_select_item(
                        "Bars",
                        RowState::of(false, false),
                    ))
                    .child(luma_ui::luma_select_item(
                        "Beats",
                        RowState::of(true, false),
                    ))
                    .child(luma_ui::luma_select_item(
                        "Seconds",
                        RowState::of(false, true),
                    ))
                    .into_any_element()
            }),
        },
        Fixture {
            // The strip's value picker, open, with the middle row current.
            id: "arg-select-open",
            width: 240.,
            height: 320.,
            build: Build::Static(|| {
                luma_ui::arg::select::luma_arg_select(
                    "arg-select",
                    "Beats",
                    &["Bars", "Beats", "Seconds"],
                    true,
                    |_, _| {},
                    |_, _, _| {},
                )
                .into_any_element()
            }),
        },
        Fixture {
            // The same picker with its trigger on the fixture's floor. There
            // is no orientation parameter to pin — what this shot holds is
            // the *mechanism*: with no window left below, the popup
            // float's snap (`float::anchored_below`) slides the open menu up until it fits, which stands it
            // over its own trigger the way a native popup menu stands over
            // its control. The silkscreen marker sits beside the trigger at
            // the same baseline, outside the menu's width, so the shot
            // carries the trigger row's position even though the menu
            // rightly occludes the trigger itself; the capture for this id
            // crops inside the snap margin (see the harness main), so the
            // menu's bottom border and its 8px window clearance are in the
            // image too.
            id: "arg-select-open-up",
            width: 280.,
            height: 176.,
            build: Build::Static(|| {
                div()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .w(px(280.))
                    .h(px(176.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            // The marker is the capture's measuring stick, so
                            // it must stay clear of the menu's footprint: the
                            // float sizes itself (min 160) from the trigger's
                            // left edge, and a marker sitting beside the
                            // trigger would be read *through* the card.
                            .justify_between()
                            .child(luma_ui::arg::select::luma_arg_select(
                                "arg-select-up",
                                "Beats",
                                &["Bars", "Beats", "Seconds"],
                                true,
                                |_, _| {},
                                |_, _, _| {},
                            ))
                            .child(luma_ui::silkscreen("TRIGGER ROW".to_string())),
                    )
                    .into_any_element()
            }),
        },
        Fixture {
            // Three stops, middle selected: flat lead-in, two exact gradient
            // segments, flat tail, primary-ring on the held handle.
            id: "arg-gradient",
            width: 304.,
            height: 72.,
            build: Build::Static(|| {
                let gradient = Gradient::new([
                    GradientStop {
                        t: 0.1,
                        color: rgb(0xff0080),
                    },
                    GradientStop {
                        t: 0.5,
                        color: rgb(0x00ffc8),
                    },
                    GradientStop {
                        t: 0.9,
                        color: rgb(0xffbe28),
                    },
                ]);
                luma_ui::arg::gradient::luma_gradient_bar(
                    "arg-gradient",
                    &gradient,
                    Some(1),
                    256.,
                    |_, _, _| {},
                )
                .into_any_element()
            }),
        },
        Fixture {
            // The web palette editor's own fallback colors, second selected,
            // with the add and remove slabs.
            id: "arg-palette",
            width: 260.,
            height: 72.,
            build: Build::Static(|| {
                luma_ui::arg::palette::luma_palette_row(
                    "arg-palette",
                    &[rgb(0xff0080), rgb(0x00ffc8), rgb(0xffbe28), rgb(0x3b82f6)],
                    Some(1),
                    |_, _, _| {},
                )
                .into_any_element()
            }),
        },
        Fixture {
            // A committed value at rest — the draft equals the formatted
            // value, which is the only state a fixture can honestly pin.
            id: "arg-number",
            width: 160.,
            height: 72.,
            build: Build::View(|window, cx| {
                cx.new(|cx| DraftedNumber::new("arg-number", 0.35, 0., 1., 96., window, cx))
                    .into()
            }),
        },
        Fixture {
            // An expression over a known vocabulary, unfocused: amber groups,
            // rose operators, grey parens, plain unknown words — the shaper's
            // own coloring, no overlay.
            id: "arg-expression",
            width: 360.,
            height: 72.,
            build: Build::View(|window, cx| {
                cx.new(|cx| {
                    GroupExpressionEditor::new(
                        ["front_wash", "back_movers", "dj_booth"].map(SharedString::from),
                        "front_wash | (all & ~back_movers)",
                        300.,
                        window,
                        cx,
                    )
                })
                .into()
            }),
        },
        Fixture {
            // Mix at 0.35 over an orange: mode selector, live swatch, closed.
            id: "arg-color",
            width: 220.,
            height: 72.,
            build: Build::View(|_, cx| {
                cx.new(|cx| {
                    ColorArgEditor::new(
                        "arg-color",
                        ColorArg {
                            rgb: [1., 0.55, 0.16],
                            mode: ColorMode::Mix(0.35),
                        },
                        cx,
                    )
                })
                .into()
            }),
        },
        Fixture {
            // The same editor with its picker plate open: SV square at the
            // stored color, hue strip, and the mix slider under them.
            id: "arg-color-open",
            width: 400.,
            // Tall: the fixture frame centers its child, and the picker plate
            // hangs *below* the anchor row, so the window leaves room for it
            // on both sides of center.
            height: 440.,
            build: Build::View(|_, cx| {
                let editor = cx.new(|cx| {
                    ColorArgEditor::new(
                        "arg-color",
                        ColorArg {
                            rgb: [1., 0.55, 0.16],
                            mode: ColorMode::Mix(0.35),
                        },
                        cx,
                    )
                });
                editor.update(cx, |editor, cx| editor.set_open(true, cx));
                editor.into()
            }),
        },
        Fixture {
            // Inherit: the swatch goes dormant (dimmed control fill, inert) —
            // the color is retained underneath but not in effect.
            id: "arg-color-inherit",
            width: 220.,
            height: 72.,
            build: Build::View(|_, cx| {
                cx.new(|cx| {
                    ColorArgEditor::new(
                        "arg-color",
                        ColorArg {
                            rgb: [1., 0.55, 0.16],
                            mode: ColorMode::Inherit,
                        },
                        cx,
                    )
                })
                .into()
            }),
        },
        Fixture {
            // Override: same rgb, live swatch, no mix slider when opened.
            id: "arg-color-override",
            width: 220.,
            height: 72.,
            build: Build::View(|_, cx| {
                cx.new(|cx| {
                    ColorArgEditor::new(
                        "arg-color",
                        ColorArg {
                            rgb: [1., 0.55, 0.16],
                            mode: ColorMode::Override,
                        },
                        cx,
                    )
                })
                .into()
            }),
        },
        Fixture {
            // The autocomplete, live: the field is focused programmatically
            // and the caret sits after the half-typed "b", so the menu shows
            // the vocabulary filtered to that prefix.
            id: "arg-expression-menu",
            width: 360.,
            height: 200.,
            build: Build::View(|window, cx| {
                let editor = cx.new(|cx| {
                    GroupExpressionEditor::new(
                        ["front_wash", "back_movers", "back_wash", "dj_booth"]
                            .map(SharedString::from),
                        "front_wash | b",
                        300.,
                        window,
                        cx,
                    )
                });
                window.focus(&editor.focus_handle(cx), cx);
                editor.into()
            }),
        },
    ]
}
