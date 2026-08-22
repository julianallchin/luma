//! The window's own edges: the head band each region wears, drawn by us.
//!
//! The window is `decorations: false` on every platform, so nothing draws
//! chrome but this module. Everything here is **glass tier** (spec §9):
//! traffic lights, the sidebar toggle, back/forward, the shell tab strip and
//! the settings gear are painted from `luma_ui::glass` — never from the
//! ladder, and never as `BUTTON_CLASS` slabs. Ladder language starts inside
//! the regions' bodies.
//!
//! # There is no titlebar
//!
//! The window splits vertically first: each region is a full-height column
//! carrying its own [`band`] across its own width. A full-width bar would cut
//! every seam off at `y = HEIGHT`, and a seam that stops is not a seam — it is
//! a border on a box. The bands align because they share one height, not
//! because they are one element.
//!
//! # The traffic lights are the topmost layer
//!
//! [`window_controls`] is positioned by the shell at the window's top-left
//! corner, above the regions *and* above any overlay. An overlay that covered
//! them would be a modal that can neither be moved nor closed, which is how a
//! first-run picker becomes a trapped window.
//!
//! Room for them is reserved by [`band`], not by each surface: **anything that
//! starts at `y = 0` wears a band**, and the leftmost one leaves
//! [`LIGHTS_WIDTH`] free. A region does, and so does an overlay plane — one
//! mechanism rather than an opt-in every future surface has to remember. The
//! strip itself is inert apart from the three dots, so forgetting is a visible
//! overlap and never a corner that silently swallows presses.

use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{dialog, glass, motion};

use crate::tab_chrome::{
    menu_choices, NewTabPrerequisites, PointerRegion, TabDescriptor, CHIP_GAP,
};
use crate::tabs::Target;
use crate::Luma;

/// Height of a region's head band — comet's `TITLEBAR_HEIGHT`.
pub const HEIGHT: f32 = 38.;
/// A tab chip: comet's 24px rounded-6 chip.
const CHIP_HEIGHT: f32 = 24.;
const CHIP_RADIUS: f32 = 6.;
/// One icon-button box in the band.
const CONTROL: f32 = 24.;
/// A band's inset from its region's edge, and the gap between its controls.
const BAND_PAD: f32 = 12.;
const BAND_GAP: f32 = 10.;
/// One traffic light, and the gap between them.
const LIGHT: f32 = 12.;
const LIGHT_GAP: f32 = 8.;

/// Room [`window_controls`] claims at the window's top-left corner, inset
/// included. Derived rather than measured so the two cannot drift: the shell
/// pads the leftmost band by exactly what the lights occupy.
pub(crate) const LIGHTS_WIDTH: f32 = BAND_PAD + 3. * LIGHT + 2. * LIGHT_GAP;

/// Width a strip may claim inside a band after its fixed siblings. Keeping
/// this arithmetic beside the band constants prevents a narrow window from
/// laying out fixed tab slots past the viewport.
pub(crate) fn tab_strip_room(
    panel_width: f32,
    leftmost: bool,
    leading_width: f32,
    trailing_width: f32,
    sibling_gaps: usize,
) -> f32 {
    (panel_width
        - 2.0 * BAND_PAD
        - if leftmost { LIGHTS_WIDTH } else { 0.0 }
        - leading_width
        - trailing_width
        - sibling_gaps as f32 * BAND_GAP)
        .max(0.0)
}

/// Window-space origin of a strip after the band's fixed leading children.
pub(crate) fn tab_strip_origin(
    panel_x: f32,
    leftmost: bool,
    leading_width: f32,
    leading_children: usize,
) -> f32 {
    panel_x
        + BAND_PAD
        + if leftmost { LIGHTS_WIDTH } else { 0.0 }
        + leading_width
        + leading_children as f32 * BAND_GAP
}

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

/// One region's head band: the empty drag strip its controls sit in.
///
/// No fill and no border — the band *is* the region's own ground, and a rule
/// under it would be the one horizontal border this shell does not have. Pass
/// `leftmost` for the region at `x = 0`, which yields the corner to
/// [`window_controls`].
pub(crate) fn band(leftmost: bool) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(BAND_GAP))
        .h(px(HEIGHT))
        .px(px(BAND_PAD))
        .when(leftmost, |band| band.pl(px(LIGHTS_WIDTH)))
        // The whole band is the drag region; every control stops propagation
        // so a press doesn't also start a move.
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
}

