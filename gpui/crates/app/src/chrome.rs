//! The window's own edges: comet's titlebar band, drawn by us.
//!
//! The window is `decorations: false` on every platform, so nothing draws
//! chrome but this module. The band is **glass tier** (spec §9): traffic
//! lights, the sidebar toggle, back/forward, the shell tab strip and the
//! settings gear all live in it, painted from `luma_ui::glass` — never from
//! the ladder, and never as `BUTTON_CLASS` slabs. Ladder language ends at the
//! content cards below this band.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName};
use luma_ui::glass;
use luma_ui::node::{Instrument, Role};

use crate::tabs::Target;
use crate::Luma;

/// Height of the titlebar band — comet's `TITLEBAR_HEIGHT`.
pub const HEIGHT: f32 = 38.;
/// A tab chip: comet's 24px rounded-6 chip.
const CHIP_HEIGHT: f32 = 24.;
const CHIP_RADIUS: f32 = 6.;
/// A chip never grows past comet's fixed slot.
const CHIP_MAX_WIDTH: f32 = 148.;
/// One icon-button box in the band.
const CONTROL: f32 = 24.;

/// Hide the close / minimise / zoom buttons AppKit puts in the content view.
///
/// The window asks for a titlebar solely to get `NSResizableWindowMask` (see
/// `main`), and that mask brings the system buttons with it. They would land on
/// top of the titlebar we draw, so we take them out; our own traffic lights
/// are the only control strip. Everything they do is still reachable — `zoom:` and
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

/// The band. One drag region carrying, left to right: traffic lights, the
/// sidebar toggle, back/forward, a flexible drag area, the tab strip with its
/// `+` and collapse chevron, and the settings gear.
pub(crate) fn titlebar(app: &Luma, cx: &mut Context<Luma>) -> Div {
    let entity = cx.entity();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(10.))
        .h(px(HEIGHT))
        .px(px(12.))
        // The band sits directly on the glass plane — no fill of its own, no
        // border. Comet separates it from the content by the cards' inset.
        // The whole bar is the drag region; every control stops propagation
        // so a press doesn't also start a move.
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
        .child(traffic_lights())
        .child(
            icon_button("sidebar-toggle", IconName::PanelLeft)
                .on_click({
                    let toggled = entity.clone();
                    move |_, _, cx| {
                        toggled.update(cx, |this, cx| {
                            this.sidebar_hidden = !this.sidebar_hidden;
                            cx.notify();
                        });
                    }
                })
                .agent_node(Role::Button, "sidebar-toggle"),
        )
        // Back/forward: comet's chrome carries them always, dimmed when there
        // is nowhere to go. This shell has no navigation history — nothing is
        // destroyed, so there is nothing to go back to — and the pair is
        // permanently at rest until a history exists to drive it.
        .child(dim_icon(IconName::ArrowLeft))
        .child(dim_icon(IconName::ArrowRight))
        .child(div().flex_1())
        .child(tab_strip(app, &entity))
        .child(
            icon_button("settings", IconName::Settings)
                .on_click({
                    let opened = entity.clone();
                    move |_, _, cx| {
                        opened.update(cx, |this, cx| this.open_settings(cx));
                    }
                })
                .agent_node(Role::Button, "Settings"),
        )
}

/// macOS traffic lights, ours: the native ones are hidden (see
/// [`hide_native_window_buttons`]) so these three dots are the window's
/// close / minimise / zoom. Hue is meaning here — the one place the shell
/// keeps color.
fn traffic_lights() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(light("close", rgb(0xff5f57), |window| {
            window.remove_window()
        }))
        .child(light("minimize", rgb(0xfebc2e), |window| {
            window.minimize_window()
        }))
        .child(light("maximize", rgb(0x28c840), |window| {
            window.zoom_window()
        }))
}

fn light(id: &'static str, color: Rgba, action: fn(&mut Window)) -> impl IntoElement {
    div()
        .id(id)
        .size(px(12.))
        .rounded_full()
        .bg(color)
        .hover(|dot| dot.opacity(0.8))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, _| action(window))
        .agent_node(Role::Button, id)
}

