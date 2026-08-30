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
use gpui::{div, px, AnyElement, App, Context, Div, DragMoveEvent, SharedString, Window};

use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::pane;
use luma_ui::{glass, ladder};

use crate::tabs::Target;
use crate::{
    add_tracks, chat_history, chrome, confirm, fixture_picker, graph, keymap, patterns, settings,
    stage, subagents, tab_chrome, track_editor, tracks, universe, visualizer, welcome, Luma,
};

/// How wide the sidebar opens. Comet's default.
pub(crate) const SIDEBAR_WIDTH: f32 = 256.0;
/// The two regions' floors — the narrowest each is still worth showing at.
///
/// They bound the seam's drag (there is no absolute cap on the panel: it may
/// take everything except a still-usable thread column, so the bound is stated
/// as what must *remain*), and together they decide whether the window can
/// carry a split at all — see the takeover rule in [`regions`].
const WORKSPACE_MIN: f32 = 320.0;
const CENTER_MIN: f32 = 360.0;
/// How wide a seam between two regions is. One device pixel's worth of rule at
/// 1×, and the only structural line the shell draws.
const SEAM_WIDTH: f32 = 1.0;
/// The empty panel's three buttons: one width for all of them so the stack
/// reads as a column rather than three sizes, wide enough for the longest
/// label ("Universe setup") at 13px without wrapping.
const EMPTY_PANEL_BUTTON_WIDTH: f32 = 168.0;
/// Between those buttons, and between one and the reason it cannot act.
const EMPTY_PANEL_GAP: f32 = 8.0;
const EMPTY_PANEL_REASON_GAP: f32 = 3.0;

/// One plane over the whole shell. The regions persist beneath it — closing
/// an overlay reveals them exactly as they were.
pub(crate) enum Overlay {
    /// The venue picker: the old welcome grid, re-homed. Auto-opens while no
    /// venue is selected, because a shell with no subject list has nothing
    /// else to offer.
    /// Boxed for the same reason [`Self::AddTracks`] is: a picker carries its
    /// morph, two text fields and their subscriptions, and an enum is as large
    /// as its largest variant — every overlay slot in the app would pay for it.
    Venues(Box<welcome::VenuePicker>),
    /// The pattern picker. Picking a row opens a [`Target::Graph`] tab.
    Patterns(patterns::Patterns),
    Settings(settings::Settings),
    AddTracks(Box<add_tracks::AddTracks>),
    /// Every conversation in the room. Boxed for the same reason the others
    /// are: it carries a text field, a scroll handle and a subscription, and an
    /// enum is as large as its largest variant.
    ChatHistory(Box<chat_history::ChatHistory>),
    /// What the thread delegated, and one child's transcript. Boxed like the
    /// rest: it carries a morph, a row of focus handles and a whole chat panel.
    Subagents(Box<subagents::Subagents>),
    /// Choosing a clip's fixtures by pointing at the room. Boxed like the
    /// rest: it carries the venue's group list, a parked render sequence and
    /// the frame on screen.
    FixturePicker(Box<fixture_picker::FixturePicker>),
    /// "Are you sure": one question, two answers, one closed list of acts —
    /// see [`crate::confirm`]. Unboxed because it is a few strings and an
    /// enum, and it is the *smallest* variant here rather than the largest.
    Confirm(confirm::Confirm),
}