/// The sidebar's show/hide toggle. Rendered by the leftmost *visible* region,
/// so hiding the sidebar hands the control to the thread rather than taking
/// it away with the region it opens.
pub(crate) fn sidebar_toggle(app: &Entity<Luma>) -> impl IntoElement {
    icon_button("sidebar-toggle", IconName::PanelLeft)
        .on_click({
            let toggled = app.clone();
            move |_, _, cx| {
                toggled.update(cx, |this, cx| {
                    this.sidebar_hidden = !this.sidebar_hidden;
                    cx.notify();
                });
            }
        })
        .agent_node(Role::Button, "sidebar-toggle")
}

/// Back/forward: comet's chrome carries them always, dimmed when there is
/// nowhere to go. This shell has no navigation history — nothing is destroyed,
/// so there is nothing to go back to — and the pair is permanently at rest
/// until a history exists to drive it.
pub(crate) fn history_pair() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(BAND_GAP))
        .child(dim_icon(IconName::ArrowLeft))
        .child(dim_icon(IconName::ArrowRight))
}

/// The settings gear, rendered at the trailing edge of the rightmost region's
/// band — the window's far corner, wherever the regions put it.
pub(crate) fn settings_button(app: &Entity<Luma>) -> impl IntoElement {
    icon_button("settings", IconName::Settings)
        .on_click({
            let opened = app.clone();
            move |_, _, cx| {
                opened.update(cx, |this, cx| this.open_settings(cx));
            }
        })
        .agent_node(Role::Button, "Settings")
}

