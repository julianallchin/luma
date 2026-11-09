//! The persistent shell: the agent panel, the title bar, the content, the
//! status bar, and one overlay slot over all of them.
//!
//! # The window is a venue
//!
//! Everything on screen belongs to the open venue: its modes (Rig, Arrange,
//! Live — see [`crate::workspace`]), the stage over them, the song list over
//! Arrange. `docs/specs/comet-shell.md` is the contract.
//!
//! # Switching is not closing
//!
//! [`crate::workspace::Modes::open`] and `close` hand a [`Body`] back and
//! [`Luma::teardown`] is the only consumer — pausing a closed editor's
//! transport, clearing its loop region. Switching modes goes nowhere near it,
//! which is what makes "the transport keeps playing across a mode switch"
//! structural rather than remembered.
//!
//! # One overlay at a time
//!
//! [`Overlay`] is a single slot, not a stack: venues, add tracks and the rest
//! are all "a plane over the shell, dismissed by Escape", and overlays that
//! each invented their own dismissal would be several ways to do one thing.
//! The venue picker re-opens itself whenever no venue is open and nothing
//! else is up — the window with no venue *is* the picker.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, Context, Div, DragMoveEvent, Window};

use luma_ui::node::{Instrument, Role};
use luma_ui::pane;
use luma_ui::{glass, ladder};

use crate::workspace::{Mode, Target};
use crate::{
    add_tracks, chat_history, chrome, confirm, export_dialog, fixture_picker, keymap, patch,
    settings, stage, subagents, track_editor, tracks, visualizer, welcome, Luma,
};

/// The agent panel's floor, and the content's: the narrowest each is still
/// worth showing at. They bound the seam's drag, and together they decide
/// whether the window can carry the panel at all — a window too narrow for
/// both gives the content the room and the panel comes back when it widens.
const AGENT_MIN: f32 = 300.0;
const CONTENT_MIN: f32 = 480.0;
/// How wide a resizable seam between two panels is: a narrow gutter of the
/// window's trim, so the trim runs on into the page along every split a
/// person can drag. Inside the pane grip's reach, so the hit area is the
/// grip's, not the gutter's.
const SEAM_WIDTH: f32 = 4.0;
/// The status bar along the window's foot.
pub(crate) const STATUS_BAR_HEIGHT: f32 = 24.0;

/// One plane over the whole shell. The regions persist beneath it — closing
/// an overlay reveals them exactly as they were.
pub(crate) enum Overlay {
    /// The venue picker. Auto-opens while no
    /// venue is selected, because a shell with no subject list has nothing
    /// else to offer.
    /// Boxed for the same reason [`Self::AddTracks`] is: a picker carries its
    /// morph, two text fields and their subscriptions, and an enum is as large
    /// as its largest variant — every overlay slot in the app would pay for it.
    Venues(Box<welcome::VenuePicker>),
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
    InsertPattern(Box<track_editor::picker::Picker>),
    /// "Are you sure": one question, two answers, one closed list of acts —
    /// see [`crate::confirm`]. Unboxed because it is a few strings and an
    /// enum, and it is the *smallest* variant here rather than the largest.
    Confirm(confirm::Confirm),
    /// A downloaded update: relaunch into it now, or let the next launch run
    /// it — see [`crate::update`].
    UpdateReady(crate::update::Ready),
    GroupRepair(patch::groups::Repair),
    /// Rendering the open score to a video file. Boxed like the rest: it
    /// carries paths, names and the running export.
    ShowExport(Box<export_dialog::ExportDialog>),
}

impl Overlay {
    /// The key context the overlay's root declares — nested inside
    /// [`keymap::context::ROOT`], beside (not inside) the regions.
    fn key_context(&self) -> &'static str {
        match self {
            Self::ChatHistory(_) => keymap::context::CHAT_HISTORY,
            Self::Venues { .. } => keymap::context::VENUES,
            Self::AddTracks(_) => keymap::context::ADD_TRACKS,
            Self::Subagents(_) => keymap::context::SUBAGENTS,
            Self::FixturePicker(_) => keymap::context::FIXTURE_PICKER,
            Self::InsertPattern(_) => keymap::context::PATTERN_INSERT,
            Self::Confirm(_) => keymap::context::CONFIRM,
            Self::UpdateReady(_) => keymap::context::UPDATE_READY,
            Self::GroupRepair(_) => keymap::context::ROOT,
            Self::ShowExport(_) => keymap::context::SHOW_EXPORT,
        }
    }
}

/// A mode's state: the editor behind one [`Target`].
///
/// The variants mirror `Target`'s — a target is *what* the body shows, and a
/// body is the state showing it.
pub(crate) enum Body {
    TrackEditor(Box<track_editor::Editor>),
    Patch(Box<patch::Patch>),
    Perform(Box<crate::perform::Perform>),
}

