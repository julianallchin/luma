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
use luma_ui::node::{Instrument, Role};
use luma_ui::Enabled;

/// Height of the titlebar plane. The web side is `padding: 0.25rem` around a
/// 20px control row; 28px is that box.
pub const HEIGHT: f32 = 28.;

/// Hide the close / minimise / zoom buttons AppKit puts in the content view.
///
/// The window asks for a titlebar solely to get `NSResizableWindowMask` (see
/// `main`), and that mask brings the system buttons with it. They would land on
/// top of the titlebar we draw, so we take them out; [`window_controls`] is the
/// only control strip. Everything they do is still reachable — `zoom:` and
/// `miniaturize:` work on a hidden button — and no-op on other platforms, which
/// never draw them in the first place.
#[cfg(target_os = "macos")]
pub fn hide_native_window_buttons(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Fully qualified: gpui's inherent `Window::window_handle` is a different
    // thing entirely (an `AnyWindowHandle`) and shadows the trait method.
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: gpui hands out the live `NSView` backing this window, and this
    // runs on the main thread inside the window's own init callback, so the
    // view (and the window owning it) outlive the borrow.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(native_window) = view.window() else {
        return;
    };
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = native_window.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_native_window_buttons(_window: &Window) {}

/// The titlebar: a drag region carrying `title` as silkscreen on the left and
/// the right-hand action cluster — settings, then the window controls — on the
/// right. Mirrors `src/shared/components/header-actions.tsx`, minus the
/// account dropdown (this host has no session yet).
pub fn titlebar(title: &str, on_settings: impl Fn(&mut Window, &mut App) + 'static) -> Div {
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
        .child(luma_ui::silkscreen(title.to_uppercase()))
        .child(header_actions(on_settings))
}

/// The `no-drag` cluster on the right: the settings button and the window
/// controls, `gap-2` apart.
fn header_actions(on_settings: impl Fn(&mut Window, &mut App) + 'static) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            luma_ui::luma_button("Settings", Enabled::Yes)
                .id("settings")
                // Same reason as a window control: the press must not also
                // start a window move.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| on_settings(window, cx))
                .agent_node(Role::Button, "Settings"),
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

fn control(glyph: Glyph, action: fn(&mut Window)) -> impl IntoElement {
    let destructive = glyph == Glyph::Close;
    div()
        .id(glyph.id())
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(ladder::muted_foreground())
        .when(destructive, |el| {
            el.hover(|s| {
                s.bg(ladder::destructive_hover())
                    .text_color(ladder::destructive_foreground())
            })
        })
        .when(!destructive, |el| {
            el.hover(|s| s.bg(ladder::white_5()).text_color(ladder::foreground()))
        })
        // Stop the mouse-down from reaching the titlebar's drag handler; the
        // click itself still lands, which is what runs the action.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, _| action(window))
        .child(Icon::new(glyph.icon()).size(px(12.)))
        .agent_node(Role::Button, glyph.id())
}
