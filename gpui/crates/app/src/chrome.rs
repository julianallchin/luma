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
//! starts at `y = 0` wears a band**, and a band that reaches the window's
//! corner leaves that corner free. A region does, and so does an overlay plane
//! — one mechanism rather than an opt-in every future surface has to remember.
//! The strip itself is inert apart from the three dots, so forgetting is a
//! visible overlap and never a corner that silently swallows presses.
//!
//! # The panel toggles are anchors, not controls a region carries
//!
//! [`sidebar_toggle`] and [`panel_toggle`] are painted in window space beside
//! the lights, and the window's two corners are the only place they appear.
//! A toggle rendered *by* a region rides that region's pane, and a pane whose
//! width is animating clips its own band — which is why the sidebar's toggle
//! used to vanish part-way through a close and reappear beside back/forward
//! once the slide had finished. A fixed point cannot live on a moving thing.
//!
//! So the rule the whole band reads:
//!
//! ```text
//! [lights][◧] left cluster …………………… right cluster [◨]
//!  \___ fixed ___/                                  \_ fixed _/
//! ```
//!
//! The toggles hold still and the *clusters* move: [`band_insets`] is the one
//! statement of the room each edge owes them, read by [`band`] when it pads
//! itself and by [`band_room`] when it offers a band's flexible child what is
//! left. A panel opening pushes its neighbour's cluster inward because the
//! neighbour's band starts where the panel's edge is this frame — the live
//! width *is* the curve, so nothing here re-states one.

use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{float, glass, ladder, motion, radius};

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
/// One icon-button box in the band — the gear, a panel toggle, the `+`. The
/// shell budgets bands against it, so it is stated here once rather than
/// restated as a second 24 beside the code that reads it.
pub(crate) const CONTROL: f32 = 24.;
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
/// Where the sidebar toggle sits: one [`BAND_GAP`] past the lights, which is
/// the spacing every other pair of controls in a band keeps. Flush against
/// [`LIGHTS_WIDTH`] the toggle's hover wash touches the green dot.
const LEFT_TOGGLE_X: f32 = LIGHTS_WIDTH + BAND_GAP;
/// Where the window's left anchor block ends — the first pixel a band's own
/// content may use.
const LEFT_ANCHOR_END: f32 = LEFT_TOGGLE_X + CONTROL + BAND_GAP;
/// Where the window's right anchor block begins, measured back from the
/// trailing edge. The panel toggle itself is inset by [`BAND_PAD`], so this is
/// one gap further in.
fn right_anchor_start(viewport: f32) -> f32 {
    viewport - BAND_PAD - CONTROL - BAND_GAP
}

/// Where a band sits in window space: what it needs to know to stay clear of
/// the anchors. The shell already computes both for every region.
#[derive(Clone, Copy)]
pub(crate) struct BandSpan {
    pub x: f32,
    pub width: f32,
    pub viewport: f32,
}

/// What a band owes each of its edges — the single statement of the anchors'
/// room. [`band`] pads itself with it and [`band_room`] subtracts it, so the
/// space a child is *offered* can never disagree with the space the band
/// actually leaves.
///
/// Stated against the band's **position**, not against "am I the leftmost
/// region?". The boolean is the same answer only at the two rest widths: a
/// sidebar one pixel open stops being leftmost while its neighbour still
/// starts under the lights, so a band that asked the boolean snapped its
/// cluster 84px left on the first frame of the slide and then crept back out
/// from beneath the toggle. Reserving by span makes the cluster hold at the
/// anchor until the panel's edge passes it and ride the edge after that —
/// which is the whole behaviour, in one `max`.
pub(crate) fn band_insets(span: BandSpan) -> (f32, f32) {
    (
        (LEFT_ANCHOR_END - span.x).max(BAND_PAD),
        (span.x + span.width - right_anchor_start(span.viewport)).max(BAND_PAD),
    )
}

/// Width a band's flexible child may claim after its fixed siblings and the
/// gaps between them. Keeping this arithmetic beside the band constants
/// prevents a narrow window from laying out fixed slots past the viewport.
pub(crate) fn band_room(
    span: BandSpan,
    leading_width: f32,
    trailing_width: f32,
    sibling_gaps: usize,
) -> f32 {
    let (left, right) = band_insets(span);
    (span.width - left - right - leading_width - trailing_width - sibling_gaps as f32 * BAND_GAP)
        .max(0.0)
}