/// What should hold the keyboard this frame. Compared across frames so a
/// change of subject re-takes focus while a field the user clicked into keeps
/// it.
#[derive(PartialEq, Eq, Clone)]
pub(crate) enum FocusSlot {
    Overlay(&'static str),
    VisualizerFullscreen,
    Settings,
    Songs,
    Body(Target),
    Shell,
}

impl Luma {
    /// Where the keyboard belongs this frame: the overlay when one is up,
    /// else the settings page or the song list when either is the content,
    /// else the body on screen, else the shell root (whose dispatch path
    /// still reaches every ROOT-scoped binding).
    pub(crate) fn focus_slot(&self) -> FocusSlot {
        if self.fullscreen_presented() {
            return FocusSlot::VisualizerFullscreen;
        }
        // `as_open`: a dialog that is leaving has already given the keyboard
        // back, so focus returns to the opener on the click rather than after
        // the animation.
        if let Some(overlay) = self.overlay.as_open() {
            return FocusSlot::Overlay(overlay.key_context());
        }
        if self.settings.is_some() {
            return FocusSlot::Settings;
        }
        if self.song_list_shown() {
            return FocusSlot::Songs;
        }
        if let Some(target) = self.workspace.active() {
            return FocusSlot::Body(target.clone());
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
        // A dialog and the settings page are each a plane the keyboard goes
        // to and comes back from: leaving one gives it back to the exact
        // element that had it, if that is where the shell returns to.
        let plane = |slot: &FocusSlot| matches!(slot, FocusSlot::Overlay(_) | FocusSlot::Settings);
        let leaving_plane = plane(&self.focused_slot) && !plane(&slot);
        if !plane(&self.focused_slot) && plane(&slot) {
            self.overlay_return_focus = window
                .focused(cx)
                .map(|focus| (self.focused_slot.clone(), focus.downgrade()));
        }
        let leaving_fullscreen = matches!(self.focused_slot, FocusSlot::VisualizerFullscreen);
        self.focused_slot = slot;
        if matches!(self.focused_slot, FocusSlot::VisualizerFullscreen)
            || (leaving_fullscreen && self.visualizer.is_some())
        {
            window.focus(&self.visualizer_focus, cx);
            return;
        }
        if matches!(self.focused_slot, FocusSlot::Overlay(_)) {
            window.focus(&self.dialog_focus, cx);
            return;
        }
        // Only to the slot it was taken from: the settings page left by a
        // mode switch lands on another body, where the opener is not drawn.
        let return_focus = leaving_plane
            .then(|| self.overlay_return_focus.take())
            .flatten()
            .filter(|(from, _)| *from == self.focused_slot)
            .and_then(|(_, focus)| focus.upgrade());
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
    /// trap. Fullscreen likewise keeps focus inside the visualizer. Elsewhere
    /// the shell is deliberately permissive: a click into the agent panel
    /// while a body is up is focus the user moved on purpose, and asking
    /// whether the body still contains it would snatch it back.
    fn keyboard_is_seated(&self, window: &Window, cx: &App) -> bool {
        if matches!(self.focused_slot, FocusSlot::Overlay(_)) {
            return self.dialog_focus.contains_focused(window, cx);
        }
        if matches!(self.focused_slot, FocusSlot::VisualizerFullscreen) {
            return self.visualizer_focus.contains_focused(window, cx);
        }
        // The settings page owns the keyboard as a dialog does. A control on
        // it that took focus and then went away (a row's Reset, once pressed)
        // leaves focus on nothing drawn, and Escape with it.
        if matches!(self.focused_slot, FocusSlot::Settings) {
            return self.focus.contains_focused(window, cx);
        }
        window.focused(cx).is_some()
    }

    /// Reveal `target`, building its body only if it is not already open,
    /// and put its mode on screen. The one entry point every open gesture
    /// routes through — see [`crate::workspace::Modes::open`]. A score
    /// replaced by another runs its teardown here.
    pub(crate) fn open_body(
        &mut self,
        target: Target,
        build: impl FnOnce() -> Body,
        cx: &mut Context<Self>,
    ) {
        if let Some(replaced) = self.workspace.open(target, build) {
            self.teardown(replaced.body, cx);
        }
        cx.notify();
    }

    /// Put `mode` on screen. Rig and Live always have something to show, so
    /// the first visit opens their body; Arrange with no score open shows the
    /// song list.
    pub(crate) fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.songs.is_none() {
            return;
        }
        self.workspace.set_mode(mode);
        if self.workspace.active().is_none() {
            match mode {
                Mode::Rig => self.open_rig(None, cx),
                Mode::Live => self.open_live(None, cx),
                Mode::Arrange => {}
            }
        }
        cx.notify();
    }

    /// Whether the song list is the content this frame: asked for over
    /// Arrange, or Arrange with no score to show instead — unless the score
    /// saved for this venue is still on its way back.
    pub(crate) fn song_list_shown(&self) -> bool {
        let Some(songs) = &self.songs else {
            return false;
        };
        self.workspace.mode() == Mode::Arrange
            && (self.song_list
                || (self.workspace.active().is_none()
                    && !self.saved_venues.restoring(songs.venue_id())))
    }

    /// Open or close the song list: the title bar's song title and ⌘P. Opening
    /// it from another mode goes to Arrange.
    pub(crate) fn toggle_song_list(&mut self, cx: &mut Context<Self>) {
        if self.songs.is_none() {
            return;
        }
        if self.workspace.mode() != Mode::Arrange {
            self.workspace.set_mode(Mode::Arrange);
            self.song_list = true;
        } else {
            self.song_list = !self.song_list_shown();
        }
        // The list takes the window's width: the agent panel stands aside
        // until it closes (`agent_wanted`).
        if self.song_list {
            self.agent_over_songs = false;
        }
        self.song_list_close = None;
        cx.notify();
    }

    /// Close the song list when there is something under it to go back to.
    /// Reports whether it closed — Escape's rung for it.
    pub(crate) fn close_song_list(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.song_list_shown() || self.workspace.active().is_none() {
            return false;
        }
        self.song_list = false;
        self.song_list_close = None;
        cx.notify();
        true
    }

    /// A closed body's exit rites. The only caller of anything here is a
    /// *close* — switching modes never comes through this path.
    pub(crate) fn teardown(&mut self, body: Body, cx: &mut Context<Self>) {
        match body {
            Body::TrackEditor(state) => {
                if let Some(session) = state.playback_session() {
                    let pause = self.library.pause(session);
                    cx.background_spawn(async move {
                        pause.await.ok();
                    })
                    .detach();
                }
            }
            Body::Patch(_) => {}
            Body::Perform(state) => self.close_perform(&state, cx),
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
        if matches!(self.overlay.as_open(), Some(Overlay::InsertPattern(_))) {
            self.dismiss_insert_menu();
        }
        self.overlay.begin_close(cx);
        cx.notify();
    }

    /// Escape: the insertion menu first (it is the thing the eye is on), then
    /// the overlay. The venue picker with nothing selected stays — see the
    /// module docs.
    pub(crate) fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        if matches!(self.overlay.as_open(), Some(Overlay::InsertPattern(_))) {
            self.close_overlay(cx);
            return;
        }
        if self.dismiss_insert_menu() {
            cx.notify();
            return;
        }
        if self.venue_menu.is_open() {
            self.close_venue_menu(cx);
            return;
        }
        if self.close_score_switcher(cx) {
            return;
        }
        // The song list's menus are on the same rung: floats over the shell,
        // closed before anything under them is.
        if self.close_score_menu(cx) || self.close_song_menu(cx) {
            return;
        }
        // Innermost first: a dialog showing a child's transcript steps back to
        // its list before the list itself closes.
        if self.subagents_to_list(cx) {
            return;
        }
        // The settings page is the content while it is up: nothing under it
        // is on screen to step out of.
        if self.overlay.as_open().is_none() && self.dismiss_settings(cx) {
            return;
        }
        if self.overlay.as_open().is_none() {
            if let Some(visualizer) = self.visualizer_mut() {
                if visualizer.settings.is_open() {
                    visualizer.settings.close();
                    cx.notify();
                    return;
                }
            }
            if let Some(Body::Patch(page)) = self.workspace.active_body_mut() {
                if page.stage.objects_open {
                    page.stage.objects_open = false;
                    cx.notify();
                    return;
                }
                if page.details_open {
                    if page.mode_menu.take().is_some() || page.menu.take().is_some() {
                        cx.notify();
                        return;
                    }
                    if page.panel.take().is_some() {
                        cx.notify();
                        return;
                    }
                    page.details_open = false;
                    cx.notify();
                    return;
                }
                if page.group_editor.is_some() && !page.group_busy {
                    page.group_editor = None;
                    page.group_refusal = None;
                    cx.notify();
                    return;
                }
            }
        }
        // The stage builder's hand — and its node menu — are floats over the
        // room on the same rung as the shell's own menus, handled here so
        // Escape reaches them wherever focus happens to be.
        let stage_menu = matches!(
            self.workspace.active_body(),
            Some(Body::Patch(page)) if page.stage.menu.is_some()
        );
        let stage_up = matches!(self.workspace.active_body(), Some(Body::Patch(_)));
        if self.overlay.as_open().is_none()
            && (stage_menu
                || self
                    .build_state()
                    .is_some_and(|build| !matches!(build.hand, crate::stage::hand::Hand::Idle))
                || (stage_up
                    && self
                        .build_state()
                        .is_some_and(|build| build.selected.is_some())))
        {
            self.stage_escape(cx);
            return;
        }
        match self.overlay.as_open() {
            Some(Overlay::Venues(_)) if self.songs.is_none() => {}
            Some(Overlay::ShowExport(dialog)) if dialog.is_running() => {}
            Some(_) => self.close_overlay(cx),
            // Nothing floating: keep stepping back out, innermost first — the
            // song list over Arrange, then a menu inside the args sheet, then
            // the clip graph back to the timeline with the clip still
            // selected, then the selection itself. Live's pattern editor goes
            // back to its surface the same way. All routed through this one
            // ladder rather than given `escape` bindings of their own, which
            // would have to out-scope the shell's and would then be the only
            // Escapes in the app that do not mean "close what is over me".
            None => {
                if self.close_song_list(cx) {
                    return;
                }
                if self.dismiss_sheet_menu() {
                    cx.notify();
                    return;
                }
                if self.close_clip_graph(cx) {
                    return;
                }
                if let Some(target) = self.workspace.active().cloned() {
                    if self.close_pattern_sheet(&target, cx) {
                        return;
                    }
                }
                self.clear_clip_selection(cx);
            }
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// What a [`Region`] shows: a function of the app, run with the app's own
/// context so every listener it builds is the app's.
type RegionBody = fn(&mut Luma, &mut Window, &mut Context<Luma>) -> AnyElement;

/// One part of the window drawn as its own view.
///
/// # Why the window is cut into views
///
/// The window renders from one `Luma`, and gpui renders the root view on every
/// frame it draws. The stage asks for a frame on every render (see
/// [`visualizer::visualizer`]), so while every pane was part of the root, each
/// stage frame rebuilt and laid out every pane in the window — the fixture
/// table, the song list, the editors — with nothing in them changed. A *cached*
/// region is not rebuilt: gpui reuses its last layout, prepaint and paint
/// until the region's entity is notified (or its bounds change, or the window
/// is refreshed).
///
/// # What rebuilds a cached region
///
/// - **Any notify of the app.** A cached region observes `Luma`, so every
///   `cx.notify()` that used to rebuild it as part of the root still does.
/// - **Anything it asks for itself.** A `request_animation_frame` made while
///   it renders, a `use_state` entity or a child view notifies the region,
///   because the region is the view being rendered.
///
/// What does not rebuild it is the stage's frame loop: the stage renders in a
/// *live* region of its own (never cached — it changes every frame), so its
/// `request_animation_frame` notifies that region and not the app. Input that
/// only moves the stage's camera notifies the stage's region too (see
/// [`Luma::notify_stage`]).
///
/// # The one gap
///
/// gpui runs no observers for a notify made *during* a draw: it only marks the
/// view dirty. So a draw-time sync in the root render (`sync_visualizer` and
/// its siblings) must not change what a cached region shows unless a notify
/// outside the draw caused it. Each of those syncs runs because some gesture
/// or load already notified the app, and that notify is what rebuilds the
/// region in the same frame.
pub(crate) struct Region {
    app: gpui::WeakEntity<Luma>,
    body: RegionBody,
    _app_changed: Option<gpui::Subscription>,
    /// How many times this region has rendered, published as a text node so a
    /// script can see that a frame reused it.
    #[cfg(feature = "agent")]
    renders: (&'static str, u64),
}

impl Region {
    fn new(
        app: &gpui::Entity<Luma>,
        label: &'static str,
        body: RegionBody,
        cached: bool,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        #[cfg(not(feature = "agent"))]
        let _ = label;
        Self {
            app: app.downgrade(),
            body,
            _app_changed: cached.then(|| cx.observe(app, |_, _, cx| cx.notify())),
            #[cfg(feature = "agent")]
            renders: (label, 0),
        }
    }
}

impl gpui::Render for Region {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let body = self.body;
        let element = self
            .app
            .upgrade()
            .map(|app| app.update(cx, |app, cx| body(app, window, cx)));
        // A cached region is laid out as its own root at the size its host
        // gave it, so its content fills that box rather than being measured.
        let region = div().size_full().flex().flex_col().children(element);
        #[cfg(feature = "agent")]
        let region = {
            let (label, renders) = &mut self.renders;
            *renders += 1;
            region.child(
                div()
                    .size_0()
                    .agent_node(Role::Text, format!("{label} renders = {renders}")),
            )
        };
        region
    }
}

#[derive(Default)]
pub(crate) struct Regions {
    body: Option<gpui::Entity<Region>>,
    songs: Option<gpui::Entity<Region>>,
    status_bar: Option<gpui::Entity<Region>>,
    environment: Option<gpui::Entity<Region>>,
    stage: Option<gpui::Entity<Region>>,
    fullscreen_stage: Option<gpui::Entity<Region>>,
}

impl Regions {
    fn get(
        slot: &mut Option<gpui::Entity<Region>>,
        label: &'static str,
        body: RegionBody,
        cached: bool,
        cx: &mut Context<Luma>,
    ) -> gpui::Entity<Region> {
        slot.get_or_insert_with(|| {
            let app = cx.entity();
            cx.new(|cx| Region::new(&app, label, body, cached, cx))
        })
        .clone()
    }

    /// The live region the fullscreen stage renders in.
    pub(crate) fn fullscreen_stage(&mut self, cx: &mut Context<Luma>) -> gpui::Entity<Region> {
        Self::get(
            &mut self.fullscreen_stage,
            "Fullscreen stage",
            crate::fullscreen::stage,
            false,
            cx,
        )
    }
}

impl Luma {
    /// Redraw the stage for input that changed only the stage — the camera, a
    /// gizmo's hover. Unlike `cx.notify()`, this leaves every cached region
    /// as it was.
    pub(crate) fn notify_stage(&self, cx: &mut App) {
        let regions = &self.regions;
        for region in [&regions.stage, &regions.fullscreen_stage]
            .into_iter()
            .flatten()
        {
            cx.notify(region.entity_id());
        }
    }
}

/// Mount a cached region in a box `style` sizes.
fn cached(region: gpui::Entity<Region>, style: gpui::StyleRefinement) -> AnyElement {
    luma_ui::node::cached_view(region, style).into_any_element()
}

/// The whole window: the dark trim, carrying the title bar across its top and
/// the status bar along its foot, and between them the page — one rounded
/// card inset from the window's edge that holds the agent panel and the
/// content beside it. The overlay sits over all of it, and the traffic lights
/// over that.
///
/// **Inside the page, panels are flush.** They touch, a hairline between
/// each, with no gaps and no rounded corners of their own; only the page
/// rounds (`docs/specs/comet-shell.md` §6).
pub(crate) fn regions(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> Div {
    let entity = cx.entity();
    let lights = chrome::corner_lit(window);
    // Settings replaces the page's whole content in one frame: the agent
    // panel is not slid away, it is simply not drawn, and its width is left
    // as it was so it comes back the same way.
    let row = match &app.settings {
        Some(_) => settings_page(app, cx),
        None => working_row(app, window, cx),
    };

    // Reap a dismissed dialog once its out-animation has played, and keep
    // frames coming until it has. Frame-driven rather than timer-driven —
    // see `Popup::tick_close`.
    if let Some(page) = app.stage_page_for_tick() {
        let _ = page.closing.tick_close();
    }
    if app.overlay.tick_close() {
        window.request_animation_frame();
    }
    if app.venue_menu.tick_close() {
        window.request_animation_frame();
    }
    if app.score_switcher.tick_close() {
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
    add_tracks::reap_preview(app, cx);
    if let Some(Overlay::ChatHistory(state)) = app.overlay.open_mut() {
        chat_history::tick(state, window, cx);
    }
    if matches!(app.overlay.as_open(), Some(Overlay::InsertPattern(_))) {
        track_editor::picker::tick(app, window, cx);
    }
    if matches!(app.overlay.as_open(), Some(Overlay::FixturePicker(_))) {
        fixture_picker::tick(app, window, cx);
    }
    if matches!(app.overlay.as_open(), Some(Overlay::Subagents(_))) {
        // Read out of the chat before the overlay is borrowed: both live on
        // `app`, and the dialog cannot reach back through the entity for them.
        let rows = app
            .front_chat()
            .map(|chat| chat.read(cx).subagents().to_vec())
            .unwrap_or_default();
        if let Some(Overlay::Subagents(state)) = app.overlay.open_mut() {
            subagents::tick(state, rows, window, cx);
        }
    }
    let status_bar = Regions::get(
        &mut app.regions.status_bar,
        "Status bar",
        crate::status_bar::bar,
        true,
        cx,
    );
    let page = div()
        .flex_1()
        .min_h_0()
        .mx(px(glass::WINDOW_TRIM))
        .flex()
        .flex_col()
        .relative()
        .bg(ladder::background())
        .child(row)
        .child(page_corners())
        .agent_node(Role::Card, "Page");
    let mut window_root =
        div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(glass::window_trim())
            // The title bar spans the window: it is on the trim, not on the
            // page, so the agent panel's coming and going never moves it.
            .child(title_bar(app, lights, &entity, cx))
            .child(page)
            .child(cached(
                status_bar,
                gpui::StyleRefinement::default()
                    .w_full()
                    .h(px(STATUS_BAR_HEIGHT))
                    .flex_none(),
            ))
            // The title bar's two fixed ends. Above the page, but *below* the
            // overlay, unlike the traffic lights: a control reachable through a
            // scrim would move the regions behind a modal that is meant to own
            // the window. See [`crate::chrome`] for the anchor rule.
            .children(app.settings.is_none().then(|| {
                chrome::agent_toggle(&entity, app.agent_wanted(), lights).into_any_element()
            }))
            .children(
                app.settings
                    .is_none()
                    .then(|| chrome::mode_switch(app, &entity, cx).into_any_element()),
            );
    if let Some(overlay) = app.overlay.get() {
        window_root = window_root.child(overlay_layer(app, overlay, &entity, window, cx));
    }
    // Last, so nothing — least of all an overlay — can cover the only controls
    // that move and close the window. See [`crate::chrome`].
    window_root.children(chrome::window_controls(window))
}

/// The page's content while settings is not up: the agent panel beside the
/// mode's content, and the seam between them.
fn working_row(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> Div {
    let viewport = f32::from(window.viewport_size().width);
    // Geometry follows state: the panel's resting width is restated every
    // frame as a pure function of the window and the seam. Only showing or
    // hiding the panel slides it. A width that moved because the seam was
    // dragged or the window resized is *set*: it is derived from something
    // already continuous, and a tween retargeted every frame trails the
    // pointer it is meant to be under (see `PaneWidth::settled`).
    let open_width = app.agent_open_width(viewport);
    let resting = if app.agent_shown(viewport) {
        open_width
    } else {
        0.0
    };
    let toggled = (resting > 0.0) != (app.agent_width.target() > 0.0);
    if app.shell_presented && (toggled || !app.agent_width.settled()) {
        app.agent_width.retarget(resting, cx);
    } else {
        app.agent_width.set(resting);
    }
    app.shell_presented = true;
    let agent_w = f32::from(app.agent_width.eval(window));
    let show_agent = agent_w > 0.0;

    let mut row = div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_row()
        .relative()
        .on_drag_move(cx.listener(Luma::drag_agent_seam))
        .on_drag_move(cx.listener(Luma::drag_visualizer_seam));

    if show_agent {
        // Laid out at its open width for the whole slide (see `pane::pane`),
        // so a panel easing away reveals its transcript rather than
        // re-wrapping it.
        row = row
            .child(
                pane::pane(px(agent_w), px(open_width), agent_panel(app))
                    .agent_node(Role::Card, "Agent"),
            )
            .child(seam());
    }
    row = row.child(
        div()
            .h_full()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(ladder::background())
            .key_context(keymap::context::WORKSPACE)
            .child(content(app, window, cx)),
    );
    if show_agent {
        // After both panels it divides, so it owns the strip it overhangs.
        row = row.child(agent_grip(agent_w + SEAM_WIDTH / 2.0, cx));
    }
    row
}

/// The page's rounded corners. gpui clips to rectangles, so the panels inside
/// would paint square corners over a rounded page; instead a ring of trim is
/// drawn over the page's edge — outer radius flush with the trim, inner radius
/// the page's — and covers what lies outside the curve. It carries no
/// handlers, so it takes no hitbox and the pointer passes through it.
fn page_corners() -> Div {
    let trim = glass::WINDOW_TRIM;
    div()
        .absolute()
        .top(px(-trim))
        .bottom(px(-trim))
        .left(px(-trim))
        .right(px(-trim))
        .border(px(trim))
        .border_color(glass::window_trim())
        .rounded(px(glass::PAGE_RADIUS + trim))
}

/// The agent panel: the chat of the body on screen, unchanged, flush at the
/// page's left. With nothing open — Arrange before a score is picked — it
/// says where a conversation comes from.
fn agent_panel(app: &Luma) -> AnyElement {
    let chat = app.front_chat().cloned().map(|chat| {
        luma_ui::node::cached_view(chat, gpui::StyleRefinement::default().size_full())
            .into_any_element()
    });
    div()
        .size_full()
        .relative()
        .flex()
        .flex_col()
        // The ground, not a card on it. **Opaque**: a translucent plane would
        // make the transcript's fade bands unpaintable — a plane composited
        // over an unknown backdrop has no colour any overlay can match.
        .bg(glass::panel_opaque())
        .key_context(keymap::context::THREAD)
        .child(chat.unwrap_or_else(|| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(24.))
                        .text_size(px(13.))
                        .text_color(luma_ui::glass::ink(0.42))
                        .child(NO_CHAT)
                        .agent_node(Role::Text, NO_CHAT),
                )
                .into_any_element()
        }))
        .into_any_element()
}

/// What the agent panel says while the mode on screen has nothing open.
const NO_CHAT: &str = "Open a score to talk about it.";

/// The window's head, on the trim above the page: the venue menu and —
/// reserved by the band's insets — the agent toggle and the mode switch
/// anchored over its two ends. Its middle is empty in every mode: what the
/// mode is working on is the page's to say.
fn title_bar(app: &Luma, lights: bool, entity: &gpui::Entity<Luma>, cx: &App) -> impl IntoElement {
    chrome::band(lights, app.settings.is_none())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .child(match app.settings {
                    Some(_) => settings::back(entity),
                    None => welcome::venue_trigger(app, entity, cx),
                }),
        )
        .agent_node(Role::Card, "Title bar")
}

/// The window's content under the title bar: the stage over the mode's body,
/// or the song list in their place.
fn content(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    if app.songs.is_none() {
        // The venue picker is up over this.
        return div().flex_1().into_any_element();
    }
    if app.song_list_shown() {
        let region = Regions::get(&mut app.regions.songs, "Songs", songs_body, true, cx);
        return div()
            .flex_1()
            .min_h_0()
            .flex()
            .key_context(keymap::context::SONGS)
            .child(cached(
                region,
                gpui::StyleRefinement::default()
                    .flex_1()
                    .min_w_0()
                    .min_h_0(),
            ))
            .into_any_element();
    }
    if app.workspace.active().is_none() {
        // A mode whose body is still on its way: Rig and Live open theirs on
        // the first visit, and a restored score lands with its catalogue.
        return div().flex_1().into_any_element();
    }
    if app.visualizer.is_none() {
        return div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(body_region(app, cx))
            .into_any_element();
    }
    let available = content_height(window);
    let (stage_height, _) = app.active_visualizer_split().resolve(available);
    let grip = visualizer_grip(stage_height + SEAM_WIDTH / 2.0, cx);
    // The venue's room — indoor or outdoor, sun, haze — sits beside the stage
    // it lights, in Rig only.
    let environment = (app.workspace.mode() == Mode::Rig).then(|| {
        let region = Regions::get(
            &mut app.regions.environment,
            "Environment",
            environment_body,
            true,
            cx,
        );
        cached(
            region,
            gpui::StyleRefinement::default()
                .w(px(visualizer::ENVIRONMENT_PANEL_WIDTH))
                .h_full()
                .flex_none(),
        )
    });
    // What Live follows — the decks, or the tapped tempo — sits beside the
    // stage, in Live only, at the width last dragged.
    let decks = match app.workspace.active_body() {
        Some(Body::Perform(state)) if app.workspace.mode() == Mode::Live => {
            let width = app.deck_panel_width(content_width(app, window));
            Some((width, crate::perform::deck_panel(state, &cx.entity())))
        }
        _ => None,
    };
    let stage = stage_region(app, cx);
    // Arrange names its song and score over the stage's top edge, between
    // the corner controls: the door to the song's other scores.
    let switcher = crate::score_switcher::trigger(app, &cx.entity(), cx);
    let mut band = div()
        .h(px(stage_height))
        .flex_none()
        .flex()
        .relative()
        .overflow_hidden()
        .on_drag_move(cx.listener(Luma::drag_deck_seam));
    if let Some((width, panel)) = decks {
        band = band
            .child(div().w(px(width)).h_full().flex_none().child(panel))
            .child(seam())
            .child(deck_grip(width + SEAM_WIDTH / 2.0, cx));
    }
    let band = band
        .child(stage_slot(stage).relative().children(switcher))
        .children(environment);
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        // The grip below is placed against this column's own top edge.
        .relative()
        .child(band)
        .child(seam_across())
        .child(body_region(app, cx))
        .child(grip)
        .into_any_element()
}

/// The settings page, in place of the whole page's content.
fn settings_page(app: &Luma, cx: &mut Context<Luma>) -> Div {
    let Some(state) = &app.settings else {
        return div();
    };
    let holds_focus = matches!(app.focus_slot(), FocusSlot::Settings);
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .key_context(keymap::context::SETTINGS)
        .when(holds_focus, |page| page.track_focus(&app.focus))
        .child(settings::settings(app, state, &cx.entity(), cx).agent_node(Role::Card, "Settings"))
}

/// How tall the stage and the body share: the page — the window less the
/// title bar and the status bar on the trim — less the gutter between them
/// and a seam grip's reach.
fn content_height(window: &Window) -> f32 {
    f32::from(window.viewport_size().height)
        - chrome::HEIGHT
        - STATUS_BAR_HEIGHT
        - SEAM_WIDTH
        - pane::HANDLE_WIDTH
}

/// How wide the page is in a window `viewport` wide: the window less the
/// trim either side.
fn page_width(viewport: f32) -> f32 {
    viewport - 2.0 * glass::WINDOW_TRIM
}

/// The song list page, in its cached region.
fn songs_body(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let holds_focus = matches!(app.focus_slot(), FocusSlot::Songs);
    let focus = app.focus.clone();
    let app = &*app;
    let statuses = app.body_statuses(cx);
    match &app.songs {
        Some(songs) => div()
            .size_full()
            .flex()
            .when(holds_focus, |page| page.track_focus(&focus))
            .on_key_down(tracks::on_key(&entity))
            .child(tracks::page(app, songs, &statuses, &entity, window, cx))
            .into_any_element(),
        None => div().into_any_element(),
    }
}

/// The body on screen, in its cached region. It fills the rest of whichever
/// column it is placed in.
fn body_region(app: &mut Luma, cx: &mut Context<Luma>) -> AnyElement {
    let region = Regions::get(&mut app.regions.body, "Body", active_body, true, cx);
    cached(
        region,
        gpui::StyleRefinement::default()
            .flex_1()
            .min_w_0()
            .min_h_0(),
    )
}

fn environment_body(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    match &app.visualizer {
        Some(state) => visualizer::environment_panel(state, &entity, window),
        None => div().into_any_element(),
    }
}

/// The embedded stage's live region. Never cached: the stage changes every
/// frame, and its frame loop notifies this region rather than the app.
fn stage_region(app: &mut Luma, cx: &mut Context<Luma>) -> AnyElement {
    Regions::get(&mut app.regions.stage, "Stage", stage_body, false, cx).into_any_element()
}

/// The embedded stage, with the venue page's tools in Rig.
fn stage_body(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let stage_view = app.stage_view();
    let venue_tools = match (app.workspace.active_body(), stage_view.as_ref()) {
        (Some(Body::Patch(page)), Some(stage_view)) => Some(stage::controls(
            &page.stage,
            &cx.entity(),
            Some(stage_view),
            window,
        )),
        _ => None,
    };
    stage_element(app, venue_tools, window, cx).unwrap_or_else(|| div().into_any_element())
}

/// The stage's element: the live view, or the placeholder it leaves while it
/// is presented fullscreen. `None` when there is no stage to show.
fn stage_element(
    app: &mut Luma,
    venue_tools: Option<AnyElement>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Option<AnyElement> {
    let fullscreen_slot = app
        .fullscreen
        .as_ref()
        .filter(|_| app.fullscreen_presented())
        .map(|state| state.slot.clone());
    let focus = app.visualizer_focus.clone();
    // Split the borrow the way `active_body` does: the stage's element
    // mutates its own state and reads the library synchronously, and the two
    // fields are disjoint.
    let Luma {
        visualizer,
        library,
        ..
    } = app;
    let state = visualizer.as_mut()?;
    Some(match fullscreen_slot {
        Some(slot) => crate::fullscreen::placeholder(slot),
        None => visualizer::visualizer(
            state,
            &cx.entity(),
            library,
            window,
            visualizer::Chrome::Embedded { venue_tools },
            &focus,
        )
        .into_any_element(),
    })
}

/// The box the stage fills, under the stage's own key context.
fn stage_slot(stage: AnyElement) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .key_context(keymap::context::VISUALIZER)
        .child(stage)
}

/// The gutter between two panels side by side, on a seam that drags: full
/// height, in the trim's colour.
fn seam() -> Div {
    div()
        .h_full()
        .flex_none()
        .w(px(SEAM_WIDTH))
        .bg(glass::window_trim())
}

/// The gutter between two panels stacked, on a seam that drags: full width,
/// in the trim's colour.
fn seam_across() -> Div {
    div()
        .w_full()
        .flex_none()
        .h(px(SEAM_WIDTH))
        .bg(glass::window_trim())
}

/// How the window divides between the agent panel and the content, and the
/// floors neither may be dragged below. The panel opens at a quarter of the
/// window.
pub(crate) fn agent_split() -> luma_ui::split::SplitFraction {
    luma_ui::split::SplitFraction::new(0.25, AGENT_MIN, CONTENT_MIN)
}

/// The grip that pulls the agent panel's seam: a wider invisible strip over
/// the hairline at `at`, so the pointer can hit the boundary without the
/// boundary having to be thick enough to aim at. Double-click restores the
/// default split.
fn agent_grip(at: f32, cx: &mut Context<Luma>) -> impl IntoElement {
    pane::resize_handle(
        "agent-seam",
        pane::Seam::Vertical,
        at,
        || AgentResize,
        |app: &mut Luma, _| app.agent_split.reset(),
        glass::glass_hover(),
        cx,
    )
    // A slider is the closest thing in the closed role vocabulary to a grip
    // whose position along an axis is the value — which is what the seam is.
    .agent_node(Role::Slider, "Agent width")
}

/// The agent panel's seam, under the pointer. gpui routes a drag by the type
/// it carries, so this marker is what tells the row's listener that the
/// pointer belongs to this seam rather than to anything else being dragged.
struct AgentResize;

impl Luma {
    /// The agent panel's width when it is shown, in a window `viewport` wide.
    fn agent_open_width(&self, viewport: f32) -> f32 {
        self.agent_split
            .resolve(page_width(viewport) - SEAM_WIDTH)
            .0
    }

    /// Whether the agent panel is on screen: not put away by hand, and a
    /// page wide enough to seat it beside a usable content column.
    fn agent_shown(&self, viewport: f32) -> bool {
        self.agent_wanted() && page_width(viewport) - SEAM_WIDTH >= AGENT_MIN + CONTENT_MIN
    }

    /// Track the pointer while the agent seam is dragged. A drag is already
    /// continuous, so the split follows it directly.
    fn drag_agent_seam(
        &mut self,
        event: &DragMoveEvent<AgentResize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Measured against the row the seam divides — the page, inset from
        // the window's edge by the trim.
        // The pointer holds the gutter by its middle.
        let room = f32::from(event.bounds.size.width) - SEAM_WIDTH;
        let at = f32::from(event.event.position.x - event.bounds.left()) - SEAM_WIDTH / 2.0;
        self.agent_split.drag_to(at, room);
        cx.notify();
    }
}

/// The session item Live's deck panel width is kept under.
pub(crate) const DECK_WIDTH_KEY: &str = "deck-panel-width";
/// Live's deck panel width as stored: `None` until first dragged.
pub(crate) type DeckWidth = crate::session_store::Stored<Option<f32>>;
/// The deck panel's width before it is ever dragged, and its floor and
/// ceiling. The stage keeps at least [`DECK_STAGE_MIN`] beside it.
const DECK_DEFAULT: f32 = 320.0;
const DECK_MIN: f32 = 240.0;
const DECK_MAX: f32 = 560.0;
const DECK_STAGE_MIN: f32 = 320.0;

/// How wide the content column is: the page less the agent panel and its
/// seam when the panel is up.
fn content_width(app: &Luma, window: &Window) -> f32 {
    let viewport = f32::from(window.viewport_size().width);
    if app.agent_shown(viewport) {
        page_width(viewport) - app.agent_open_width(viewport) - SEAM_WIDTH
    } else {
        page_width(viewport)
    }
}

/// The grip on the deck panel's seam, centred on the rule at `at` inside the
/// stage band. Double-click restores the default width.
fn deck_grip(at: f32, cx: &mut Context<Luma>) -> impl IntoElement {
    pane::resize_handle(
        "deck-seam",
        pane::Seam::Vertical,
        at,
        || DeckResize,
        |app: &mut Luma, cx| app.store_session(|this| &mut this.deck_width, None, cx),
        glass::glass_hover(),
        cx,
    )
    .agent_node(Role::Slider, "Deck panel width")
}

/// The deck panel's seam, under the pointer — routed by type like
/// [`AgentResize`].
struct DeckResize;

impl Luma {
    /// The deck panel's width in a content column `room` wide: as stored,
    /// within its bounds, and never so wide the stage loses its floor.
    fn deck_panel_width(&self, room: f32) -> f32 {
        let wanted = self.deck_width.value().unwrap_or(DECK_DEFAULT);
        let ceiling = (room - SEAM_WIDTH - DECK_STAGE_MIN)
            .min(DECK_MAX)
            .max(DECK_MIN);
        wanted.clamp(DECK_MIN, ceiling)
    }

    /// Follow the pointer while the deck seam is dragged, and keep the width
    /// for this device. The listener sits on the stage band, so its left
    /// edge is the panel's.
    fn drag_deck_seam(
        &mut self,
        event: &DragMoveEvent<DeckResize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(event.event.position.x - event.bounds.left()) - SEAM_WIDTH / 2.0;
        let room = f32::from(event.bounds.size.width);
        let ceiling = (room - SEAM_WIDTH - DECK_STAGE_MIN)
            .min(DECK_MAX)
            .max(DECK_MIN);
        let width = width.clamp(DECK_MIN, ceiling).round();
        self.store_session(|this| &mut this.deck_width, Some(width), cx);
        cx.notify();
    }
}

/// The stage's resting share of the content column (half), and the floors
/// that keep either half from being dragged away entirely.
///
/// A stage needs enough height to read as a room rather than a letterbox; a
/// body needs enough for its own head plus a row or two.
pub(crate) fn visualizer_split() -> luma_ui::split::SplitFraction {
    luma_ui::split::SplitFraction::new(0.5, 140.0, 200.0)
}

/// The grip on the stage's lower seam, centred on the rule at `at`. Mounted
/// after the stage *and* the body for the reason [`agent_grip`] is.
fn visualizer_grip(at: f32, cx: &mut Context<Luma>) -> impl IntoElement {
    pane::resize_handle(
        "visualizer-seam",
        pane::Seam::Horizontal,
        at,
        || VisualizerResize,
        |app: &mut Luma, _| app.active_visualizer_split().reset(),
        glass::glass_hover(),
        cx,
    )
    .agent_node(Role::Slider, "Stage height")
}

/// The stage/body seam, under the pointer — routed by type like
/// [`AgentResize`].
struct VisualizerResize;

impl Luma {
    fn active_visualizer_split(&mut self) -> &mut luma_ui::split::SplitFraction {
        if matches!(self.workspace.active_body(), Some(Body::TrackEditor(_))) {
            &mut self.score_editor_split
        } else {
            &mut self.visualizer_split
        }
    }

    /// Track the pointer while the stage seam is dragged. Like the agent
    /// seam, the height follows the pointer directly.
    fn drag_visualizer_seam(
        &mut self,
        event: &DragMoveEvent<VisualizerResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let available = content_height(window);
        // The column starts at the top of the row that listens — the page's
        // top edge, under the title bar — and the pointer holds the gutter by
        // its middle.
        let offset = f32::from(event.event.position.y - event.bounds.top()) - SEAM_WIDTH / 2.0;
        self.active_visualizer_split().drag_to(offset, available);
        cx.notify();
    }
}

/// The body on screen, with its own key context nested inside the
/// workspace's — which is what lets the track editor's whole binding block
/// survive the shell's moves character-for-character.
fn active_body(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let Some(target) = app.workspace.active().cloned() else {
        return div().into_any_element();
    };
    // The one focus handle lives at exactly one element per frame, and
    // [`Luma::focus_slot`] is the only thing that decides which — asking it
    // rather than restating its conditions is what keeps the handle from
    // being tracked twice, or nowhere.
    let holds_focus = matches!(app.focus_slot(), FocusSlot::Body(_));
    let focus = app.focus.clone();
    let Some(body) = app.workspace.body_mut(&target) else {
        return div().into_any_element();
    };
    let layout = matches!(&*body, Body::Patch(_));
    let graph_page = match &mut *body {
        Body::TrackEditor(state) => state.clip_graph_shown(),
        Body::Perform(state) => state.pattern_sheet.is_some(),
        Body::Patch(_) => false,
    };
    let mut keys = gpui::KeyContext::default();
    // The graph page is the clip graph's key scope, not the timeline's: the
    // timeline's letters and Delete act on the selected *clips*, and over the
    // page the selection is the clip being edited. Live's pattern editor is
    // the same page.
    keys.add(if layout {
        keymap::context::STAGE
    } else if graph_page {
        keymap::context::CLIP_GRAPH
    } else {
        target.key_context()
    });
    if layout {
        keys.add(keymap::context::PATCH);
        keys.add(keymap::context::VISUALIZER);
    }
    let inner = match body {
        Body::TrackEditor(state) if graph_page => {
            track_editor::clip_graph_page(state, &entity, window, cx)
        }
        Body::TrackEditor(state) => {
            track_editor::track_editor(state, &entity, window, cx).into_any_element()
        }
        Body::Patch(state) => patch::patch(state, &entity).into_any_element(),
        Body::Perform(state) => {
            crate::perform::pattern_sheet::sync(state, window, cx);
            crate::perform::perform(state, &entity)
        }
    };
    div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .when(holds_focus, |body| body.track_focus(&focus))
        .key_context(keys)
        .child(inner)
        .into_any_element()
}

/// The overlay plane: a full-area surface over the regions.
/// `cx` is here for the pickers' loading state: a skeleton pulses off the one
/// shared clock ([`luma_ui::motion::pulse_delta`]), which needs an `App` to
/// take its lease from. That keeps every skeleton in the window phase-locked.
fn overlay_layer(
    app: &Luma,
    overlay: &Overlay,
    entity: &gpui::Entity<Luma>,
    window: &Window,
    cx: &mut Context<Luma>,
) -> AnyElement {
    // Every arm hands back a finished card. There is one card mechanism —
    // `morph::card` — and a dialog that never morphs simply has one route
    // (`morph::fixed_card`); the shell does not describe a dialog's box.
    let (card, label) = match overlay {
        Overlay::Venues(state) => {
            let statuses = app.venue_statuses(cx);
            (
                welcome::render(state, &statuses, entity, window, cx),
                "Venue dialog",
            )
        }
        Overlay::AddTracks(state) => (
            add_tracks::render(
                state,
                add_tracks::Ambient {
                    activity: app.track_import.as_ref(),
                    preview: app.source_preview.as_ref(),
                    art: &app.source_art,
                },
                entity,
                window,
                cx,
            ),
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
        Overlay::InsertPattern(state) => (
            match app.workspace.active_body() {
                Some(Body::TrackEditor(editor)) => {
                    track_editor::picker::render(state, editor, entity)
                }
                _ => div().into_any_element(),
            },
            "Insert pattern dialog",
        ),
        Overlay::FixturePicker(state) => (
            fixture_picker::render(state, entity, window),
            "Fixture picker dialog",
        ),
        Overlay::GroupRepair(state) => (
            patch::groups::repair_dialog(state, entity),
            "Group repair dialog",
        ),
        Overlay::ShowExport(state) => (export_dialog::render(state, entity), "Export show dialog"),
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
        Overlay::UpdateReady(state) => (
            crate::update::render(
                state,
                entity,
                &app.dialog_first_focus,
                app.dialog_first_focus.is_focused(window),
                &app.dialog_last_focus,
                app.dialog_last_focus.is_focused(window),
            ),
            "Update ready dialog",
        ),
    };
    // A running export is closed by its own Cancel, never by a stray click.
    let scrim_dismiss =
        if (matches!(overlay, Overlay::Venues(_)) && app.songs.is_none()) || app.exporting_show() {
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

    /// The agent panel and the content each keep their floor, at any window
    /// that can seat both.
    #[test]
    fn the_split_keeps_both_floors() {
        let split = agent_split();
        for room in [AGENT_MIN + CONTENT_MIN + 1.0, 1280.0, 1920.0, 3200.0] {
            let (agent, content) = split.resolve(room);
            assert!(agent >= AGENT_MIN - 0.001, "agent {agent} at {room}");
            assert!(
                content >= CONTENT_MIN - 0.001,
                "content {content} at {room}"
            );
        }
    }

    /// A corner with no lights in it (a macOS fullscreen space) is given back.
    #[test]
    fn a_corner_without_lights_gives_its_room_back() {
        for anchored in [false, true] {
            let lead = |lights| chrome::band_insets(lights, anchored).0;
            assert!(lead(false) < lead(true));
            assert_eq!(
                chrome::band_insets(false, anchored).1,
                chrome::band_insets(true, anchored).1
            );
        }
    }

    /// Putting the anchors away gives their room back at both ends.
    #[test]
    fn a_band_without_anchors_gives_their_room_back() {
        for lights in [false, true] {
            let (left, right) = chrome::band_insets(lights, false);
            let (anchored_left, anchored_right) = chrome::band_insets(lights, true);
            assert!(left < anchored_left);
            assert!(right < anchored_right);
        }
    }
}
