//! The persistent shell: three regions, one overlay slot, and the workspace's
//! tabs — the layer that replaced the `Screen` router.
//!
//! # Nothing is destroyed to show something else
//!
//! The old router held one screen and a provenance chain (`from`, `previous`,
//! `browser`) so Back could restore what opening something had thrown away.
//! Here nothing is thrown away, so there is nothing to restore and no Back:
//! the sidebar's browser persists beside the editor it opened, a tab keeps its
//! state while another is showing, and an overlay covers the regions without
//! displacing them. `docs/specs/comet-shell.md` is the contract.
//!
//! # Switching is not closing
//!
//! [`Tabs::close`] hands a [`Body`] back and [`Luma::teardown`] is the only
//! consumer — pausing a closed editor's transport, clearing its loop region.
//! Switching tabs goes nowhere near it, which is what makes "the transport
//! keeps playing across a tab switch" structural rather than remembered.
//!
//! # One overlay at a time
//!
//! [`Overlay`] is a single slot, not a stack: venues, patterns and settings
//! are all "a plane over the shell, dismissed by Escape", and three overlays
//! that each invented their own dismissal would be three ways to do one thing.
//! The venue picker re-opens itself whenever nothing is selected and nothing
//! else is up — the shell with no venue *is* the picker, so "dismissed the
//! picker into an empty shell" is a state that cannot be reached.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Div, DragMoveEvent, SharedString, Window};

use luma_ui::node::{Instrument, Role};
use luma_ui::pane;
use luma_ui::{glass, ladder};

use crate::tabs::Target;
use crate::{
    add_tracks, chrome, graph, keymap, patterns, settings, track_editor, tracks, universe,
    visualizer, welcome, Luma,
};

/// How wide the sidebar and the shared workspace open. Comet's defaults; the
/// workspace's is the width it *starts* at, since its seam can be dragged.
pub(crate) const SIDEBAR_WIDTH: f32 = 256.0;
pub(crate) const WORKSPACE_WIDTH: f32 = 520.0;
/// How far the workspace panel's seam can be dragged. There is no absolute
/// cap: the panel may take everything except a still-usable thread column, so
/// the bound is stated as what must *remain*, not what may be taken.
const WORKSPACE_MIN: f32 = 320.0;
const CENTER_MIN: f32 = 360.0;
const TAB_CONTROL_WIDTH: f32 = 24.0;
/// How wide a seam between two regions is. One device pixel's worth of rule at
/// 1×, and the only structural line the shell draws.
const SEAM_WIDTH: f32 = 1.0;

/// One plane over the whole shell. The regions persist beneath it — closing
/// an overlay reveals them exactly as they were.
pub(crate) enum Overlay {
    /// The venue picker: the old welcome grid, re-homed. Auto-opens while no
    /// venue is selected, because a shell with no subject list has nothing
    /// else to offer.
    Venues(welcome::VenuePicker),
    /// The pattern picker. Picking a row opens a [`Target::Graph`] tab.
    Patterns(patterns::Patterns),
    Settings(settings::Settings),
    AddTracks(Box<add_tracks::AddTracks>),
}

impl Overlay {
    /// The key context the overlay's root declares — nested inside
    /// [`keymap::context::ROOT`], beside (not inside) the regions.
    fn key_context(&self) -> &'static str {
        match self {
            Self::Venues { .. } => keymap::context::VENUES,
            Self::Patterns(_) => keymap::context::PATTERNS,
            Self::Settings(_) => keymap::context::SETTINGS,
            Self::AddTracks(_) => keymap::context::ADD_TRACKS,
        }
    }
}

/// A workspace tab's state: the editor behind one [`Target`].
///
/// The variants mirror `Target`'s — a target is *what* the tab shows, and a
/// body is the state showing it.
pub(crate) enum Body {
    TrackEditor(Box<track_editor::Editor>),
    Graph(Box<graph::Editor>),
    Visualizer(Box<visualizer::Visualizer>),
    Universe(Box<universe::Universe>),
}

impl Body {
    /// What the tab's chip and the window title call this tab.
    pub(crate) fn title(&self) -> SharedString {
        match self {
            Self::TrackEditor(state) => state.track_name().to_string().into(),
            Self::Graph(state) => state.pattern_name().to_string().into(),
            Self::Visualizer(state) => state.venue_name().to_string().into(),
            Self::Universe(state) => state.venue_name().to_string().into(),
        }
    }
}