/// One glass icon button: quiet ink in a rounded box that washes on hover.
fn icon_button(id: &'static str, icon: IconName) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(CONTROL))
        .rounded(px(CHIP_RADIUS))
        .flex()
        .items_center()
        .justify_center()
        .text_color(glass::ink(0.55))
        .hover(|button| button.bg(glass::wash(0.06)).text_color(glass::ink(0.90)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(Icon::new(icon).size(px(14.)))
}

/// A control that exists but cannot act yet, at rest: comet dims it rather
/// than dropping it, so the chrome's anatomy holds still.
fn dim_icon(icon: IconName) -> Div {
    div()
        .size(px(CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .text_color(glass::ink(0.22))
        .child(Icon::new(icon).size(px(14.)))
}

/// The shell tab strip: one rounded chip per open tab, the `+`, and the
/// collapse chevron. In the titlebar band because the band owns that strip in
/// comet — controls mounted in the pane itself would sit under the drag
/// region.
fn tab_strip(app: &Luma, entity: &Entity<Luma>) -> Div {
    let active = app.workspace.active().cloned();
    let mut strip = div().flex().items_center().gap(px(4.));
    for tab in app.workspace.iter() {
        strip = strip.child(chip(
            &tab.target,
            tab.body.title(),
            active.as_ref() == Some(&tab.target),
            entity,
        ));
    }
    strip = strip.child(
        icon_button("new-tab", IconName::Plus)
            .on_click({
                let opened = entity.clone();
                move |_, _, cx| {
                    opened.update(cx, |this, cx| this.show_patterns(cx));
                }
            })
            .agent_node(Role::Button, "new-tab"),
    );
    if !app.workspace.is_empty() {
        strip = strip.child(
            icon_button("workspace-collapse", IconName::ChevronRight)
                .on_click({
                    let collapsed = entity.clone();
                    move |_, _, cx| {
                        collapsed.update(cx, |this, cx| {
                            this.workspace_hidden = !this.workspace_hidden;
                            cx.notify();
                        });
                    }
                })
                .agent_node(Role::Button, "workspace-collapse"),
        );
    }
    strip
}

/// One tab chip: a leading icon slot that swaps in place for a ✕ on hover, the
/// title, active `wash(0.10)`, hover `wash(0.06)`. Click selects; the ✕
/// closes through the same teardown every close takes.
fn chip(
    target: &Target,
    title: SharedString,
    is_active: bool,
    entity: &Entity<Luma>,
) -> impl IntoElement {
    let group = SharedString::from(format!("chip:{}", target.element_key()));
    let select = entity.clone();
    let selected = target.clone();
    let close = entity.clone();
    let closed = target.clone();
    div()
        .id(group.clone())
        .group(group.clone())
        .h(px(CHIP_HEIGHT))
        .max_w(px(CHIP_MAX_WIDTH))
        .px(px(8.))
        .rounded(px(CHIP_RADIUS))
        .flex()
        .items_center()
        .gap(px(6.))
        .when(is_active, |chip| {
            chip.bg(glass::wash(0.10)).text_color(glass::ink(0.92))
        })
        .when(!is_active, |chip| {
            chip.text_color(glass::ink(0.55))
                .hover(|chip| chip.bg(glass::wash(0.06)))
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            select.update(cx, |this, cx| {
                this.workspace.select(&selected);
                cx.notify();
            });
        })
        // The 14px leading slot: the tab's icon at rest, the ✕ under the
        // pointer — swapped in place so the chip never changes width.
        .child(
            div()
                .size(px(14.))
                .flex_none()
                .relative()
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .group_hover(group.clone(), |icon| icon.opacity(0.))
                        .child(Icon::new(kind_icon(target)).size(px(12.))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "close:{}",
                            target.element_key()
                        )))
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover(group.clone(), |x| x.opacity(1.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            close.update(cx, |this, cx| {
                                if let Some(body) = this.workspace.close(&closed) {
                                    this.teardown(body, cx);
                                }
                                cx.notify();
                            });
                        })
                        .child(Icon::new(IconName::Close).size(px(11.))),
                ),
        )
        .child(
            div()
                .text_size(px(11.5))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(title.clone()),
        )
        .agent_node(Role::Button, title)
}

/// The icon a tab kind wears in its chip.
fn kind_icon(target: &Target) -> IconName {
    match target {
        Target::TrackEditor { .. } => IconName::Play,
        Target::Graph { .. } => IconName::Network,
        Target::Visualizer { .. } => IconName::Frame,
        Target::Universe { .. } => IconName::Cpu,
    }
}
