//! The window's own edges: the head band each region wears, drawn by us.
//!
//! The window draws no titlebar of its own on any platform, so nothing draws
//! chrome but this module — with one exception, the traffic lights on macOS,
//! which are AppKit's (see below). Everything painted here is **glass tier**
//! (`docs/specs/comet-shell.md` §6): the agent toggle, the venue menu's
//! trigger, the mode switch and, off macOS, our own traffic lights.
//!
//! # Two head bands, one line
//!
//! The window splits vertically first: the agent panel runs the window's full
//! height on the left, and the content column beside it carries the title bar
//! across its own width. The agent panel's head is the chat's own header; the
//! title bar is [`band`]. They align because they share one height,
//! [`HEIGHT`], which is also the chat header's.
//!
//! # The traffic lights are the topmost layer
//!
//! An overlay that covered them would be a modal that can neither be moved nor
//! closed, which is how a first-run picker becomes a trapped window. Who keeps
//! them on top depends on who draws them:
//!
//! - **macOS: AppKit.** The window asks for a transparent titlebar (see
//!   `main`), and the close / minimise / zoom buttons that come with it are the
//!   real ones. gpui moves them to [`traffic_light_position`] and moves them
//!   back after every resize, re-layout and fullscreen exit. They are views in
//!   the titlebar, which sits over the content view, so nothing we paint can
//!   cover them.
//! - **Everywhere else: us.** [`window_controls`] paints three dots at the
//!   window's top-left corner, and the shell paints it last — above the
//!   regions *and* above any overlay.
//!
//! On both, [`window_controls`] is the corner's one statement in the tree: an
//! inert slot the lights' width, empty on macOS because AppKit fills it.
//!
//! **In a macOS fullscreen space the corner is given back.** AppKit hides the
//! buttons until the menu bar drops over the top edge, so a reserved corner
//! would be an empty gap at rest. [`corner_lit`] says so, [`BandSpan::lights`]
//! carries it, and the left anchor slides into the corner.
//!
//! # The corner controls are anchors, not controls a region carries
//!
//! [`agent_toggle`] beside the lights and [`mode_switch`] at the trailing edge
//! are painted in window space, and the window's two corners are the only
//! place they appear. A control rendered *by* the agent panel rides its pane,
//! and a pane whose width is animating clips its own content, so the toggle
//! would vanish part-way through a hide. A fixed point cannot live on a moving
//! thing. So with the agent shown the lights and its toggle sit over the
//! panel's head; with it hidden the title bar starts at the window's edge and
//! they sit at its start, ahead of the venue menu:
//!
//! ```text
//! [lights][◧] venue ▾ ………… subject ………… [Rig · Arrange · Live]
//!  \___ fixed ___/                         \______ fixed ______/
//! ```
//!
//! The anchors hold still and the *clusters* move: [`band_insets`] is the one
//! statement of the room each edge owes them, read by [`band`] when it pads
//! itself. A panel opening pushes the title bar's cluster inward because the
//! bar starts where the panel's edge is this frame.

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};

use crate::workspace::{Mode, Target};
use crate::Luma;

/// Height of the title bar, and of the agent panel's head (the chat's
/// header) beside it.
pub const HEIGHT: f32 = luma_chat::theme::HEADER_HEIGHT;
/// One icon-button box in the band.
pub(crate) const CONTROL: f32 = 24.;
/// A band's inset from its region's edge, and the gap between its controls.
const BAND_PAD: f32 = 12.;
const BAND_GAP: f32 = 10.;
/// One traffic light, and the gap between them. On macOS these are AppKit's
/// own metrics as of macOS 26 (a 14px button every 23px); gpui keeps AppKit's
/// spacing and only moves the strip, so they are what the buttons occupy, not
/// a size we set. Earlier releases draw a narrower strip, which this room
/// still holds.
#[cfg(target_os = "macos")]
const LIGHT: f32 = 14.;
#[cfg(target_os = "macos")]
const LIGHT_GAP: f32 = 9.;
#[cfg(not(target_os = "macos"))]
const LIGHT: f32 = 12.;
#[cfg(not(target_os = "macos"))]
const LIGHT_GAP: f32 = 8.;

/// Room the traffic lights claim at the window's top-left corner, inset
/// included. Derived rather than measured so the two cannot drift.
const LIGHTS_WIDTH: f32 = BAND_PAD + 3. * LIGHT + 2. * LIGHT_GAP;

