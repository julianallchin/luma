use gpui::*;

use luma_ui::{
    luma_button, luma_checkbox, luma_dropdown, luma_input, luma_select, luma_selector, luma_slider,
    luma_toggle, luma_toggle_group,
};

/// One fixture = one component in one deterministic state, identified by an
/// id shared with the web harness (src/harness/fixtures.tsx). Both renderers
/// must render the same state for the same id — that is the whole contract.
///
/// `width`/`height` are the window size in points: content plus the same
/// 24px padding the web harness wraps fixtures in.
pub struct Fixture {
    pub id: &'static str,
    pub width: f32,
    pub height: f32,
    pub build: fn() -> AnyElement,
}

pub fn all() -> Vec<Fixture> {
    vec![
        Fixture {
            id: "button",
            width: 160.,
            height: 72.,
            build: || luma_button("Import Tracks", false).into_any_element(),
        },
        Fixture {
            id: "button-disabled",
            width: 160.,
            height: 72.,
            build: || luma_button("Import Tracks", true).into_any_element(),
        },
        Fixture {
            id: "button-row",
            width: 310.,
            height: 72.,
            build: || {
                div()
                    .flex()
                    .gap(px(8.))
                    .child(luma_button("Save", false))
                    .child(luma_button("Cancel", false))
                    .child(luma_button("Delete Track", false))
                    .into_any_element()
            },
        },
        Fixture {
            id: "select",
            width: 208.,
            height: 72.,
            build: || luma_select("Opus 5", 160.).into_any_element(),
        },
        Fixture {
            id: "selector",
            width: 134.,
            height: 72.,
            build: || luma_selector("Bars", &["Bars", "Beats", "Seconds"]).into_any_element(),
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
            build: || luma_input("Track name", true, 160.).into_any_element(),
        },
        Fixture {
            id: "checkbox-row",
            width: 80.,
            height: 60.,
            build: || {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(luma_checkbox(true))
                    .child(luma_checkbox(false))
                    .into_any_element()
            },
        },
        Fixture {
            // Action menu, closed — only the self-sizing trigger is captured.
            // The trigger is as wide as the widest item ("Import From
            // Rekordbox"), which is the geometry the port has to reproduce.
            id: "dropdown-closed",
            width: 208.,
            height: 72.,
            build: || {
                luma_dropdown(
                    "Actions",
                    &["Import From Rekordbox", "Reanalyze", "Sign Out"],
                )
                .into_any_element()
            },
        },
        Fixture {
            id: "toggle-pressed",
            width: 93.,
            height: 72.,
            build: || luma_toggle("Loop", true).into_any_element(),
        },
        Fixture {
            id: "toggle-unpressed",
            width: 93.,
            height: 72.,
            build: || luma_toggle("Loop", false).into_any_element(),
        },
        Fixture {
            id: "toggle-group",
            width: 206.,
            height: 72.,
            build: || luma_toggle_group("Beats", &["Bars", "Beats", "Seconds"]).into_any_element(),
        },
        Fixture {
            // 40 of 0..100 → fill bar covers 40% of the track.
            id: "slider",
            width: 304.,
            height: 76.,
            build: || luma_slider(40., 0., 100., 256.).into_any_element(),
        },
    ]
}
