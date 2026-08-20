//! The window's own edges: a custom titlebar and custom traffic lights.
//!
//! The window is `decorations: false` on every platform (the same choice
//! `tauri.conf.json` makes), so nothing draws these but us. The titlebar is
//! the deepest plane in the ladder and carries **no** bottom border — it reads
//! as the top edge of one continuous surface, separated from the body by value
//! alone. Mirrors `src/shared/components/window-controls.tsx` and the
//! `.titlebar` rule in `src/App.css`.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName};
use luma_ui::ladder;

/// Height of the titlebar plane. The web side is `padding: 0.25rem` around a
/// 20px control row; 28px is that box.
pub const HEIGHT: f32 = 28.;

/// The titlebar: a drag region carrying `title` as silkscreen on the left and
/// the window controls on the right.
pub fn titlebar(title: &str) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .h(px(HEIGHT))
        .px(px(8.))
        .bg(ladder::titlebar_background())
        // The whole bar is the drag region; the controls below stop
        // propagation so a click on one doesn't also start a move.
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
        .child(
            div()
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(ladder::muted_foreground())
                .child(title.to_uppercase()),
        )
        .child(window_controls())
}

/// Minimize / maximize / close. Same 20px hit boxes, same muted-to-foreground
/// hover, and the same red close as the web control strip.
fn window_controls() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(control(Glyph::Minimize, |window| window.minimize_window()))
        .child(control(Glyph::Maximize, |window| window.zoom_window()))
        .child(control(Glyph::Close, |window| window.remove_window()))
}

/// Which of the three marks a control draws.
#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Minimize,
    Maximize,
    Close,
}

impl Glyph {
    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }

    /// The same three shapes the web control strip draws inline — a rule, a
    /// square outline, an X — but from gpui-component's asset set rather than
    /// hand-built out of `div` borders. At 12px the 24px viewBox lands ~8px of
    /// ink, which is the web SVGs' size.
    fn icon(self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Maximize => IconName::WindowMaximize,
            Self::Close => IconName::WindowClose,
        }
    }
}

fn control(glyph: Glyph, action: fn(&mut Window)) -> Stateful<Div> {
    let destructive = glyph == Glyph::Close;
    div()
        .id(glyph.id())
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(ladder::muted_foreground())
        .when(destructive, |el| {
            el.hover(|s| s.bg(rgb(0xdc2626)).text_color(rgb(0xffffff)))
        })
        .when(!destructive, |el| {
            el.hover(|s| s.bg(rgba(0xffffff0d)).text_color(ladder::foreground()))
        })
        // Stop the mouse-down from reaching the titlebar's drag handler; the
        // click itself still lands, which is what runs the action.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, _| action(window))
        .child(Icon::new(glyph.icon()).size(px(12.)))
}