impl Overlay {
    /// The key context the overlay's root declares — nested inside
    /// [`keymap::context::ROOT`], beside (not inside) the regions.
    fn key_context(&self) -> &'static str {
        match self {
            Self::ChatHistory(_) => keymap::context::CHAT_HISTORY,
            Self::Venues { .. } => keymap::context::VENUES,
            Self::Patterns(_) => keymap::context::PATTERNS,
            Self::Settings(_) => keymap::context::SETTINGS,
            Self::AddTracks(_) => keymap::context::ADD_TRACKS,
            Self::Subagents(_) => keymap::context::SUBAGENTS,
            Self::FixturePicker(_) => keymap::context::FIXTURE_PICKER,
            Self::Confirm(_) => keymap::context::CONFIRM,
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
    Universe(Box<universe::Universe>),
    Stage(Box<stage::StagePage>),
}

impl Body {
    /// What the tab's chip and the window title call this tab.
    pub(crate) fn title(&self) -> SharedString {
        match self {
            Self::TrackEditor(state) => state.track_name().to_string().into(),
            Self::Graph(state) => state.pattern_name().to_string().into(),
            Self::Universe(state) => state.venue_name().to_string().into(),
            Self::Stage(state) => state.venue_name.clone().into(),
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
        // `as_open`: a dialog that is leaving has already given the keyboard
        // back, so focus returns to the opener on the click rather than after
        // the animation.
        if let Some(overlay) = self.overlay.as_open() {
            return FocusSlot::Overlay(overlay.key_context());
        }
        // A panel that is not on screen still *has* an active tab: leaving the
        // keyboard with it would track the one focus handle at an element no
        // frame renders, and every action dispatched from there — including the
        // one that would bring the panel back — dead-ends silently.
        //
        // Being put away is now the panel's only way off screen: a window too
        // narrow to seat it beside the thread gives it the room instead of
        // dropping it (see [`regions`]). While that was not so, a 420px window
        // held a live tab, an unrendered focus handle and a dead ⌘P.
        if let Some(target) = self.workspace.active() {
            if !self.workspace_hidden {
                return FocusSlot::Tab(target.clone());
            }
        }
        FocusSlot::Shell
    }

    /// Take the keyboard when the slot changed hands or nothing on screen
    /// holds it.
    pub(crate) fn take_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let slot = self.focus_slot();
        if slot == self.focused_slot && self.keyboard_is_seated(window, cx) {
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

    /// Whether the keyboard is somewhere the *last painted frame* can dispatch
    /// from — the question [`Self::take_focus`] has to answer before it decides
    /// nothing is owed.
    ///
    /// `window.focused()` is not that question. It reports the focus *id*, which
    /// outlives the element that carried it: a handle whose element stopped
    /// being rendered — a dialog stepping between morph routes unmounts the row
    /// the keyboard was on — still answers `is_focused`, while gpui resolves
    /// both key bindings and `on_key_down` against the rendered dispatch tree
    /// and finds no path for it. The window then looks focused and is deaf:
    /// every keystroke falls back to the tree's root, which carries no key
    /// context, so not even Escape fires.
    ///
    /// A modal is the strict case and gets the strict test — the keyboard
    /// belongs *inside the card*, since that is the whole claim of a focus
    /// trap. Everywhere else the shell is deliberately permissive: a click into
    /// the sidebar while a tab is up is focus the user moved on purpose, and
    /// asking whether the tab still contains it would snatch it back.
    fn keyboard_is_seated(&self, window: &Window, cx: &App) -> bool {
        if matches!(self.focused_slot, FocusSlot::Overlay(_)) {
            return self.dialog_focus.contains_focused(window, cx);
        }
        window.focused(cx).is_some()
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
            Body::Graph(_) | Body::Universe(_) | Body::Stage(_) => {}
        }
    }

    /// Begin the dialog's exit and schedule its reap.
    ///
    /// The state is not dropped here: it stays mounted, inert, for as long as
    /// the out-animation runs — see [`luma_ui::dialog::Popup`]. With motion off
    /// it goes on the spot, so a driver that dismisses and asserts absence in
    /// the next frame is not racing a timer.
    pub(crate) fn close_overlay(&mut self, cx: &mut Context<Self>) {
        // The exit is reaped from the render frame (see `shell`), not from a
        // timer — so all this owes is the first repaint.
        self.overlay.begin_close(cx);
        cx.notify();
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
        if self.account_menu.is_open() {
            self.close_account_menu(cx);
            return;
        }
        // The sidebar's score menu is on the same rung as the other two: a
        // float over the shell, closed before anything under it is.
        if self.close_score_menu(cx) {
            return;
        }
        // Innermost first: a dialog showing a child's transcript steps back to
        // its list before the list itself closes.
        if self.subagents_to_list(cx) {
            return;
        }
        match self.overlay.as_open() {
            Some(Overlay::Venues(_)) if self.sidebar.is_none() => {}
            Some(_) => self.close_overlay(cx),
            // Nothing floating: keep stepping back out, innermost first — a
            // menu inside the args sheet, then the selection the sheet *is*,
            // then the sidebar's second level. All routed through the one
            // dismissal ladder rather than given `escape` bindings of their
            // own, which would have to out-scope the shell's and would then be
            // the only Escapes in the app that do not mean "close what is over
            // me". The sheet's rungs sit here, below the overlay arm, so a
            // dialog open over the timeline still answers Escape first.
            None => {
                if self.dismiss_sheet_menu() {
                    cx.notify();
                    return;
                }
                if self.clear_clip_selection(cx) {
                    return;
                }
                self.leave_scores(cx);
            }
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
    // The sidebar's level change is stepped in the same place and for the same
    // reason as its width: a hand-driven tween has nothing else asking for the
    // next frame, and both have to be settled before the column is rendered
    // out of an immutable borrow of the shell.
    app.tick_sidebar_push(window);
    let sidebar_w = app.sidebar_width.eval(window);
    let viewport = f32::from(window.viewport_size().width);
    // What the thread and the panel share, and how they divide it. Stored as a
    // proportion (see [`luma_ui::split`]), so the sidebar taking 256px from the
    // pair takes it from *both* in the ratio they were already at — the split
    // is unchanged by a ⌘B, which is the whole point of storing it this way. A
    // stored panel width would instead have spent the entire delta on the
    // thread, because the thread is the flexible one.
    let room = shared_room(viewport, f32::from(sidebar_w));
    let workspace_open_w = app.workspace_split.resolve(room).1;
    // The panel's width is *derived* while it is open and *tweened* when it is
    // toggled, and those are different motions. A derived width has to track
    // the sidebar exactly — the sidebar's own tween is the animation, and a
    // second tween chasing a target that moves every frame would trail it and
    // then catch up in a lurch. So a settled pane is set, and only a pane
    // mid-toggle is retargeted.
    let workspace_slot = if app.workspace_hidden {
        0.0
    } else {
        workspace_open_w
    };
    if app.workspace_width.settled() {
        app.workspace_width.set(workspace_slot);
    } else {
        app.workspace_width.retarget(workspace_slot, cx);
    }
    let workspace_w = app.workspace_width.eval(window);

    // Takeover: an open workspace covers everything right of the sidebar and
    // the thread column collapses behind it. The default is comet's split;
    // `ToggleExpand` trades the thread's room for the tab's.
    //
    // A window with no legal split reaches the same layout without anyone
    // asking for it. There are two floors — [`CENTER_MIN`] and
    // [`WORKSPACE_MIN`] — and a window that cannot honour both has to drop one
    // region rather than shave both: the panel carries the tab strip, so a
    // panel ground down to a sliver takes every tab and the `+` with it, and
    // the thread cannot host them instead without the strip having two homes
    // again. The panel takes the room, through the branch takeover already is.
    let squeezed = room < CENTER_MIN + WORKSPACE_MIN;
    let takeover = !app.workspace_hidden && (app.expanded || squeezed);
    let show_sidebar = app.sidebar.is_some() && sidebar_w > px(0.0);
    let show_thread = !takeover;
    // The panel is up exactly when it has not been put away. Emptiness used to
    // be a second, silent reason to hide it — which made "no tabs" a state with
    // no way out: the panel that offers the first tab was the thing withheld
    // until a tab existed. It now opens onto [`empty_panel`].
    let show_workspace = takeover || workspace_w > px(0.0);
    let workspace_panel_width = if takeover {
        viewport - f32::from(sidebar_w) - if show_sidebar { SEAM_WIDTH } else { 0.0 }
    } else {
        f32::from(workspace_w)
    };
    // The strip is the band's only child, so it gets everything the band's
    // anchors leave. It used to share the band with the settings gear and
    // yield to it; the account moved to the sidebar's foot, and with it the
    // one reason a narrow window had to choose between reaching a tab and
    // reaching an account.
    let workspace_strip_width = chrome::band_room(
        chrome::BandSpan {
            x: viewport - workspace_panel_width,
            width: workspace_panel_width,
            viewport,
        },
        0.0,
        0.0,
        0,
    );
    // The `+` menu hangs off the strip, and the strip is the panel's: put the
    // panel away or empty it and the menu has nothing to hang off, so it goes
    // too rather than waiting armed for whatever brings the strip back.
    //
    // Asked of the panel's *state*, not of `show_workspace`: that is this
    // frame's animated width, which says "not yet" at the start of an entrance
    // the state has already committed to. ⌘T opens the panel and its menu in
    // one action, and nothing about that pair should turn on where a tween
    // happens to be sampled.
    if app.workspace_hidden || app.workspace.is_empty() {
        app.tab_chrome.dismiss_menu();
    }
    // Same rule for the account menu: it hangs off the sidebar's foot, so a
    // sidebar that is away has nothing for it to hang from. Dropped rather
    // than closed — an exit needs a surface to play over.
    if app.sidebar.is_none() || app.sidebar_hidden {
        app.account_menu = luma_ui::dialog::Popup::default();
    }

    // The one surface the desktop shows through. Everything above paints on
    // top of this single translucent fill — see `luma_ui::glass::tone_column`
    // for why the sidebar must not paint a plane of its own.
    let mut row = div()
        .size_full()
        .flex()
        .flex_row()
        .relative()
        .bg(glass::glass())
        .on_drag_move(cx.listener(Luma::drag_workspace_seam))
        .on_drag_move(cx.listener(Luma::drag_visualizer_seam));

    if let Some(browser) = &app.sidebar {
        if show_sidebar {
            // The sidebar's content is laid out at its full width for the whole
            // slide (see `pane::pane`), so its band is spanned that way too —
            // reserving against the clipped width would re-pad it every frame.
            let span = chrome::BandSpan {
                x: 0.0,
                width: SIDEBAR_WIDTH,
                viewport,
            };
            let body = column(chrome::band(span))
                // The one region that raises itself off the ground, because it
                // is the frame and not the subject. On the blurred window that
                // lift is a WASH over the root fill, not a plane: an opaque
                // rung would be a slab sitting on the blur, and a translucent
                // one would land on the root's own tone and vanish.
                //
                // No edge of its own: the rule on its trailing side is the
                // `seam` below, which every region boundary here is drawn by.
                // A border here would have been a second line beside that one,
                // inside the clipping pane rather than between the regions.
                .bg(glass::tone_column())
                .key_context(keymap::context::SIDEBAR)
                .child(tracks::sidebar(app, browser, &entity, window))
                .into_any_element();
            // Laid out at its full width for the whole slide, so a sidebar
            // easing open reveals its rows rather than re-wrapping them.
            row = row.child(
                pane::pane(sidebar_w, px(SIDEBAR_WIDTH), body)
                    // The region itself, named. Its content is addressable but
                    // its *edge* was not, and the edge is what the regions
                    // beside it are measured from.
                    .agent_node(Role::Card, "Sidebar"),
            );
            // The sidebar and whatever is beside it are different planes, so
            // the line between them is the bright one.
            row = row.child(seam(ladder::seam_plane()));
        }
    }

    if show_thread {
        // Where the thread sits this frame: it begins at the sidebar's live
        // edge and ends at the workspace's, so its band tracks both panels
        // without restating either one's curve.
        let span = chrome::BandSpan {
            x: f32::from(sidebar_w) + if show_sidebar { SEAM_WIDTH } else { 0.0 },
            width: viewport
                - f32::from(sidebar_w)
                - if show_sidebar { SEAM_WIDTH } else { 0.0 }
                - if show_workspace {
                    f32::from(workspace_w) + SEAM_WIDTH
                } else {
                    0.0
                },
            viewport,
        };
        // Back/forward, then empty band. The thread carries no tabs — they are
        // the panel's — and nothing else: the account, which is what used to
        // end this band, lives at the foot of the sidebar now.
        let head = chrome::band(span)
            .child(chrome::history_pair())
            .child(div().flex_1());
        row = row.child(
            column(head)
                .flex_1()
                // The ground, not a card on it: the thread is what the app is
                // about, and the content plane is the darkest one there is.
                //
                // **Opaque**, like the workspace ground below — a structural
                // plane has no coverage (see `glass`'s module docs). It used to
                // spend part of its alpha on the blur behind the window, and
                // that is what made the transcript's fade bands unpaintable: a
                // plane composited over an unknown backdrop has no colour any
                // overlay can match, so the band read as a dark strip however
                // it was tinted. Opaque, `panel_opaque()` *is* the plane, and
                // the band that fades to it disappears into it by construction.
                .bg(glass::panel_opaque())
                .key_context(keymap::context::THREAD)
                .children(app.chat.clone()),
        );
    }

    if show_workspace {
        // The panel is the last region in the row, so it always ends at the
        // window's trailing edge — takeover only makes it start further left.
        let span = chrome::BandSpan {
            x: viewport - workspace_panel_width,
            width: workspace_panel_width,
            viewport,
        };
        let head = chrome::band(span).child(chrome::tab_strip(
            app,
            &entity,
            workspace_strip_width,
            chrome::tab_strip_origin(span),
            window,
            cx,
        ));
        if show_thread {
            // Both sides are lit surfaces whose own value step already divides
            // them, so this rule is a hint. The grip that pulls it is mounted
            // after the panel, not here — see [`workspace_grip`].
            row = row.child(seam(ladder::seam_hint()));
        }
        // The same ground the thread column is — which is why the rule between
        // them is [`ladder::seam_hint`] and not the bright one. Opaque, where
        // the thread column is not: a tab holds an *instrument* surface, and a
        // waveform read through a blurred desktop is a waveform you cannot
        // read. The two grounds are the same rung; only their coverage differs,
        // and it differs because of what each one carries.
        let panel = column(head)
            .bg(ladder::background())
            .key_context(keymap::context::WORKSPACE)
            .child(workspace_body(app, window, cx))
            .into_any_element();
        row = row.child(if takeover {
            div().h_full().flex_1().min_w_0().child(panel)
        } else {
            pane::pane(workspace_w, px(workspace_open_w), panel)
        });
        if show_thread {
            // After both panes it divides, so the strip it overhangs is its
            // own — and before the layers below, which must keep the pointer
            // they cover. The rule is a hair to the panel's left, and the
            // panel ends at the window, so that is where the grip centres.
            row = row.child(workspace_grip(
                viewport - f32::from(workspace_w) - SEAM_WIDTH / 2.0,
                cx,
            ));
        }
    }

    // Window-space tab exits and their stable close target sit above both pane
    // rails, but below popovers and modal overlays.
    row = row.child(chrome::tab_transition_layer(app, &entity, window, cx));

    // The window's two fixed corners. Above every region — a toggle inside one
    // rides that region's animated pane and gets clipped by it — but *below*
    // the overlay, unlike the traffic lights: a panel toggle reachable through
    // a scrim would move the regions behind a modal that is meant to own the
    // window. See [`crate::chrome`] for the anchor rule.
    row = row
        .child(chrome::sidebar_toggle(
            &entity,
            show_sidebar,
            app.sidebar.is_some(),
        ))
        // Always live: opening the panel with no tabs is how [`empty_panel`]
        // is reached, so "there is nothing to show" is the one state this
        // control most needs to answer.
        .child(chrome::panel_toggle(&entity, show_workspace, true));

    // Reap a dismissed dialog once its out-animation has played, and keep
    // frames coming until it has. Frame-driven rather than timer-driven —
    // see `Popup::tick_close`.
    if app.overlay.tick_close() {
        window.request_animation_frame();
    }
    if app.account_menu.tick_close() {
        window.request_animation_frame();
    }
    // `open_mut`, not `get_mut`: every tick here takes the keyboard, and a
    // dialog playing its exit has already handed the keyboard back to whatever
    // opened it (see [`Luma::focus_slot`]). Ticking a leaving dialog re-focused
    // a field on a card that was about to unmount, and the focus went nowhere
    // when it did. The state stays mounted for the exit so it can be *painted*
    // — which is what `get` below is for.
    let dialog_focus = app.dialog_focus.clone();
    if let Some(Overlay::Venues(state)) = app.overlay.open_mut() {
        welcome::tick(state, &dialog_focus, window, cx);
    }
    if let Some(Overlay::AddTracks(state)) = app.overlay.open_mut() {
        add_tracks::tick(state, &dialog_focus, window, cx);
    }
    if let Some(Overlay::ChatHistory(state)) = app.overlay.open_mut() {
        chat_history::tick(state, window, cx);
    }
    if matches!(app.overlay.as_open(), Some(Overlay::FixturePicker(_))) {
        fixture_picker::tick(app, window, cx);
    }
    if matches!(app.overlay.as_open(), Some(Overlay::Subagents(_))) {
        // Read out of the chat before the overlay is borrowed: both live on
        // `app`, and the dialog cannot reach back through the entity for them.
        let rows = app
            .chat
            .as_ref()
            .map(|chat| chat.read(cx).subagents().to_vec())
            .unwrap_or_default();
        if let Some(Overlay::Subagents(state)) = app.overlay.open_mut() {
            subagents::tick(state, rows, window, cx);
        }
    }
    if let Some(overlay) = app.overlay.get() {
        row = row.child(overlay_layer(app, overlay, &entity, window, cx));
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

/// How the thread and the workspace panel divide the room they share, and the
/// floors neither may be dragged below.
///
/// Even by default. Comet states its panel as 520px, which at the reference
/// window is a hair over half of what the pair shares — so the proportion says
/// the same thing the pixel count did, minus its one flaw: a width holds only
/// at the width it was measured at, and this holds at every window size and on
/// both sides of a ⌘B.
///
/// Stated here rather than in `Luma::new` so the split and the floors it
/// enforces sit together.
pub(crate) fn workspace_split() -> luma_ui::split::SplitFraction {
    luma_ui::split::SplitFraction::new(0.5, CENTER_MIN, WORKSPACE_MIN)
}

/// The room the thread and the panel share this frame: the window, less the
/// sidebar where it is *now* and the seams between the three.
///
/// The sidebar's own seam exists exactly while any of its panel is visible, so
/// it belongs in the budget too. Taken against the sidebar's live width rather
/// than its destination, which is what makes the pair follow a sidebar
/// mid-slide instead of jumping when it lands.
fn shared_room(viewport: f32, sidebar: f32) -> f32 {
    let sidebar_seam = if sidebar > 0.0 { SEAM_WIDTH } else { 0.0 };
    (viewport - sidebar - sidebar_seam - SEAM_WIDTH).max(0.0)
}

/// The grip that pulls the workspace panel's seam: a wider invisible strip
/// over the hint-toned rule at `at`, so the pointer can hit the boundary
/// without the boundary having to be thick enough to aim at. Double-click
/// restores the default split — the gesture comet's seams answer to.
///
/// A child of the row rather than of the rule, because a grip owns the strip
/// it overhangs only if it is painted after both panes — see
/// [`pane::resize_handle`].
fn workspace_grip(at: f32, cx: &mut Context<Luma>) -> impl IntoElement {
    pane::resize_handle(
        "workspace-seam",
        pane::Seam::Vertical,
        at,
        || WorkspaceResize,
        |app: &mut Luma, _| app.workspace_split.reset(),
        glass::glass_hover(),
        cx,
    )
    // A slider is the closest thing in the closed role vocabulary to a grip
    // whose position along an axis is the value — which is what the seam is.
    .agent_node(Role::Slider, "Workspace width")
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

    /// Track the pointer while the workspace seam is dragged. A drag is
    /// already continuous, so the split follows it directly — tweening toward
    /// the pointer would only add lag to a gesture that has none.
    fn drag_workspace_seam(
        &mut self,
        event: &DragMoveEvent<WorkspaceResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Against the sidebar's live width, not its destination: a drag during
        // a closing sidebar must not overlap the half of it still on screen.
        let sidebar = self.sidebar_width.current();
        let room = shared_room(f32::from(window.viewport_size().width), sidebar);
        // The pointer holds the seam, and what it is setting is the *thread's*
        // share — the regions are flush, so the offset into the shared room is
        // simply the pointer minus where that room starts.
        let origin = sidebar + if sidebar > 0.0 { SEAM_WIDTH } else { 0.0 };
        self.workspace_split
            .drag_to(f32::from(event.event.position.x) - origin, room);
        self.workspace_width
            .set(self.workspace_split.resolve(room).1);
        cx.notify();
    }
}

/// The workspace column below its band: the stage over the visible editor.
///
/// **The stage is not a tab.** It is a view of whatever the editor below it is
/// about, so it sits outside the tab's key context and outside its focus
/// tracking — switching tabs within one room leaves the same stage running.
/// [`Luma::sync_visualizer`] has already decided whether one exists; this
/// decides only whether there is room to show it, and *not mounting it here is
/// what stops its redraw loop* (see [`visualizer::visualizer`]).
///
/// # A dialog does not stand the stage down
///
/// An earlier revision unmounted the stage while an overlay was up. That is
/// wrong twice over: the dialog backdrop is a tint rather than a blur, so the
/// shell behind it is *meant* to be read — and a rig that vanishes when you
/// open "add track" reads as the app losing your work. The stage stays mounted
/// and live underneath.
///
/// What that revision was actually protecting was the modal's focus trap: at
/// the time, Shift-Tab out of a dialog escaped into the stage's toolbar. That
/// belonged to the dialog and has been fixed there, so nothing is owed here —
/// the sidebar and the editors have always had tab stops behind a modal too,
/// and a shell where every background region had to disarm itself would be the
/// trap's job done N times at the wrong layer. `shell_panels` and
/// `dialog_focus` are what hold that line.
fn workspace_body(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    // An empty panel is not a panel with nothing in it — it is the place a
    // first tab is started from. The stage stands down with it: a rig view
    // over no editor is a room with no subject.
    if app.workspace.is_empty() {
        return empty_panel(app, &cx.entity());
    }
    if app.visualizer.is_none() {
        return active_tab(app, window, cx);
    }
    // The stage tab *is* the picture: its chrome floats over the room rather
    // than sitting under it, so there is nothing to split and no seam to drag.
    // Every other tab is an editor beside a rig view, and keeps the split.
    //
    // This is the one place the difference is stated. A stage page that had to
    // be told how tall the room was would be a page that could disagree with
    // it.
    let full_bleed = matches!(app.workspace.active_body(), Some(Body::Stage(_)));
    if full_bleed {
        let Luma {
            visualizer,
            library,
            ..
        } = app;
        let room = visualizer
            .as_mut()
            .map(|state| visualizer::visualizer(state, &cx.entity(), library, window));
        return div()
            .flex_1()
            .min_h_0()
            .relative()
            .key_context(keymap::context::VISUALIZER)
            .children(room)
            // The tab renders as an overlay inside the room's own box, so the
            // builder's floats are positioned against the picture they aim at.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    // A flex box, because `active_tab` sizes itself with
                    // `flex_1` — inside a bare absolute parent that resolves to
                    // nothing and the whole page collapses to zero height.
                    .flex()
                    .flex_col()
                    .child(active_tab(app, window, cx)),
            )
            .into_any_element();
    }
    let available = f32::from(window.viewport_size().height) - chrome::HEIGHT - pane::HANDLE_WIDTH;
    let (stage_height, _) = app.visualizer_split.resolve(available);
    let grip = visualizer_grip(stage_height + SEAM_WIDTH / 2.0, cx);
    // Split the borrow the way `active_tab` does: the stage's element mutates
    // its own state and reads the library synchronously, and the two fields
    // are disjoint.
    let Luma {
        visualizer,
        library,
        ..
    } = app;
    let stage = visualizer
        .as_mut()
        .map(|state| visualizer::visualizer(state, &cx.entity(), library, window));
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        // The grip below is placed against this column's own top edge.
        .relative()
        .children(stage.map(|stage| {
            div()
                .h(px(stage_height))
                .flex_none()
                .overflow_hidden()
                .key_context(keymap::context::VISUALIZER)
                .child(stage)
        }))
        .child(visualizer_seam())
        .child(active_tab(app, window, cx))
        .child(grip)
        .into_any_element()
}

/// What the panel shows before its first tab: the three ways to open one,
/// stacked and centred.
///
/// These are [`tab_chrome::NewTabChoice`] — the same three the `+` menu
/// offers, with the same labels and the same prerequisites. The choices are
/// stated once and drawn twice: a menu when the strip has tabs to sit beside,
/// and this when it does not. A second list here would be the one that drifts.
///
/// A choice that cannot act yet keeps its slot and says why, the way the
/// chrome's own dimmed controls do — the panel's anatomy is the same whether
/// you have a track selected or not.
fn empty_panel(app: &Luma, entity: &gpui::Entity<Luma>) -> AnyElement {
    let prerequisites = app.new_tab_prerequisites();
    let mut stack = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(EMPTY_PANEL_GAP));
    for availability in tab_chrome::menu_choices(&prerequisites) {
        let choice = availability.choice;
        let enabled = availability.enabled();
        let label = choice.label();
        // One primary per card (see `float::btn_primary`): the universe is the
        // room itself, and the other two open something inside it.
        let button = if matches!(choice, tab_chrome::NewTabChoice::Universe) {
            luma_ui::float::btn_primary(label)
        } else {
            luma_ui::float::btn(label, format!("empty-panel-{label}"))
        }
        .id(SharedString::from(format!("empty-panel:{label}")))
        .w(px(EMPTY_PANEL_BUTTON_WIDTH));
        let button = if enabled {
            let opened = entity.clone();
            button.on_click(move |_, _, cx| {
                opened.update(cx, |this, cx| this.activate_new_tab_choice(choice, cx));
            })
        } else {
            button.opacity(0.35)
        };
        stack = stack.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(EMPTY_PANEL_REASON_GAP))
                // The node rides a plain wrapper: `agent_disabled` is not on
                // `Stateful`, and the button has an id because it is clickable.
                .child(
                    div()
                        .child(button)
                        .agent_node(Role::Button, label)
                        .agent_disabled(!enabled),
                )
                .children(availability.reason.map(|reason| {
                    div()
                        .text_size(px(10.0))
                        .text_color(glass::ink(0.32))
                        .child(reason)
                        .agent_node(Role::Text, reason)
                })),
        );
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .child(stack)
        .agent_node(Role::Card, "Empty panel")
        .into_any_element()
}

/// The stage's resting share of the workspace column, and the floors that keep
/// either half from being dragged away entirely.
///
/// A stage needs enough height to read as a room rather than a letterbox; an
/// editor needs enough for its transport plus a lane or two. Stated here rather
/// than in `Luma::new` so the two floors sit beside the column that enforces
/// them.
pub(crate) fn visualizer_split() -> luma_ui::split::SplitFraction {
    luma_ui::split::SplitFraction::new(0.4, 140.0, 200.0)
}

/// The rule between the stage and the editor below it: the same hint-toned
/// hairline the workspace's own seam is, turned across the other axis.
fn visualizer_seam() -> Div {
    div()
        .w_full()
        .flex_none()
        .h(px(SEAM_WIDTH))
        .bg(ladder::seam_hint())
}

/// The grip that pulls it, centred on the rule at `at`. Mounted after the
/// stage *and* the editor for the reason [`workspace_grip`] is.
fn visualizer_grip(at: f32, cx: &mut Context<Luma>) -> impl IntoElement {
    pane::resize_handle(
        "visualizer-seam",
        pane::Seam::Horizontal,
        at,
        || VisualizerResize,
        |app: &mut Luma, _| app.visualizer_split.reset(),
        glass::glass_hover(),
        cx,
    )
    .agent_node(Role::Slider, "Stage height")
}

/// The stage/editor seam, under the pointer. gpui routes a drag by the type it
/// carries, so this marker is what tells the row's listener that the pointer
/// belongs to this seam rather than to the workspace's vertical one.
struct VisualizerResize;

impl Luma {
    /// Track the pointer while the stage seam is dragged. Like the workspace
    /// seam, the height follows the pointer directly — a drag is already
    /// continuous, and tweening toward it would only add lag.
    fn drag_visualizer_seam(
        &mut self,
        event: &DragMoveEvent<VisualizerResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let available =
            f32::from(window.viewport_size().height) - chrome::HEIGHT - pane::HANDLE_WIDTH;
        // The column starts below the band, and the pointer holds the seam.
        let offset = f32::from(event.event.position.y) - chrome::HEIGHT;
        self.visualizer_split.drag_to(offset, available);
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
    // Read before the workspace is borrowed mutably: the builder lives beside
    // the picture, not in the tab, and its state is a projection either way.
    let stage_view = app.stage_view();
    // Evaluated here because the tween lives on the builder beside the picture
    // and only the shell holds it mutably — see `Build::inspector_target`.
    let sheet = app.stage_inspector_width(window, cx);
    let Some(body) = app.workspace.body_mut(&target) else {
        return div().into_any_element();
    };
    let inner = match body {
        Body::TrackEditor(state) => {
            track_editor::track_editor(state, &entity, window, cx).into_any_element()
        }
        Body::Graph(state) => graph::graph(state, &entity).into_any_element(),
        Body::Universe(state) => universe::universe(state).into_any_element(),
        Body::Stage(state) => {
            stage::stage_page(state, &entity, stage_view.as_ref(), sheet, window, cx)
                .into_any_element()
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
/// `cx` is here for the pickers' loading state: a skeleton pulses off the one
/// shared clock ([`luma_ui::motion::pulse_delta`]), which needs an `App` to
/// take its lease from. Without it each picker hand-rolled a static ramp, and
/// three still ladders is what "one pulse, phase-locked across the window"
/// was supposed to prevent.
fn overlay_layer(
    app: &Luma,
    overlay: &Overlay,
    entity: &gpui::Entity<Luma>,
    window: &Window,
    cx: &mut Context<Luma>,
) -> AnyElement {
    // Every arm hands back a finished card. There is one card mechanism —
    // `morph::card` — and a dialog that never morphs simply has one route
    // (`morph::fixed_card`); the shell no longer describes a dialog's box.
    let (card, label) = match overlay {
        Overlay::Venues(state) => (welcome::render(state, entity, window, cx), "Venue dialog"),
        Overlay::Patterns(state) => {
            let body = patterns::patterns(
                state,
                entity,
                &app.dialog_first_focus,
                app.dialog_first_focus.is_focused(window),
                &app.dialog_last_focus,
                app.dialog_last_focus.is_focused(window),
                app.graph_track_context().is_some(),
            );
            (
                morph::fixed_card(
                    "Pattern dialog",
                    MorphSize::new(760.0, 600.0),
                    body.into_any_element(),
                ),
                "Pattern dialog",
            )
        }
        Overlay::Settings(state) => (
            morph::fixed_card(
                "Settings dialog",
                MorphSize::new(900.0, 680.0),
                settings::settings(app, state, entity).into_any_element(),
            ),
            "Settings dialog",
        ),
        Overlay::AddTracks(state) => (
            add_tracks::render(state, app.track_import.as_ref(), entity, window, cx),
            "Add tracks dialog",
        ),
        Overlay::ChatHistory(state) => (
            chat_history::render(state, entity, window, cx),
            "Chat history dialog",
        ),
        Overlay::Subagents(state) => (
            subagents::render(state, entity, window, cx),
            "Subagents dialog",
        ),
        Overlay::FixturePicker(state) => (
            fixture_picker::render(state, entity, window, cx),
            "Fixture picker dialog",
        ),
        Overlay::Confirm(state) => (
            confirm::render(
                state,
                entity,
                &app.dialog_first_focus,
                app.dialog_first_focus.is_focused(window),
                &app.dialog_last_focus,
                app.dialog_last_focus.is_focused(window),
            ),
            "Confirm dialog",
        ),
    };
    let scrim_dismiss = if matches!(overlay, Overlay::Venues(_)) && app.sidebar.is_none() {
        luma_ui::dialog::ScrimDismiss::Disabled
    } else {
        let dismissed = entity.clone();
        luma_ui::dialog::ScrimDismiss::Enabled(Box::new(move |_, cx| {
            dismissed.update(cx, |this, cx| this.dismiss_overlay(cx));
        }))
    };
    div()
        .absolute()
        .inset_0()
        .key_context(overlay.key_context())
        .child(
            luma_ui::dialog::Host {
                id: format!("{}-host", overlay.key_context()).into(),
                viewport: window.viewport_size(),
                focus: &app.dialog_focus,
                focused: app.dialog_focus.contains_focused(window, cx),
                label: label.into(),
                scrim_dismiss,
                closing: app.overlay.closing_since(),
            }
            .render(card),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sidebar takes its width from the *pair*, and the pair keeps its
    /// proportion — which is what ⌘B has to preserve.
    #[test]
    fn opening_the_sidebar_narrows_both_neighbours_in_the_ratio_they_were_at() {
        let split = workspace_split();
        let closed = shared_room(1200.0, 0.0);
        let open = shared_room(1200.0, SIDEBAR_WIDTH);
        assert_eq!(closed, 1199.0);
        assert_eq!(open, 942.0);

        let (thread_closed, panel_closed) = split.resolve(closed);
        let (thread_open, panel_open) = split.resolve(open);
        // Both got narrower, and by the same share of what the sidebar took.
        assert!(thread_open < thread_closed && panel_open < panel_closed);
        assert!(
            ((thread_closed / panel_closed) - (thread_open / panel_open)).abs() < 0.001,
            "the ratio moved: {thread_closed}:{panel_closed} then {thread_open}:{panel_open}"
        );
    }

    /// …until a floor is reached, where the ratio has to give. A window that
    /// cannot seat both minimums is [`regions`]' takeover case, so what this
    /// pins is the band between: the split yields to the floors rather than
    /// letting either region be squeezed under one.
    #[test]
    fn a_floor_wins_over_the_proportion_before_the_window_runs_out() {
        let split = workspace_split();
        let (thread, panel) = split.resolve(CENTER_MIN + WORKSPACE_MIN + 40.0);
        assert!(thread >= CENTER_MIN && panel >= WORKSPACE_MIN);
        // Half of 720 is 360 — exactly the thread's floor, so the extra 40 all
        // goes to the panel rather than half of it going under the floor.
        assert!((thread - 360.0).abs() < 0.001);
        assert!((panel - 360.0).abs() < 0.001);
    }

    /// A panel band ending at the window's edge, `width` wide.
    fn panel(width: f32) -> chrome::BandSpan {
        chrome::BandSpan {
            x: 1280.0 - width,
            width,
            viewport: 1280.0,
        }
    }

    /// The anchors' room is stated once, and both the band and the strip
    /// arithmetic read it — a strip offered more than its band leaves would
    /// lay chips out under the toggle.
    #[test]
    fn a_strip_is_never_offered_the_room_its_band_reserved() {
        for span in [
            chrome::BandSpan {
                x: 0.0,
                width: 1280.0,
                viewport: 1280.0,
            },
            chrome::BandSpan {
                x: 0.0,
                width: 256.0,
                viewport: 1280.0,
            },
            chrome::BandSpan {
                x: 257.0,
                width: 1023.0,
                viewport: 1280.0,
            },
            chrome::BandSpan {
                x: 257.0,
                width: 503.0,
                viewport: 1280.0,
            },
            panel(520.0),
        ] {
            let (left, right) = chrome::band_insets(span);
            assert_eq!(
                chrome::band_room(span, 0.0, 0.0, 0),
                (span.width - left - right).max(0.0),
            );
            assert_eq!(
                chrome::tab_strip_origin(span),
                span.x + left,
                "a strip starts where its band's leading inset ends",
            );
        }
    }

    /// The rule the whole rework is: the toggles hold still and the clusters
    /// move. A band's leading content stays put while the sidebar's edge is
    /// still left of the anchor, then rides that edge — monotonically, with no
    /// step at the moment the sidebar stops being the leftmost region.
    #[test]
    fn a_sliding_sidebar_pushes_its_neighbour_without_ever_pulling_it_back() {
        let cluster_x = |sidebar: f32| {
            let seam = if sidebar > 0.0 { SEAM_WIDTH } else { 0.0 };
            let span = chrome::BandSpan {
                x: sidebar + seam,
                width: 1280.0 - sidebar - seam,
                viewport: 1280.0,
            };
            chrome::tab_strip_origin(span)
        };
        let mut previous = cluster_x(0.0);
        // Closed, the thread's cluster clears the lights and the toggle.
        assert_eq!(previous, 108.0);
        for step in 1..=64 {
            let x = cluster_x(SIDEBAR_WIDTH * step as f32 / 64.0);
            assert!(
                x >= previous,
                "the cluster moved left at sidebar {}: {x} < {previous}",
                SIDEBAR_WIDTH * step as f32 / 64.0,
            );
            previous = x;
        }
        // Open, it sits just past the sidebar's seam.
        assert_eq!(previous, SIDEBAR_WIDTH + SEAM_WIDTH + 12.0);
    }
}