/// What should hold the keyboard this frame. Compared across frames so a
/// change of subject re-takes focus while a field the user clicked into keeps
/// it — the same contract the old per-screen `take_focus` kept, restated over
/// regions.
#[derive(PartialEq, Eq, Clone)]
pub(crate) enum FocusSlot {
    Overlay(&'static str),
    Tab(Target),
    Shell,
}

impl Luma {
    /// Where the keyboard belongs this frame: the overlay when one is up,
    /// else the active tab, else the shell root (whose dispatch path still
    /// reaches every ROOT-scoped binding).
    pub(crate) fn focus_slot(&self) -> FocusSlot {
        if let Some(overlay) = &self.overlay {
            return FocusSlot::Overlay(overlay.key_context());
        }
        // A hidden panel still *has* an active tab, but that tab is not on
        // screen: leaving the keyboard with it would track the one focus
        // handle at an element no frame renders, and every action dispatched
        // from there — including the one that shows the panel again — would
        // dead-end. Hiding the workspace hands the keyboard back to the shell.
        if let Some(target) = self.workspace.active() {
            if !self.workspace_hidden {
                return FocusSlot::Tab(target.clone());
            }
        }
        FocusSlot::Shell
    }

    /// Take the keyboard when the slot changed hands or nothing holds it.
    pub(crate) fn take_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let slot = self.focus_slot();
        if slot == self.focused_slot && window.focused(cx).is_some() {
            return;
        }
        let leaving_overlay = matches!(self.focused_slot, FocusSlot::Overlay(_))
            && !matches!(slot, FocusSlot::Overlay(_));
        let entering_overlay = !matches!(self.focused_slot, FocusSlot::Overlay(_))
            && matches!(slot, FocusSlot::Overlay(_));
        if entering_overlay {
            self.overlay_return_focus = window.focused(cx).map(|focus| focus.downgrade());
        }
        self.focused_slot = slot;
        if matches!(self.focused_slot, FocusSlot::Overlay(_)) {
            window.focus(&self.dialog_focus, cx);
            return;
        }
        let return_focus = leaving_overlay
            .then(|| {
                self.overlay_return_focus
                    .take()
                    .and_then(|focus| focus.upgrade())
            })
            .flatten();
        if let Some(focus) = return_focus {
            window.focus(&focus, cx);
            return;
        }
        window.focus(&self.focus, cx);
    }

    /// Reveal `target`, building its body only if it is not already open.
    /// The one entry point every open gesture routes through — see
    /// [`Tabs::open`].
    pub(crate) fn open_tab(
        &mut self,
        target: Target,
        build: impl FnOnce() -> Body,
        cx: &mut Context<Self>,
    ) {
        self.workspace.open(target, build);
        self.workspace_hidden = false;
        cx.notify();
    }