/// macOS traffic lights, ours: the native ones are hidden (see
/// [`hide_native_window_buttons`]) so these three dots are the window's
/// close / minimise / zoom. Hue is meaning here — the one place the shell
/// keeps color.
///
/// Absolutely placed at the window's corner and painted last, for the reason
/// in the module docs.
///
/// **Nothing here is interactive but the three dots.** The strip carries no
/// handler of its own, so a press anywhere else in the corner reaches whatever
/// this is painted over — which is what keeps a surface that forgot to reserve
/// the corner a *visible* overlap rather than a silent dead zone that eats its
/// buttons. Dragging the corner still moves the window, because the [`band`]
/// underneath is the drag region and the press now gets there.
pub(crate) fn window_controls() -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .h(px(HEIGHT))
        .pl(px(BAND_PAD))
        .flex()
        .items_center()
        .gap(px(LIGHT_GAP))
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
        .size(px(LIGHT))
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
/// collapse chevron. It rides the workspace's own band, which is the width the
/// strip is *about* — comet right-aligns it into a full-width bar to fake the
/// same thing.
pub(crate) fn tab_strip(
    app: &mut Luma,
    entity: &Entity<Luma>,
    available_width: f32,
    window_x: f32,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> impl IntoElement {
    app.tab_chrome
        .set_strip_origin(window_x, (HEIGHT - CHIP_HEIGHT) / 2.0);
    let active = app.workspace.active().cloned();
    let descriptors = app
        .workspace
        .iter()
        .map(|tab| TabDescriptor {
            target: tab.target.clone(),
            title: tab.body.title().to_string(),
        })
        .collect::<Vec<_>>();
    let show_plus = available_width >= CONTROL;
    let show_collapse = !descriptors.is_empty() && available_width >= 2.0 * CONTROL + CHIP_GAP;
    let controls_width = if show_plus { CONTROL } else { 0.0 }
        + if show_collapse {
            CHIP_GAP + CONTROL
        } else {
            0.0
        };
    let now = Instant::now();
    let frame = app.tab_chrome.frame(
        &descriptors,
        available_width,
        controls_width,
        motion::reduced_motion(cx),
        now,
    );
    if frame.animating {
        window.request_animation_frame();
    }

    let strip = div()
        .w(px(available_width.max(0.0)))
        .h(px(CHIP_HEIGHT))
        .flex_none()
        .relative();
    let mut rail = div()
        .size_full()
        .flex()
        .items_center()
        .relative()
        // The menu is a sibling outside this mask. Chips and fixed controls
        // are clipped to the exact room the band assigned them, so a 0px slot
        // cannot leak its icon/padding over history or settings.
        .overflow_hidden();
    for (index, tab) in frame.live.iter().enumerate() {
        let region = PointerRegion {
            x: index as f32 * (tab.width + frame.gap) + 8.0,
            y: (CHIP_HEIGHT - 14.0) / 2.0,
            width: 14.0,
            height: 14.0,
        };
        rail = rail.child(
            div()
                .w(px(tab.width))
                .h(px(CHIP_HEIGHT))
                .mr(px(frame.gap))
                .overflow_hidden()
                .relative()
                .left(px(tab.x_offset))
                .child(chip(
                    &tab.target,
                    tab.title.clone().into(),
                    active.as_ref() == Some(&tab.target),
                    region,
                    entity,
                )),
        );
    }
    let menu_open = app.tab_chrome.menu_open && show_plus;
    let mut controls = div()
        .h(px(CHIP_HEIGHT))
        .flex()
        .items_center()
        .gap(px(CHIP_GAP))
        .relative()
        .left(px(frame.controls_x_offset));
    if show_plus {
        controls = controls.child(new_tab_control(entity));
    }
    if show_collapse {
        controls = controls.child(
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
    rail = rail.child(controls);

    let strip = strip.child(rail);
    if menu_open {
        let width = frame.live.first().map_or(0.0, |chip| chip.width);
        let plus_x = descriptors.len() as f32 * (width + frame.gap) + frame.controls_x_offset;
        let viewport = f32::from(window.viewport_size().width);
        app.tab_chrome.menu_anchor_x =
            Some((window_x + plus_x).clamp(0.0, (viewport - 240.0).max(0.0)));
    } else {
        app.tab_chrome.menu_anchor_x = None;
    }
    strip.agent_node(Role::Card, "Tab strip")
}

/// Window-space close transition layer. Exit paint and the stable successor
/// hotspot live here rather than in either strip rail, so a pane resize or
/// thread/workspace ownership handoff cannot rebase or clip them.
pub(crate) fn tab_transition_layer(
    app: &mut Luma,
    entity: &Entity<Luma>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> AnyElement {
    let now = Instant::now();
    let frame = app
        .tab_chrome
        .transition_frame(motion::reduced_motion(cx), now);
    if frame.animating {
        window.request_animation_frame();
    }
    let mut layer = div().absolute().inset_0();
    if frame.stable_close.is_some() {
        let moved = entity.clone();
        layer = layer.on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
            moved.update(cx, |this, cx| {
                if this
                    .tab_chrome
                    .stable_pointer_moved(f32::from(event.position.x), f32::from(event.position.y))
                {
                    cx.notify();
                }
            });
        });
    }
    for exit in &frame.exits {
        layer = layer.child(exit_chip(exit));
    }
    if let Some(region) = frame.stable_close {
        layer = layer.child(stable_close_hotspot(entity, region));
    }
    layer.into_any_element()
}

/// The new-tab popover on the shell's window-space layer. Its anchor is
/// evaluated by [`tab_strip`] from live pane geometry, but it is deliberately
/// not a pane child: a menu opened during a panel's entrance must already be
/// full-sized instead of being cropped to the panel's intermediate width.
pub(crate) fn tab_menu_layer(app: &Luma, entity: &Entity<Luma>) -> AnyElement {
    let Some(x) = app
        .tab_chrome
        .menu_open
        .then_some(app.tab_chrome.menu_anchor_x)
        .flatten()
    else {
        return div().into_any_element();
    };
    let prerequisites = NewTabPrerequisites {
        venue: app
            .sidebar
            .as_ref()
            .map(|state| state.venue_id().to_string()),
        track: app.selected_track.clone(),
        pattern: app
            .selected_pattern
            .as_ref()
            .map(|pattern| pattern.id.clone()),
    };
    div()
        .absolute()
        .top(px(HEIGHT - 3.0))
        .left(px(x))
        .w(px(240.0))
        .child(
            // The menu's glass already carries translucency. Fading that
            // entire layer over a GPU-heavy timeline makes its first frame
            // unreadable (and some compositors expose jagged source texture
            // edges). Keep the authored 2px entrance, but its surface and
            // text remain fully opaque throughout.
            div()
                .w_full()
                .child(dialog::frosted(
                    10.0,
                    28.0,
                    new_tab_menu(entity, &prerequisites),
                ))
                .with_animation("new-tab-menu-in", motion::MENU_IN.animation(), |menu, t| {
                    menu.relative().top(px(-2.0 * (1.0 - t)))
                })
                .agent_node(Role::Card, "New tab menu"),
        )
        .into_any_element()
}

fn new_tab_control(entity: &Entity<Luma>) -> Div {
    let toggled = entity.clone();
    div().size(px(CONTROL)).relative().child(
        icon_button("new-tab", IconName::Plus)
            .on_click(move |_, _, cx| {
                toggled.update(cx, |this, cx| {
                    this.tab_chrome.toggle_menu();
                    cx.notify();
                });
            })
            .agent_node(Role::Button, "new-tab"),
    )
}

fn new_tab_menu(entity: &Entity<Luma>, prerequisites: &NewTabPrerequisites) -> Div {
    let mut menu = div()
        .w_full()
        .flex_none()
        .p(px(6.0))
        .rounded(px(10.0))
        .occlude()
        .border_1()
        .border_color(glass::hairline(0.12))
        .bg(glass::overlay())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    for availability in menu_choices(prerequisites) {
        let choice = availability.choice;
        let enabled = availability.enabled();
        let opened = entity.clone();
        let mut row = div()
            .id(SharedString::from(format!("new-tab:{}", choice.label())))
            .w_full()
            .min_h(px(38.0))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(7.0))
            .flex()
            .flex_col()
            .justify_center()
            .text_size(px(11.5))
            .text_color(if enabled {
                glass::ink(0.88)
            } else {
                glass::ink(0.30)
            })
            .when(enabled, |row| row.hover(|row| row.bg(glass::glass_hover())))
            .child(choice.label());
        if let Some(reason) = availability.reason {
            row = row.child(
                div()
                    .mt(px(2.0))
                    .text_size(px(9.5))
                    .text_color(glass::ink(0.32))
                    .child(reason)
                    .agent_node(Role::Text, reason),
            );
        }
        if enabled {
            row = row.on_click(move |_, _, cx| {
                opened.update(cx, |this, cx| this.activate_new_tab_choice(choice, cx));
            });
        }
        menu = menu.child(
            row.agent_node(Role::Button, choice.label())
                .agent_disabled(!enabled),
        );
    }
    menu
}

fn stable_close_hotspot(entity: &Entity<Luma>, region: PointerRegion) -> impl IntoElement {
    let closed = entity.clone();
    div()
        .id("stable-tab-close")
        .absolute()
        .left(px(region.x))
        .top(px(region.y))
        .w(px(region.width))
        .h(px(region.height))
        .rounded(px(3.0))
        .flex()
        .items_center()
        .justify_center()
        .hover(|button| button.bg(glass::wash(0.10)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            closed.update(cx, |this, cx| {
                let targets = this
                    .workspace
                    .iter()
                    .map(|tab| tab.target.clone())
                    .collect::<Vec<_>>();
                if let Some(target) = this
                    .tab_chrome
                    .stable_close_target(&targets, Instant::now())
                {
                    this.close_tab_at_window_region(&target, region, cx);
                }
            });
        })
        .child(Icon::new(IconName::Close).size(px(11.0)))
        .agent_node(Role::Button, "Close next tab")
}

fn exit_chip(exit: &crate::tab_chrome::ExitChipFrame) -> impl IntoElement {
    div()
        .absolute()
        .left(px(exit.x))
        .top(px(exit.y))
        .w(px(exit.width))
        .h(px(CHIP_HEIGHT))
        .opacity(exit.opacity)
        .overflow_hidden()
        .rounded(px(CHIP_RADIUS))
        .bg(glass::wash(0.08))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(Icon::new(kind_icon(&exit.target)).size(px(12.0)))
        .child(
            div()
                .truncate()
                .text_size(px(11.5))
                .child(exit.title.clone()),
        )
        .agent_node(
            Role::Card,
            format!("Closing {} opacity {:.3}", exit.title, exit.opacity),
        )
}

/// One tab chip: a leading icon slot that swaps in place for a ✕ on hover, the
/// title, active `wash(0.10)`, hover `wash(0.06)`. Click selects; the ✕
/// closes through the same teardown every close takes.
fn chip(
    target: &Target,
    title: SharedString,
    is_active: bool,
    close_region: PointerRegion,
    entity: &Entity<Luma>,
) -> impl IntoElement {
    let group = SharedString::from(format!("chip:{}", target.element_key()));
    let select = entity.clone();
    let selected = target.clone();
    let close = entity.clone();
    let closed = target.clone();
    let close_label = SharedString::from(format!("Close {title}"));
    let middle = entity.clone();
    let middle_closed = target.clone();
    div()
        .id(group.clone())
        .group(group.clone())
        .h(px(CHIP_HEIGHT))
        .w_full()
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
        .on_mouse_down(MouseButton::Middle, move |_, _, cx| {
            cx.stop_propagation();
            middle.update(cx, |this, cx| this.close_tab(&middle_closed, None, cx));
        })
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
                                this.close_tab(&closed, Some(close_region), cx);
                            });
                        })
                        .child(Icon::new(IconName::Close).size(px(11.)))
                        .agent_node(Role::Button, close_label),
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