/// Window-space origin of the tab strip. It is the panel band's first child
/// (see [`tab_strip`]), so this is where that band's leading inset ends.
pub(crate) fn tab_strip_origin(span: BandSpan) -> f32 {
    span.x + band_insets(span).0
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
/// under it would be the one horizontal border this shell does not have. The
/// span is where the region sits this frame, which is what yields the window's
/// corners to [`window_controls`] and the two panel toggles.
pub(crate) fn band(span: BandSpan) -> Div {
    let (left, right) = band_insets(span);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(BAND_GAP))
        .h(px(HEIGHT))
        .pl(px(left))
        .pr(px(right))
        // The whole band is the drag region; every control stops propagation
        // so a press doesn't also start a move.
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
}

/// The sidebar's show/hide toggle: the window's fixed left anchor, one gap
/// right of the traffic lights. See the module docs for why it is painted here
/// and not by whichever region happens to be leftmost.
pub(crate) fn sidebar_toggle(app: &Entity<Luma>, open: bool, enabled: bool) -> impl IntoElement {
    panel_anchor(
        anchor(px(LEFT_TOGGLE_X), None),
        "sidebar-toggle",
        IconName::PanelLeft,
        open,
        enabled,
        app,
        |this| this.sidebar_hidden = !this.sidebar_hidden,
    )
}

/// The workspace panel's show/hide toggle: the window's fixed right anchor.
pub(crate) fn panel_toggle(app: &Entity<Luma>, open: bool, enabled: bool) -> impl IntoElement {
    panel_anchor(
        anchor(px(0.0), Some(px(BAND_PAD))),
        "panel-toggle",
        IconName::PanelRight,
        open,
        enabled,
        app,
        |this| this.workspace_hidden = !this.workspace_hidden,
    )
}

/// One toggle in its fixed slot. Both corners are the same control with a
/// different icon and a different flag, so they are the same function: an
/// anchor that behaved differently on one side would be two rules wearing one
/// name.
///
/// `enabled` is false when there is nothing to toggle — no venue on the left,
/// no tabs on the right. Dimmed and inert then, the way [`history_pair`] is:
/// the chrome's anatomy holds still, and a control that cannot act says so
/// rather than vanishing and moving everything beside it.
fn panel_anchor(
    slot: Div,
    id: &'static str,
    icon: IconName,
    open: bool,
    enabled: bool,
    app: &Entity<Luma>,
    toggle: fn(&mut Luma),
) -> impl IntoElement {
    let button = if enabled {
        toggle_button(id, icon, open)
            .on_click({
                let toggled = app.clone();
                move |_, _, cx| {
                    toggled.update(cx, |this, cx| {
                        toggle(this);
                        cx.notify();
                    });
                }
            })
            .into_any_element()
    } else {
        dim_icon(icon).into_any_element()
    };
    slot.child(
        div()
            .child(button)
            .agent_node(Role::Button, id)
            .agent_disabled(!enabled),
    )
}

/// One window-space slot at the head band's height, pinned to an edge. `right`
/// wins when given; the toggles are the only callers and each picks one side.
fn anchor(left: Pixels, right: Option<Pixels>) -> Div {
    div()
        .absolute()
        .top_0()
        .h(px(HEIGHT))
        .flex()
        .items_center()
        .map(|slot| match right {
            Some(right) => slot.right(right),
            None => slot.left(left),
        })
}

/// What [`history_pair`] occupies in a band, gaps included. The shell budgets
/// a strip against it and used to restate `2 * 24 + 10` to do so — two places
/// that had to be edited together, one of which did not know the constants.
pub(crate) const HISTORY_PAIR_WIDTH: f32 = 2.0 * CONTROL + BAND_GAP;

/// Back/forward: comet's chrome carries them always, dimmed when there is
/// nowhere to go. This shell has no navigation history — nothing is destroyed,
/// so there is nothing to go back to — and the pair is permanently at rest
/// until a history exists to drive it.
pub(crate) fn history_pair() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(BAND_GAP))
        // Published disabled rather than left out of the tree: this pair *is*
        // the left cluster the fixed toggle pushes, so where it sits is the
        // observable half of the anchor rule (`chrome_anchors`).
        .child(
            dim_icon(IconName::ArrowLeft)
                .agent_node(Role::Button, "Back")
                .agent_disabled(true),
        )
        .child(
            dim_icon(IconName::ArrowRight)
                .agent_node(Role::Button, "Forward")
                .agent_disabled(true),
        )
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
    toggle_button(id, icon, false)
}