    /// Close the visible tab and run its state's own close semantics. Bound to
    /// ⌘W inside the workspace; also the handler behind every tab-closing
    /// gesture, so the teardown cannot be skipped by one of them.
    pub(crate) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        self.close_tab(&target, None, cx);
    }

    /// A closed tab's exit rites. The only caller of anything here is a
    /// *close* — switching tabs never comes through this path.
    pub(crate) fn teardown(&mut self, body: Body, cx: &mut Context<Self>) {
        match body {
            Body::TrackEditor(mut state) => {
                // The loop belongs to the transport, which outlives the tab: a
                // region left armed would wrap the *next* track at times that
                // meant something on this one. Same for playback itself.
                let looping = state.take_loop_region();
                let pause = self.library.pause();
                cx.background_spawn(async move {
                    pause.await.ok();
                })
                .detach();
                if looping {
                    let clear = self.library.set_loop_region(None);
                    cx.background_spawn(async move {
                        clear.await.ok();
                    })
                    .detach();
                }
            }
            Body::Graph(_) | Body::Visualizer(_) | Body::Universe(_) => {}
        }
    }

    /// Escape: the insertion menu first (it is the thing the eye is on), then
    /// the overlay. The venue picker with nothing selected stays — see the
    /// module docs.
    pub(crate) fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        if self.tab_chrome.dismiss_menu() {
            cx.notify();
            return;
        }
        if self.dismiss_insert_menu() {
            cx.notify();
            return;
        }
        match &self.overlay {
            Some(Overlay::Venues(_)) if self.sidebar.is_none() => {}
            Some(_) => {
                self.overlay = None;
                cx.notify();
            }
            None => {}
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// The whole window: full-height region columns, the seams between them, the
/// overlay over all of it, and the traffic lights over that.
///
/// **The window splits vertically first.** Every region runs `y = 0` to the
/// bottom and carries its own head band across its own width (see
/// [`crate::chrome`]), so the seams between regions are uninterrupted rules
/// from edge to edge. Regions are flush and square: no insets, no gutters, no
/// rounded cards. Depth is a value step across a seam — the one structural
/// line this shell draws, and the only border it has in either axis.
pub(crate) fn regions(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> Div {
    let entity = cx.entity();
    // Geometry follows state. Each edge region's resting width is restated
    // here every frame as a pure function of what the shell is showing, so a
    // toggle only flips a flag and `retarget` — a no-op while the destination
    // is unchanged — turns that into a slide.
    app.sidebar_width.retarget(app.sidebar_slot(), cx);
    let sidebar_w = app.sidebar_width.eval(window);
    app.workspace_width.retarget(app.workspace_slot(), cx);
    let viewport = f32::from(window.viewport_size().width);
    let workspace_w = px(f32::from(app.workspace_width.eval(window))
        .min(workspace_max(viewport, f32::from(sidebar_w))));

    // Takeover: an open workspace covers everything right of the sidebar and
    // the thread column collapses behind it. The default is comet's split;
    // `ToggleExpand` trades the thread's room for the tab's.
    let takeover = app.expanded && !app.workspace_hidden && !app.workspace.is_empty();
    let show_sidebar = app.sidebar.is_some() && sidebar_w > px(0.0);
    let show_thread = !takeover;
    let show_workspace = !app.workspace.is_empty() && (takeover || workspace_w > px(0.0));
    let workspace_leftmost = !show_sidebar && !show_thread;
    let workspace_panel_width = if takeover {
        viewport - f32::from(sidebar_w) - if show_sidebar { SEAM_WIDTH } else { 0.0 }
    } else {
        f32::from(workspace_w)
    };
    let (workspace_strip_width, workspace_show_settings) =
        workspace_band_tab_strip(workspace_panel_width, workspace_leftmost);
    // Exactly one band owns the shared strip. A workspace that exists but is
    // still only a sliver during pane motion cannot own it yet: the thread
    // keeps painting the same TabChrome until the receiving band can reserve
    // the complete add control, then ownership transfers in one frame.
    let workspace_owns_tab_strip = show_workspace && workspace_strip_width >= TAB_CONTROL_WIDTH;

    let mut row = div()
        .size_full()
        .flex()
        .flex_row()
        .relative()
        .on_drag_move(cx.listener(Luma::drag_workspace_seam));

    if let Some(browser) = &app.sidebar {
        if show_sidebar {
            let body = column(chrome::band(true).child(chrome::sidebar_toggle(&entity)))
                // The one region that raises itself off the ground, because it
                // is the frame and not the subject. Every other plane in the
                // shell is the floor; this step *is* the depth model.
                .bg(ladder::chrome_plane())
                .key_context(keymap::context::SIDEBAR)
                .child(tracks::sidebar(browser, &entity, window))
                .into_any_element();
            // Laid out at its full width for the whole slide, so a sidebar
            // easing open reveals its rows rather than re-wrapping them.
            row = row.child(pane::pane(sidebar_w, px(SIDEBAR_WIDTH), body));
            // The sidebar and whatever is beside it are different planes, so
            // the line between them is the bright one.
            row = row.child(seam(ladder::seam_plane()));
        }
    }

    if show_thread {
        let leftmost = !show_sidebar;
        let mut head = chrome::band(leftmost);
        if !show_sidebar {
            head = head.child(chrome::sidebar_toggle(&entity));
        }
        head = head.child(chrome::history_pair());
        if !workspace_owns_tab_strip {
            let leading_width = 2.0 * 24.0 + 10.0 + if show_sidebar { 0.0 } else { 24.0 };
            let panel_width = viewport
                - f32::from(sidebar_w)
                - if show_sidebar { SEAM_WIDTH } else { 0.0 }
                - if show_workspace {
                    f32::from(workspace_w) + SEAM_WIDTH
                } else {
                    0.0
                };
            let leading_children = if show_sidebar { 1 } else { 2 };
            let (strip_width, show_settings) = if show_workspace {
                (
                    chrome::tab_strip_room(
                        panel_width,
                        leftmost,
                        leading_width,
                        0.0,
                        leading_children,
                    ),
                    false,
                )
            } else {
                let gaps_with_settings = if show_sidebar { 3 } else { 4 };
                let room_with_settings = chrome::tab_strip_room(
                    panel_width,
                    leftmost,
                    leading_width,
                    TAB_CONTROL_WIDTH,
                    gaps_with_settings,
                );
                let show_settings = room_with_settings >= TAB_CONTROL_WIDTH;
                (
                    if show_settings {
                        room_with_settings
                    } else {
                        chrome::tab_strip_room(
                            panel_width,
                            leftmost,
                            leading_width,
                            0.0,
                            leading_children,
                        )
                    },
                    show_settings,
                )
            };
            let panel_x = f32::from(sidebar_w) + if show_sidebar { SEAM_WIDTH } else { 0.0 };
            let strip_x =
                chrome::tab_strip_origin(panel_x, leftmost, leading_width, leading_children);
            head = head.child(chrome::tab_strip(
                app,
                &entity,
                strip_width,
                strip_x,
                window,
                cx,
            ));
            if show_settings {
                head = head
                    .child(div().flex_1())
                    .child(chrome::settings_button(&entity));
            }
        } else {
            head = head.child(div().flex_1());
        }
        row = row.child(
            column(head)
                .flex_1()
                // The ground, not a card on it: the thread is what the app is
                // about, and the content plane is the darkest one there is.
                .bg(ladder::background())
                .key_context(keymap::context::THREAD)
                .children(app.chat.clone()),
        );
    }

    if show_workspace {
        let mut head = chrome::band(workspace_leftmost);
        if workspace_owns_tab_strip {
            let strip_x = chrome::tab_strip_origin(
                viewport - workspace_panel_width,
                workspace_leftmost,
                0.0,
                0,
            );
            head = head.child(chrome::tab_strip(
                app,
                &entity,
                workspace_strip_width,
                strip_x,
                window,
                cx,
            ));
        }
        if workspace_show_settings {
            head = head
                .child(div().flex_1())
                .child(chrome::settings_button(&entity));
        } else if !workspace_owns_tab_strip {
            head = head.child(div().flex_1());
        }
        if show_thread {
            // Both sides are lit surfaces whose own value step already divides
            // them, so this rule is a hint — and it is what the pointer grabs.
            row = row.child(workspace_seam(cx));
        }
        // The same ground the thread column is — which is why the rule between
        // them is [`ladder::seam_hint`] and not the bright one. Opaque: a tab
        // holds an instrument surface, and a timeline read through a blurred
        // desktop is a timeline you cannot read.
        let panel = column(head)
            .bg(ladder::background())
            .key_context(keymap::context::WORKSPACE)
            .child(active_tab(app, window, cx))
            .into_any_element();
        row = row.child(if takeover {
            div().h_full().flex_1().min_w_0().child(panel)
        } else {
            pane::pane(workspace_w, px(app.workspace_open_width), panel)
        });
    }

    // Window-space tab exits and their stable close target sit above both pane
    // rails, but below popovers and modal overlays.
    row = row.child(chrome::tab_transition_layer(app, &entity, window, cx));

    if app.overlay.is_none() {
        row = row.child(chrome::tab_menu_layer(app, &entity));
    }

    let dialog_focus = app.dialog_focus.clone();
    if let Some(Overlay::Venues(state)) = &mut app.overlay {
        welcome::tick(state, window, cx);
    }
    if let Some(Overlay::AddTracks(state)) = &mut app.overlay {
        add_tracks::tick(state, &dialog_focus, window, cx);
    }
    if let Some(overlay) = &app.overlay {
        row = row.child(overlay_layer(app, overlay, &entity, sidebar_w, window, cx));
    }
    // Last, so nothing — least of all an overlay — can cover the only controls
    // that move and close the window. See [`crate::chrome`].
    row.child(chrome::window_controls())
}

/// One region: its head band, then whatever it shows, floor to ceiling.
fn column(head: Div) -> Div {
    div().h_full().min_w_0().flex().flex_col().child(head)
}

/// The rule between two regions: full height, one pixel, no radius.
fn seam(color: gpui::Rgba) -> Div {
    div().h_full().flex_none().w(px(SEAM_WIDTH)).bg(color)
}

/// The most room the workspace may occupy while leaving the live centre
/// column readable. The sidebar seam exists exactly while any of its panel is
/// visible, so it belongs in the budget too.
fn workspace_max(viewport: f32, sidebar: f32) -> f32 {
    let sidebar_seam = if sidebar > 0.0 { SEAM_WIDTH } else { 0.0 };
    (viewport - sidebar - sidebar_seam - CENTER_MIN - SEAM_WIDTH).max(0.0)
}

/// Allocate the workspace band from its live width. Settings yields before the
/// add control; callers use the returned room as the sole ownership boundary
/// for the shared strip.
fn workspace_band_tab_strip(panel_width: f32, leftmost: bool) -> (f32, bool) {
    let room_with_settings =
        chrome::tab_strip_room(panel_width, leftmost, 0.0, TAB_CONTROL_WIDTH, 2);
    let show_settings = room_with_settings >= TAB_CONTROL_WIDTH;
    let strip_width = if show_settings {
        room_with_settings
    } else {
        chrome::tab_strip_room(panel_width, leftmost, 0.0, 0.0, 0)
    };
    (strip_width, show_settings)
}

/// The workspace panel's seam, which is also its drag handle: a hint-toned
/// rule with a wider invisible grip floating over it, so the pointer can hit
/// the boundary without the boundary having to be thick enough to aim at.
/// Double-click restores the default width — the gesture comet's seams answer
/// to.
fn workspace_seam(cx: &mut Context<Luma>) -> Div {
    let grip = pane::resize_handle(
        "workspace-seam",
        || WorkspaceResize,
        |app: &mut Luma, _| app.workspace_open_width = WORKSPACE_WIDTH,
        glass::glass_hover(),
        cx,
    );
    seam(ladder::seam_hint()).relative().child(
        grip.absolute()
            .top_0()
            .left(px((SEAM_WIDTH - pane::HANDLE_WIDTH) / 2.0))
            // A slider is the closest thing in the closed role vocabulary to a
            // grip whose position along an axis is the value — which is what
            // the seam is.
            .agent_node(Role::Slider, "Workspace width"),
    )
}

/// The workspace panel's left seam, under the pointer. gpui routes a drag by
/// the type it carries, so this marker is what tells the row's listener that
/// the pointer belongs to this seam rather than to anything else being dragged.
struct WorkspaceResize;

impl Luma {
    /// How wide the sidebar's card rests: open, or nothing at all.
    fn sidebar_slot(&self) -> f32 {
        if self.sidebar_hidden || self.sidebar.is_none() {
            return 0.0;
        }
        SIDEBAR_WIDTH
    }

    /// The same for the workspace panel, whose open width is the dragged one.
    /// In takeover the panel has no width of its own — it is the remainder —
    /// and the layout branch in [`regions`] takes over.
    fn workspace_slot(&self) -> f32 {
        if self.workspace_hidden || self.workspace.is_empty() {
            return 0.0;
        }
        self.workspace_open_width
    }

    /// Track the pointer while the workspace seam is dragged. A drag is
    /// already continuous, so the width follows it directly — tweening toward
    /// the pointer would only add lag to a gesture that has none.
    fn drag_workspace_seam(
        &mut self,
        event: &DragMoveEvent<WorkspaceResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = f32::from(window.viewport_size().width);
        // The pointer holds the seam, and the panel is everything right of it:
        // the regions are flush, so there is no gutter to subtract.
        let seam = f32::from(event.event.position.x);
        let width = viewport - seam - SEAM_WIDTH;
        // The cap is whatever leaves the thread column readable beside the
        // sidebar *where it is this frame*. Using the tween's destination here
        // would let a drag overlap the still-visible half of a closing panel.
        let max = workspace_max(viewport, self.sidebar_width.current());
        let min = WORKSPACE_MIN.min(max);
        self.workspace_open_width = width.clamp(min, max);
        self.workspace_width.set(self.workspace_slot());
        cx.notify();
    }
}

/// The visible tab's body, with the tab's own key context nested inside the
/// workspace's — which is what lets the track editor's whole binding block
/// survive the shell swap character-for-character.
fn active_tab(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let Some(target) = app.workspace.active().cloned() else {
        return div().into_any_element();
    };
    // The one focus handle lives at exactly one element per frame, and
    // [`Luma::focus_slot`] is the only thing that decides which — asking it
    // rather than restating its conditions is what keeps the handle from
    // being tracked twice, or nowhere.
    let holds_focus = matches!(app.focus_slot(), FocusSlot::Tab(_));
    let focus = app.focus.clone();
    // Split the borrow: the 3D view's element both mutates its state (a
    // lazily-acquired GPU, this frame's status) and reads the library
    // synchronously, and the two fields are disjoint.
    let Luma {
        workspace, library, ..
    } = app;
    let Some(body) = workspace.body_mut(&target) else {
        return div().into_any_element();
    };
    let inner = match body {
        Body::TrackEditor(state) => track_editor::track_editor(state, &entity).into_any_element(),
        Body::Graph(state) => graph::graph(state, &entity).into_any_element(),
        Body::Visualizer(state) => {
            visualizer::visualizer(state, &entity, library, window).into_any_element()
        }
        Body::Universe(state) => universe::universe(state).into_any_element(),
    };
    div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .when(holds_focus, |tab| tab.track_focus(&focus))
        .key_context(target.key_context())
        .child(inner)
        .into_any_element()
}

/// The overlay plane: a full-area surface over the regions. It reuses each
/// old screen's render function whole, so an overlay looks exactly like the
/// screen it used to be — only what is *underneath* it changed.
fn overlay_layer(
    app: &Luma,
    overlay: &Overlay,
    entity: &gpui::Entity<Luma>,
    sidebar_width: gpui::Pixels,
    window: &Window,
    _cx: &mut Context<Luma>,
) -> AnyElement {
    let (card, label) = match overlay {
        Overlay::Venues(state) => {
            let body = welcome::welcome(state, entity, window);
            (fixed_dialog(body, 920.0, 620.0), "Venue dialog")
        }
        Overlay::Patterns(state) => {
            let body = patterns::patterns(
                state,
                entity,
                &app.dialog_first_focus,
                app.dialog_first_focus.is_focused(window),
                &app.dialog_last_focus,
                app.dialog_last_focus.is_focused(window),
            );
            (fixed_dialog(body, 760.0, 600.0), "Pattern dialog")
        }
        Overlay::Settings(state) => (
            fixed_dialog(settings::settings(state, entity), 900.0, 680.0),
            "Settings dialog",
        ),
        Overlay::AddTracks(state) => (
            add_tracks::render(state, app.track_import.as_ref(), entity, window),
            "Add tracks dialog",
        ),
    };
    div()
        .absolute()
        .inset_0()
        .key_context(overlay.key_context())
        .child(luma_ui::dialog::host(
            format!("{}-host", overlay.key_context()),
            window.viewport_size(),
            sidebar_width,
            &app.dialog_focus,
            app.dialog_focus.contains_focused(window, _cx),
            label,
            card,
        ))
        .into_any_element()
}

fn fixed_dialog(body: impl IntoElement, width: f32, height: f32) -> AnyElement {
    div()
        .w(px(width))
        .h(px(height))
        .max_w_full()
        .max_h_full()
        .overflow_hidden()
        .bg(glass::overlay())
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_budget_preserves_the_centre_before_the_panel_minimum() {
        assert_eq!(workspace_max(1200.0, 0.0), 839.0);
        assert_eq!(workspace_max(1200.0, SIDEBAR_WIDTH), 582.0);
        assert_eq!(workspace_max(600.0, 0.0), 239.0);
        assert_eq!(workspace_max(300.0, 0.0), 0.0);
    }

    #[test]
    fn workspace_strip_ownership_changes_only_at_a_complete_add_control() {
        let (below, below_settings) = workspace_band_tab_strip(47.0, false);
        let (boundary, boundary_settings) = workspace_band_tab_strip(48.0, false);
        assert_eq!(below, 23.0);
        assert!(!below_settings);
        assert!(below < TAB_CONTROL_WIDTH);
        assert_eq!(boundary, TAB_CONTROL_WIDTH);
        assert!(!boundary_settings);
    }
}
