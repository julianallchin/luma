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
use gpui::{div, px, AnyElement, Context, Div, SharedString, Window};

use luma_lib::models::venues::Venue;
use luma_ui::ladder;

use crate::tabs::Target;
use crate::{graph, keymap, patterns, settings, track_editor, tracks, visualizer, welcome, Luma};

/// How wide the sidebar and the shared workspace open. Comet's defaults; the
/// drag-resize and persisted widths land with the polish phase.
pub(crate) const SIDEBAR_WIDTH: f32 = 256.0;
pub(crate) const WORKSPACE_WIDTH: f32 = 520.0;
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
    fn focus_slot(&self) -> FocusSlot {
        if let Some(overlay) = &self.overlay {
            return FocusSlot::Overlay(overlay.key_context());
        }
        if let Some(target) = self.workspace.active() {
            return FocusSlot::Tab(target.clone());
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
    let mut row = div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_row()
        .relative()
        .gap(px(CARD_GAP))
        .px(px(CARD_GAP))
        .pb(px(CARD_GAP));

    if !app.sidebar_hidden {
        if let Some(browser) = &app.sidebar {
            row = row.child(
                div()
                    .w(px(SIDEBAR_WIDTH))
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .key_context(keymap::context::SIDEBAR)
                    .child(tracks::sidebar(browser, &entity, window)),
            );
        }
    }

    // Takeover: an open workspace covers everything right of the sidebar and
    // the thread column collapses behind it. The default is comet's split;
    // `ToggleExpand` trades the thread's room for the tab's.
    let takeover = app.expanded && !app.workspace_hidden && !app.workspace.is_empty();
    if !takeover {
        row = row.child(
            card()
                .flex_1()
                .min_w_0()
                .key_context(keymap::context::THREAD)
                .children(app.chat.clone()),
        );
    }

    if !app.workspace_hidden && !app.workspace.is_empty() {
        let pane = if takeover {
            card().flex_1().min_w_0()
        } else {
            card().w(px(WORKSPACE_WIDTH)).flex_none()
        };
        row = row.child(
            pane.key_context(keymap::context::WORKSPACE)
                .child(active_tab(app, window, cx)),
        );
    }

    if let Some(overlay) = &app.overlay {
        row = row.child(overlay_layer(app, overlay, &entity, cx));
    }
    row
}

/// One content card: a rounded plane inset from the glass ground. The ladder
/// (or the thread) lives *inside* it, clipped by the radius, so instrument
/// surfaces never touch the window edge.
fn card() -> Div {
    div()
        .h_full()
        .rounded(px(CARD_RADIUS))
        .overflow_hidden()
        .flex()
        .flex_col()
        .bg(luma_ui::glass::grey(6))
}

/// The visible tab's body, with the tab's own key context nested inside the
/// workspace's — which is what lets the track editor's whole binding block
/// survive the shell swap character-for-character.
fn active_tab(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let Some(target) = app.workspace.active().cloned() else {
        return div().into_any_element();
    };
    // The one focus handle lives at exactly one element per frame — here only
    // while no overlay out-ranks the tab. See `Luma::focus_slot`.
    let holds_focus = app.overlay.is_none();
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
        .bg(ladder::background())
        .key_context(overlay.key_context())
        .track_focus(&app.focus)
        .child(body)
}
