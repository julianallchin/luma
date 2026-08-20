use gpui::prelude::FluentBuilder;
use gpui::*;

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
    ]
}

// GPUI port of BUTTON_CLASS (src/shared/components/ui/button.tsx):
// h-6 px-2 border rounded-none, 9px uppercase bold tracking-wider,
// bg-control on control-border, text foreground/90, hover bg-hover.
// Letter-spacing has no gpui styled equivalent yet — a known, visible gap
// the comparison agent should call out until we add it.
fn luma_button(label: &str, disabled: bool) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .h(px(24.))
        .px(px(8.))
        .border_1()
        .border_color(rgb(0x080808))
        .bg(rgb(0x2e2e2e))
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgba(0xe4e4e4e6))
        .when(disabled, |el| el.opacity(0.5))
        .when(!disabled, |el| {
            el.hover(|s| s.bg(rgb(0x3b3b3b)).text_color(rgb(0xe4e4e4)))
        })
        .child(label.to_uppercase())
}