/// Where gpui places AppKit's close button: one [`BAND_PAD`] in, and centred
/// in the band, so the buttons share a centre line with every control beside
/// them.
pub fn traffic_light_position() -> Point<Pixels> {
    point(px(BAND_PAD), px((HEIGHT - LIGHT) / 2.))
}

/// Whether the window's top-left corner holds the traffic lights this frame.
/// False only in a macOS fullscreen space, where AppKit hides them.
pub(crate) fn corner_lit(window: &Window) -> bool {
    !(cfg!(target_os = "macos") && window.is_fullscreen())
}

/// Where the agent toggle sits: one [`BAND_GAP`] past the lights. With the
/// corner given back it takes the band's own inset, like any first control.
fn left_toggle_x(lights: bool) -> f32 {
    if lights {
        LIGHTS_WIDTH + BAND_GAP
    } else {
        BAND_PAD
    }
}

/// Where the window's left anchor block ends — the first pixel a band's own
/// content may use.
pub(crate) fn left_anchor_end(lights: bool) -> f32 {
    left_toggle_x(lights) + CONTROL + BAND_GAP
}

/// Where the window's right anchor block begins, measured back from the
/// trailing edge.
fn right_anchor_start(viewport: f32) -> f32 {
    viewport - BAND_PAD - MODE_SWITCH_WIDTH - BAND_GAP
}

/// Where a band sits in window space: what it needs to know to stay clear of
/// the anchors.
#[derive(Clone, Copy)]
pub(crate) struct BandSpan {
    pub x: f32,
    pub width: f32,
    pub viewport: f32,
    /// Whether the window's corner holds traffic lights — [`corner_lit`].
    pub lights: bool,
}

/// What a band owes each of its edges — the single statement of the anchors'
/// room.
///
/// Stated against the band's **position**, not against "am I the leftmost
/// region?": a panel one pixel open stops being leftmost while the bar beside
/// it still starts under the lights, so a band that asked the boolean would
/// snap its cluster on the first frame of the slide. Reserving by span makes
/// the cluster hold at the anchor until the panel's edge passes it, and ride
/// the edge after that.
pub(crate) fn band_insets(span: BandSpan) -> (f32, f32) {
    (
        (left_anchor_end(span.lights) - span.x).max(BAND_PAD),
        (span.x + span.width - right_anchor_start(span.viewport)).max(BAND_PAD),
    )
}

/// Window-space origin of a band's first child: where its leading inset ends.
#[cfg(test)]
pub(crate) fn band_origin(span: BandSpan) -> f32 {
    span.x + band_insets(span).0
}

/// The title bar: the drag strip its controls sit in.
///
/// No fill of its own, and a hairline under it: the bar is the content
/// column's head, and the stage below is a different panel.
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

/// A drag strip over the agent panel's head, left of the chat's own
/// buttons: the panel has no band of its own, and the window still has to be
/// movable by its top edge there.
pub(crate) fn head_drag_strip(width: f32) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(width.max(0.0)))
        .h(px(HEIGHT))
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
}

/// The agent panel's show/hide toggle: the window's fixed left anchor, one
/// gap right of the traffic lights.
pub(crate) fn agent_toggle(app: &Entity<Luma>, open: bool, lights: bool) -> impl IntoElement {
    let toggled = app.clone();
    anchor(px(left_toggle_x(lights)), None).child(
        div()
            .child(
                luma_ui::icon_toggle(IconName::PanelLeft, open)
                    .id("agent-toggle")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        toggled.update(cx, |this, cx| {
                            this.toggle_agent();
                            cx.notify();
                        });
                    }),
            )
            .agent_node(Role::Button, if open { "Hide agent" } else { "Show agent" }),
    )
}

/// How wide [`mode_switch`] is. Fixed, because the right anchor is reserved
/// by width before anything is laid out — see [`band_insets`] — and its cells
/// split it evenly, so the control does not change shape with the mode.
const MODE_SWITCH_WIDTH: f32 = 210.;

