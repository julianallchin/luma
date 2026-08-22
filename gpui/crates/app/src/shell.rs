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
use gpui::{div, px, AnyElement, Context, Div, DragMoveEvent, Pixels, SharedString, Window};

use luma_lib::models::venues::Venue;
use luma_ui::node::{Instrument, Role};
use luma_ui::pane;
use luma_ui::{glass, ladder};

use crate::tabs::Target;
use crate::{graph, keymap, patterns, settings, track_editor, tracks, visualizer, welcome, Luma};

/// How wide the sidebar and the shared workspace open. Comet's defaults; the
/// workspace's is the width it *starts* at, since its seam can be dragged.
pub(crate) const SIDEBAR_WIDTH: f32 = 256.0;
pub(crate) const WORKSPACE_WIDTH: f32 = 520.0;
/// How far the workspace panel's seam can be dragged. The upper bound is also
/// capped at a fraction of the window, so a narrow window cannot be dragged
/// into having no thread column left.
const WORKSPACE_MIN: f32 = 320.0;
const WORKSPACE_MAX: f32 = 900.0;
const WORKSPACE_MAX_FRACTION: f32 = 0.62;
/// The content cards' corner radius and their inset from the glass plane —
/// comet's `PANEL_RADIUS` and pane gap.
pub(crate) const CARD_RADIUS: f32 = 10.0;
pub(crate) const CARD_GAP: f32 = 8.0;

/// One plane over the whole shell. The regions persist beneath it — closing
/// an overlay reveals them exactly as they were.
pub(crate) enum Overlay {
    /// The venue picker: the old welcome grid, re-homed. Auto-opens while no
    /// venue is selected, because a shell with no subject list has nothing
    /// else to offer.
    Venues {
        venues: Vec<Venue>,
        error: Option<String>,
    },
    /// The pattern picker. Picking a row opens a [`Target::Graph`] tab.
    Patterns(patterns::Patterns),
    Settings(settings::Settings),
}

impl Overlay {
    /// The key context the overlay's root declares — nested inside
    /// [`keymap::context::ROOT`], beside (not inside) the regions.
    fn key_context(&self) -> &'static str {
        match self {
            Self::Venues { .. } => keymap::context::VENUES,
            Self::Patterns(_) => keymap::context::PATTERNS,
            Self::Settings(_) => keymap::context::SETTINGS,
        }
    }
}

/// A workspace tab's state: the editor behind one [`Target`].
///
/// The variants mirror `Target`'s — a target is *what* the tab shows, a body
/// is the state showing it — minus `Universe`, which has no opening gesture
/// until the `+` menu lands and would otherwise be a plate nothing can reach.
pub(crate) enum Body {
    TrackEditor(Box<track_editor::Editor>),
    Graph(Box<graph::Editor>),
    Visualizer(Box<visualizer::Visualizer>),
}

impl Body {
    /// What the tab's chip and the window title call this tab.
    pub(crate) fn title(&self) -> SharedString {
        match self {
            Self::TrackEditor(state) => state.track_name().to_string().into(),
            Self::Graph(state) => state.pattern_name().to_string().into(),
            Self::Visualizer(state) => state.venue_name().to_string().into(),
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
        self.focused_slot = slot;
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
        if let Some(body) = self.workspace.close(&target) {
            self.teardown(body, cx);
        }
        if self.workspace.is_empty() {
            // Closing the last tab destroys what the panel was showing, so
            // there is nothing left to slide shut — an empty card easing out
            // would be the animation drawing attention to its own machinery.
            self.workspace_width.set(0.0);
        }
        cx.notify();
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
            Body::Graph(_) | Body::Visualizer(_) => {}
        }
    }