/// [`icon_button`] that also reads its own state: a panel toggle wears the
/// active tab chip's wash while the panel it opens is showing, so "which
/// panels are up" is legible from the corners without counting regions.
///
/// One `hover` call, branching inside — gpui panics on a second one, and a
/// wrapper that added its own active style on top of `icon_button`'s is
/// exactly that.
fn toggle_button(id: &'static str, icon: IconName, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(CONTROL))
        .rounded(px(CHIP_RADIUS))
        .flex()
        .items_center()
        .justify_center()
        .text_color(glass::ink(if active { 0.92 } else { 0.55 }))
        .when(active, |button| button.bg(glass::wash(glass::WASH_REST)))
        .hover(move |button| {
            button
                .bg(glass::wash(if active {
                    glass::WASH_EMPHASIS
                } else {
                    glass::WASH_SUBTLE
                }))
                .text_color(glass::ink(if active { 0.92 } else { 0.90 }))
        })
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

/// The shell tab strip: one rounded chip per open tab, and the `+`.
///
/// # The strip belongs to the panel
///
/// It rides the workspace panel's own band and nowhere else, so it opens and
/// closes with the thing it is about — comet right-aligns its strip into a
/// full-width bar to fake the same relationship. The `+` is part of the
/// **strip** rather than of the band, for the same reason: it extends a row of
/// tabs, so it is wherever that row is.
///
/// A band that could *borrow* the strip when the panel was away is what this
/// replaced, and it cost two rules the shell no longer has to keep: which band
/// owns the strip this frame, and where the `+` goes when ownership changes
/// hands mid-close. Closing the panel now simply puts its tabs away with it,
/// and ⌘T opens the panel before it offers anything (see `Luma::render`).
///
/// With **no** tabs there is no `+`: the panel's own empty state is the offer
/// then, and two offers for one question is the thing that rule prevents.
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
    let show_plus = !descriptors.is_empty() && available_width >= CONTROL;
    let controls_width = if show_plus { CONTROL } else { 0.0 };
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
    rail = rail.child(controls);

    let mut strip = strip.child(rail);
    // The menu hangs off the strip through the house floating layer:
    // `deferred(…).priority(1)` lifts it above everything painted in normal
    // order — the window controls included — and `anchored` owns the
    // off-screen fitting a hand clamp used to approximate. An overlay up
    // means the menu yields, as it always has.
    if menu_open && app.overlay.get().is_none() {
        let prerequisites = app.new_tab_prerequisites();
        strip = strip.child(float::anchored_at(
            "new-tab-menu",
            point(px(window_x + frame.controls_x), px(HEIGHT - 3.0)),
            div()
                .w(px(240.0))
                .child(new_tab_menu(entity, &prerequisites))
                .agent_node(Role::Card, "New tab menu")
                .into_any_element(),
        ));
    }
    strip.agent_node(Role::Card, "Tab strip")
}

/// Window-space close transition layer. Exit paint and the stable successor
/// hotspot live here rather than in the strip's own rail, so the panel sliding
/// or being dragged under them cannot rebase or clip them.
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

/// The `+` menu's card: the house popover surface with the house rows, not a
/// third drawing of either. Rows are [`float::menu_row`], so hover fades the
/// way every other floating row's does; a choice that cannot act dims whole
/// ([`float::INERT_OPACITY`]) rather than minting its own grey.
fn new_tab_menu(entity: &Entity<Luma>, prerequisites: &NewTabPrerequisites) -> Div {
    let mut menu = float::popover_card()
        .w_full()
        .flex_none()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    for availability in menu_choices(prerequisites) {
        let choice = availability.choice;
        let enabled = availability.enabled();
        let opened = entity.clone();
        let key = SharedString::from(format!("new-tab:{}", choice.label()));
        let mut column = div().flex().flex_col().gap(px(2.0)).child(choice.label());
        if let Some(reason) = availability.reason {
            column = column.child(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::foreground_alpha(0.45))
                    .child(reason)
                    .agent_node(Role::Text, reason),
            );
        }
        let mut row = float::menu_row(float::RowState::Rest, key.clone())
            .id(key)
            .w_full()
            .min_h(px(38.0))
            .child(column);
        if enabled {
            row = row.on_click(move |_, _, cx| {
                opened.update(cx, |this, cx| this.activate_new_tab_choice(choice, cx));
            });
        } else {
            row = row.cursor_default().opacity(float::INERT_OPACITY);
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
        .rounded(px(radius::CHIP))
        .flex()
        .items_center()
        .justify_center()
        .hover(|button| button.bg(glass::wash(glass::WASH_REST)))
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
            chip.bg(glass::wash(glass::WASH_REST))
                .text_color(glass::ink(0.92))
        })
        .when(!is_active, |chip| {
            chip.text_color(glass::ink(0.55))
                .hover(|chip| chip.bg(glass::wash(glass::WASH_SUBTLE)))
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
        Target::Universe { .. } => IconName::Cpu,
    }
}