/// Rig · Arrange · Live: the fixed right anchor. Each cell carries, while
/// that mode's agent is working or finished unseen, its dot. The keys
/// (1 / 2 / 3) are the cells' order and are not drawn.
pub(crate) fn mode_switch(app: &Luma, entity: &Entity<Luma>, cx: &App) -> impl IntoElement {
    let on = app.workspace.mode();
    let enabled = app.songs.is_some();
    let track = luma_ui::float::segmented()
        .w(px(MODE_SWITCH_WIDTH))
        .children(Mode::ALL.into_iter().map(|mode| {
            let label = mode.label();
            let status = app
                .workspace
                .of_mode(mode)
                .and_then(|slot| app.body_status(&slot.target, cx));
            let pressed = entity.clone();
            luma_ui::float::segment(label, mode == on, label)
                .id(label)
                .gap(px(6.))
                .children(status.map(|status| crate::agent::status_dot(status, label)))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(format!("{label} · {}", mode.key()))
                        .build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(enabled, |cell| {
                    cell.on_click(move |_, _, cx| {
                        pressed.update(cx, |this, cx| this.set_mode(mode, cx))
                    })
                })
                .agent_node(Role::Toggle, label)
                .agent_disabled(!enabled)
        }));
    anchor(px(0.0), Some(px(BAND_PAD))).child(track.agent_node(Role::Card, "Modes"))
}

/// One window-space slot at the head band's height, pinned to an edge. `right`
/// wins when given.
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

/// The middle of the title bar: what the mode is working on. In Arrange, the
/// open song and score — the door to the song list. In Live, what is being
/// followed. In Rig, nothing.
pub(crate) fn subject(app: &Luma, entity: &Entity<Luma>) -> Option<AnyElement> {
    match app.workspace.active() {
        Some(Target::Score { .. }) | None if app.workspace.mode() == Mode::Arrange => {
            Some(song_title(app, entity))
        }
        Some(Target::Perform { .. }) => {
            let line = app.live_line()?;
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(luma_ui::glass::ink(0.6))
                    .child(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(luma_ui::ladder::status_ok()),
                    )
                    .child(line.clone())
                    .agent_node(Role::Text, line)
                    .into_any_element(),
            )
        }
        _ => None,
    }
}

/// Arrange's song title: the open score's track and score, or the offer to
/// pick one. A press opens or closes the song list.
fn song_title(app: &Luma, entity: &Entity<Luma>) -> AnyElement {
    let open = app.song_list_shown();
    let (art, title, score) = match app.workspace.active_body() {
        Some(crate::shell::Body::TrackEditor(editor)) => (
            app.songs
                .as_ref()
                .and_then(|songs| songs.find(editor.track_id()))
                .and_then(|track| track.album_art_path),
            editor.track_name().to_string(),
            editor.score_label(),
        ),
        _ => (None, "Songs".to_string(), String::new()),
    };
    let pressed = entity.clone();
    let label = if score.is_empty() {
        title.clone()
    } else {
        format!("{title} · {score}")
    };
    luma_ui::float::nav_row(luma_ui::float::RowState::of(open, false), "song-title")
        .id("song-title")
        .flex_none()
        .gap(px(10.))
        .px(px(10.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| pressed.update(cx, |this, cx| this.toggle_song_list(cx)))
        .children(
            art.is_some()
                .then(|| crate::tracks::album_art(art.as_deref(), 20.)),
        )
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(luma_ui::glass::ink(0.95))
                .child(title),
        )
        .when(!score.is_empty(), |row| {
            row.child(div().text_color(luma_ui::glass::ink(0.6)).child(score))
        })
        .child(
            gpui_component::Icon::new(if open {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            })
            .size(px(11.))
            .text_color(luma_ui::glass::ink(0.42)),
        )
        .agent_node(Role::Button, label)
        .into_any_element()
}

/// The window's traffic-light corner: AppKit's slot on macOS, our three dots
/// everywhere else. `None` while the corner is given back ([`corner_lit`]).
///
/// **Nothing here is interactive but the painted dots.** A press anywhere
/// else in the corner reaches whatever this is painted over, and dragging the
/// corner still moves the window.
pub(crate) fn window_controls(window: &Window) -> Option<AnyElement> {
    if !corner_lit(window) {
        return None;
    }
    let slot = div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(LIGHTS_WIDTH))
        .h(px(HEIGHT))
        .pl(px(BAND_PAD))
        .flex()
        .items_center()
        .gap(px(LIGHT_GAP));
    let slot = if cfg!(target_os = "macos") {
        slot
    } else {
        slot.child(light("close", rgb(0xff5f57), |window| {
            window.remove_window()
        }))
        .child(light("minimize", rgb(0xfebc2e), |window| {
            window.minimize_window()
        }))
        .child(light("maximize", rgb(0x28c840), |window| {
            window.zoom_window()
        }))
    };
    Some(
        slot.agent_node(Role::Card, "Window controls")
            .into_any_element(),
    )
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