    /// Escape: the insertion menu first (it is the thing the eye is on), then
    /// the overlay. The venue picker with nothing selected stays — see the
    /// module docs.
    pub(crate) fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        if self.dismiss_insert_menu() {
            cx.notify();
            return;
        }
        match &self.overlay {
            Some(Overlay::Venues { .. }) if self.sidebar.is_none() => {}
            Some(_) => {
                self.overlay = None;
                cx.notify();
            }
            None => {}
        }
    }

    /// Show the venue picker and re-read the venue list.
    pub(crate) fn show_venues(&mut self, cx: &mut Context<Self>) {
        self.overlay = Some(Overlay::Venues {
            venues: Vec::new(),
            error: None,
        });
        cx.notify();
        let pending = self.library.venues();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(Overlay::Venues { venues, error }) = &mut this.overlay {
                    match result {
                        Ok(loaded) => *venues = loaded,
                        Err(failed) => *error = Some(failed.to_string()),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The loaded venue a picker card names.
    pub(crate) fn find_venue(&self, id: &str) -> Option<Venue> {
        let Some(Overlay::Venues { venues, .. }) = &self.overlay else {
            return None;
        };
        venues.iter().find(|venue| venue.id == id).cloned()
    }
}

// -- rendering ----------------------------------------------------------------

/// The row of regions under the titlebar, and the overlay over them.
///
/// One glass plane frames everything: the sidebar sits transparent on the
/// frost, and the centre and the workspace are **inset rounded cards** over
/// it. Depth is the plane showing between cards — not borders, and not value
/// steps butted edge to edge.
pub(crate) fn regions(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> Div {
    let entity = cx.entity();
    // Geometry follows state. Each edge region's resting width is restated
    // here every frame as a pure function of what the shell is showing, so a
    // toggle only flips a flag and `retarget` — a no-op while the destination
    // is unchanged — turns that into a slide.
    app.sidebar_width.retarget(app.sidebar_slot(), cx);
    app.workspace_width.retarget(app.workspace_slot(), cx);
    let sidebar_w = app.sidebar_width.eval(window);
    let workspace_w = app.workspace_width.eval(window);

    // No `gap` between the children: an edge region carries the gutter to its
    // neighbour itself (see [`region`]), because a gap laid out by the row
    // would be there in full on the first frame of a slide and shove the
    // centre 8px sideways before the panel had moved at all.
    let mut row = div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_row()
        .relative()
        .px(px(CARD_GAP))
        .pb(px(CARD_GAP))
        .on_drag_move(cx.listener(Luma::drag_workspace_seam));

    if let Some(browser) = &app.sidebar {
        if sidebar_w > px(0.0) {
            let body = div()
                .h_full()
                .flex()
                .flex_col()
                .key_context(keymap::context::SIDEBAR)
                .child(tracks::sidebar(browser, &entity, window))
                .into_any_element();
            row = row.child(region(sidebar_w, SIDEBAR_WIDTH, Gutter::Right, body));
        }
    }

    // Takeover: an open workspace covers everything right of the sidebar and
    // the thread column collapses behind it. The default is comet's split;
    // `ToggleExpand` trades the thread's room for the tab's.
    let takeover = app.expanded && !app.workspace_hidden && !app.workspace.is_empty();
    if !takeover {
        row = row.child(
            card()
                .bg(glass::panel())
                .flex_1()
                .min_w_0()
                .key_context(keymap::context::THREAD)
                .children(app.chat.clone()),
        );
    }

    if !app.workspace.is_empty() && (takeover || workspace_w > px(0.0)) {
        // Opaque, and the ladder's own ground: a tab holds an instrument
        // surface, and a timeline read through a blurred desktop is a
        // timeline you cannot read.
        let tab = card()
            .bg(ladder::background())
            .flex_1()
            .min_w_0()
            .key_context(keymap::context::WORKSPACE)
            .child(active_tab(app, window, cx))
            .into_any_element();
        row = row.child(if takeover {
            // The remainder of the row, with no gutter of its own: the sidebar
            // already carries the one between them, and the row's padding is
            // what stands between the card and the window edge.
            div().flex_1().min_w_0().h_full().child(tab)
        } else {
            // The seam floats in the gutter the region already leaves, at zero
            // layout width — a strip that took room of its own would widen the
            // gutter into something that reads as a border.
            region(workspace_w, app.workspace_open_width, Gutter::Left, tab).child(
                workspace_seam(cx)
                    .absolute()
                    .top_0()
                    .left(px((CARD_GAP - pane::HANDLE_WIDTH) / 2.0))
                    // A slider is the closest thing in the closed role
                    // vocabulary to a grip whose position along an axis is the
                    // value — which is what the seam is.
                    .agent_node(Role::Slider, "Workspace width"),
            )
        });
    }

    if let Some(overlay) = &app.overlay {
        row = row.child(overlay_layer(app, overlay, &entity, cx));
    }
    row
}

/// Which side of a region the gutter to the neighbouring card is on.
enum Gutter {
    Left,
    Right,
}

/// One edge region at its live width: a card clipped to `width`, laid out at
/// `content`, and the gutter between it and the card beside it.
///
/// Two things this shape buys, both of them only visible mid-slide. The card
/// is laid out at its **final** width the whole way, so a panel sliding in
/// reveals its text rather than re-wrapping it forty times. And the gutter is
/// the region's own child rather than the row's `gap`, so it is never in full
/// before the panel has moved — it closes only over the last few pixels, which
/// on this curve is the slowest part of the slide, so the two cards never
/// touch on the way past each other.
///
/// The returned element is positioned, so a caller may hang an absolute child
/// (the seam) in its gutter.
fn region(width: Pixels, content: f32, gutter: Gutter, inner: AnyElement) -> Div {
    let clipped = pane::pane(width, px(content), inner);
    let gap = div().h_full().flex_none().w(width.min(px(CARD_GAP)));
    let region = div().h_full().flex_none().flex().flex_row().relative();
    match gutter {
        Gutter::Left => region.child(gap).child(clipped),
        Gutter::Right => region.child(clipped).child(gap),
    }
}

/// The workspace panel's draggable left edge. Double-click restores the
/// default width — the same gesture comet's seams answer to.
fn workspace_seam(cx: &mut Context<Luma>) -> gpui::Stateful<Div> {
    pane::resize_handle(
        "workspace-seam",
        || WorkspaceResize,
        |app: &mut Luma, _| app.workspace_open_width = WORKSPACE_WIDTH,
        glass::glass_hover(),
        cx,
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
        // The pointer holds the seam's centre, which sits in the middle of the
        // gutter: the card's own left edge is half a gutter to its right, and
        // the row's padding is what remains to the window edge.
        let seam = f32::from(event.event.position.x);
        let width = viewport - seam - CARD_GAP - CARD_GAP / 2.0;
        let max = WORKSPACE_MAX.min(viewport * WORKSPACE_MAX_FRACTION);
        self.workspace_open_width = width.clamp(WORKSPACE_MIN, max.max(WORKSPACE_MIN));
        self.workspace_width.set(self.workspace_slot());
        cx.notify();
    }
}

/// One content card: a rounded plane inset from the glass ground, clipping
/// whatever lives inside it to the radius so no content touches the window
/// edge.
///
/// **No fill.** Which ground a card takes is which *tier* its contents belong
/// to (spec §9), and only the caller knows that: the thread column is chrome
/// and paints [`glass::panel`], a workspace tab is an instrument and paints
/// [`ladder::background`] opaque. A default here would be the wrong one half
/// the time and invisible when it was.
fn card() -> Div {
    div()
        .h_full()
        .rounded(px(CARD_RADIUS))
        .overflow_hidden()
        .flex()
        .flex_col()
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
    _cx: &mut Context<Luma>,
) -> Div {
    let body = match overlay {
        Overlay::Venues { venues, error } => {
            let opened = entity.clone();
            let pattern_picker = entity.clone();
            welcome::welcome(
                venues,
                error.as_deref(),
                move |id, _, cx| {
                    let id = id.to_string();
                    opened.update(cx, |this, cx| {
                        if let Some(venue) = this.find_venue(&id) {
                            this.open_venue(venue, cx);
                        }
                    });
                },
                move |_, cx| pattern_picker.update(cx, |this, cx| this.show_patterns(cx)),
            )
            .into_any_element()
        }
        Overlay::Patterns(state) => patterns::patterns(state, entity).into_any_element(),
        Overlay::Settings(state) => settings::settings(state, entity).into_any_element(),
    };
    div()
        .absolute()
        .inset_0()
        // The plane the overlay's body sits on, not the body's own ground:
        // every screen re-homed here fills it with an opaque ladder surface of
        // its own. A scrim rather than a ladder tone so the day one of them is
        // inset as a card, what shows around it is the shell, dimmed.
        .bg(glass::scrim(glass::SCRIM_ALPHA))
        .key_context(overlay.key_context())
        .track_focus(&app.focus)
        .child(body)
}
