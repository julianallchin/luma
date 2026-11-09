//! Luma, natively.
//!
//! A GPUI host over the same command surface the desktop app runs on: this
//! crate reads the real library through [`luma_lib::dispatch`] and renders it
//! with the design system in `luma-ui`.
//!
//! # Shape
//!
//! ```text
//! main         window + chrome, one root entity      (main.rs)
//!  └ Luma      the persistent shell, one open venue  (this file + shell.rs)
//!     ├ agent      the chat, full height on the left (luma-chat, agent.rs)
//!     ├ title bar  venue / song crumbs · modes       (chrome.rs, welcome.rs,
//!     │             each crumb a menu                 score_switcher.rs)
//!     ├ content    the stage over the mode's body:   (shell.rs, workspace.rs)
//!     │             Rig, Arrange or Live
//!     ├ song list  the venue's songs, over Arrange   (tracks.rs)
//!     ├ status bar sync, output, the mode's line     (status_bar.rs)
//!     ├ settings   a page in place of the content    (settings.rs)
//!     ├ overlay    dialogs: add tracks, venues, …    (shell.rs)
//!     └ library    the only door to Luma's data
//! ```
//!
//! The view tree is a library rather than a binary's private module so that
//! `gpui-agent` can host the same [`Luma`] under a test platform. There is one
//! app: a harness that rebuilt these screens would be testing itself.
//!
//! There is no router and no Back: the shell is persistent, opening one thing
//! never destroys another, and each gesture's transition lives with the module
//! it belongs to (see `settings::open_settings`), so this file stays the list
//! of what exists. `docs/specs/comet-shell.md` is the contract.

// The hitch report's self-describing schema is one `serde_json::json!`
// literal, and it grows a line every time the instrument learns to see
// something new. Each key costs a macro recursion; the default 128 is reached
// well before the schema stops being worth extending.
#![recursion_limit = "256"]

use gpui::prelude::FluentBuilder as _;
use gpui::*;

mod add_tracks;
mod agent;
mod chat_history;
mod chrome;
mod confirm;
mod export_dialog;
mod failure;
mod fixture_library;
mod fixture_picker;
mod fullscreen;
mod history;
mod keymap;
mod library;
mod patch;
mod perform;
mod picker_preview;
mod saved_cameras;
mod saved_venues;
mod score_switcher;
mod session_store;
mod settings;
mod shell;
mod shortcuts;
mod signin;
mod stage;
mod status_bar;
mod subagents;
mod sync_status;
mod track_editor;
mod tracks;
mod update;
mod visualizer;
mod welcome;
mod workspace;

pub use chrome::traffic_light_position;
#[cfg(feature = "agent")]
pub use library::NavigationFixture;
pub use library::{
    Account, Library, LibraryError, SourceLibrary, SourcePlaylist, SourceTrack, TrackImportRequest,
    TrackSource,
};
#[cfg(feature = "agent")]
pub use library::{SourceAdapterFixture, SourceSearchFixtureResponse};
pub use luma_lib::models::tracks::{TrackImportPhase, TrackImportProgress, TrackImportResult};
pub use visualizer::warm_renderer;

/// Everything the app's views need present in an `App` before a window opens:
/// gpui-component's theme (every `Icon` reads it), Inter (not a system font,
/// so without it the text system silently picks another face), and the
/// keymap.
///
/// Both hosts call this — the real binary and the automation harness — so a
/// screen cannot render or answer a key differently depending on who opened
/// the window.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    fonts::install(cx);
    keymap::init(cx);
    // Both halves of the text-field keymap, and they must be bound together:
    // the app's bindings exclude `TEXT_INPUT`, and the field's own supply the
    // editing keys that exclusion leaves it. Registering one without the other
    // gives a field that either eats the app's shortcuts or cannot be typed in.
    text_input::init(cx);
    keymap::init_field_overrides(cx);
    motion::init(cx);
}
use luma_chat::RunningTurns;
use luma_ui::{fonts, ladder, motion, text_input};

use shell::{Body, FocusSlot, Overlay};
use workspace::Modes;

pub struct Luma {
    pub(crate) library: Library,
    pub(crate) track_import: Option<add_tracks::TrackImportActivity>,
    pub(crate) next_track_import: u64,
    /// The add-track dialog's audio preview. Owned here rather than by the
    /// dialog so that however the dialog goes away — closed, replaced, or
    /// dropped outright under reduced motion — the shell still sees a
    /// preview with no dialog and stops it (see `add_tracks::reap_preview`).
    pub(crate) source_preview: Option<add_tracks::SourcePreview>,
    /// Decoded covers for source-browser rows, kept across openings of the
    /// dialog and bounded — see [`add_tracks::ArtCache`].
    pub(crate) source_art: add_tracks::ArtCache,
    /// The open venue's songs — the song list page's state. `None` while no
    /// venue is open, which is also when the venue picker keeps itself open:
    /// the two states are one fact read twice.
    pub(crate) songs: Option<tracks::Tracks>,
    /// Whether the song list page is up over Arrange. Arrange with no score
    /// open shows it whatever this says — see [`Luma::song_list_shown`].
    pub(crate) song_list: bool,
    /// A score click's pending close of the song list — see
    /// [`Luma::open_listed_score`]. Dropping it cancels the close.
    pub(crate) song_list_close: Option<gpui::Task<()>>,
    /// Whether the agent panel is put away by hand: the remembered state
    /// every page that has the panel shows it in. A page without one (the
    /// song list) leaves it alone — see [`Luma::page_has_agent`].
    pub(crate) agent_hidden: bool,
    /// The next frame slides the panel to its new width: set only by the
    /// person's own toggle. Every other change of width — arriving on a page
    /// with or without the panel, the window narrowing past it — lands at
    /// once.
    pub(crate) agent_slide: bool,
    /// The window's parts drawn as views of their own, so the stage's frames
    /// rebuild only the stage — see [`shell::Region`].
    pub(crate) regions: shell::Regions,
    /// The agent panel's live width — the slide
    /// [`agent_hidden`](Self::agent_hidden) asks for. Intent and geometry are
    /// kept apart because only one of them is true mid-slide: the flag says
    /// where the panel is going, this says where it is.
    pub(crate) agent_width: luma_ui::pane::PaneWidth,
    /// The open venue's modes and their bodies — **the state on screen**.
    /// What each body shows *is* its identity, see [`workspace`].
    pub(crate) workspace: Modes<Body>,
    /// Every other venue's. [`Luma::sync_venue_modes`] swaps them.
    pub(crate) venues: workspace::Venues<Body>,
    /// Each venue's mode and open score as this device last left them — see
    /// [`saved_venues`].
    pub(crate) saved_venues: saved_venues::SavedVenues,
    pub(crate) saved_cameras: saved_cameras::SavedCameras,
    /// Live's deck panel width as last dragged on this device; `None` is the
    /// default — see [`shell::DeckWidth`].
    pub(crate) deck_width: shell::DeckWidth,
    /// This machine's rebound shortcuts — see [`keymap::Overrides`].
    pub(crate) shortcuts: session_store::Stored<keymap::Overrides>,
    /// The title bar's venue menu while it is open or leaving — see
    /// [`welcome::VenueMenu`].
    pub(crate) venue_menu: luma_ui::dialog::Popup<welcome::VenueMenu>,
    /// The title bar's song / score menu in Arrange while it is open or leaving —
    /// see [`score_switcher`].
    pub(crate) score_switcher: luma_ui::dialog::Popup<score_switcher::ScoreSwitcher>,
    /// The settings page while it is up, in place of the page's content —
    /// see [`settings`].
    pub(crate) settings: Option<settings::Settings>,
    pub(crate) shell_presented: bool,
    pub(crate) restoring_venue: bool,
    /// The stored session could not be refreshed for a reason other than a
    /// refusal — the network, most likely. The cause is in the log.
    pub(crate) session_refresh_failed: bool,
    /// How the window divides between the agent panel and the content. The
    /// panel's width is *derived* from it every frame rather than stored, so
    /// the two can never disagree. Session-lived: a split chosen for one
    /// window is not a preference about every future window.
    pub(crate) agent_split: luma_ui::split::SplitFraction,
    /// Device pixels per logical pixel, as of the last render. Offscreen
    /// previews are drawn at the size they are shown.
    pub(crate) scale_factor: f32,
    /// The stage above the editors, when the visible tab is about a room.
    ///
    /// Not a tab and not keyed like one: it is a *view of whatever is below
    /// it*, derived from the workspace every frame by
    /// [`Luma::sync_visualizer`]. `None` is both "no room to show" and the
    /// off switch for its redraw loop — see [`visualizer::visualizer`].
    pub(crate) visualizer: Option<visualizer::Visualizer>,
    pub(crate) fullscreen: Option<fullscreen::State>,
    pub(crate) visualizer_focus: FocusHandle,
    /// Whether the stage pane is suppressed by hand. Kept apart from
    /// [`visualizer`](Self::visualizer) the way `agent_hidden` is from the
    /// chat: one says what there is to show, the other whether the room for
    /// it was given away.
    pub(crate) visualizer_hidden: bool,
    /// How the content column divides between the stage and the mode below
    /// it: Arrange's timeline, and the other modes. Session-lived, like
    /// `agent_split`.
    pub(crate) score_editor_split: luma_ui::split::SplitFraction,
    pub(crate) visualizer_split: luma_ui::split::SplitFraction,
    /// The sign-in screen, or none. **Not an overlay**: while it is up it is
    /// the app's whole content and the shell is not rendered at all — see
    /// [`signin`]. Boxed because it carries a morph, two text fields and their
    /// subscriptions, and `Luma` is one entity that would otherwise carry all
    /// of that for the entire session it is not showing.
    pub(crate) sign_in: Option<Box<signin::SignIn>>,
    /// A lapsed session is being asked online whether it still proves anyone;
    /// the window shows [`signin::splash`] until it answers.
    pub(crate) refreshing_session: bool,
    pub(crate) sync_status: sync_status::SyncState,
    /// The status bar's line about something just done that did not land —
    /// see [`failure`].
    pub(crate) failure: failure::Failure,
    /// The self-updater and what it last said — see [`update`].
    pub(crate) update: update::AutoUpdate,
    /// The one plane over the regions, or none — see [`shell::Overlay`].
    /// The dialog on screen, and — for the frames after it is dismissed —
    /// the one leaving. See [`luma_ui::dialog::Popup`]: gpui unmounts an
    /// element the frame its state drops, so an exit animation needs the state
    /// held alive while it plays.
    pub(crate) overlay: luma_ui::dialog::Popup<Overlay>,
    /// What the one sign-out gesture is doing — see [`settings::AccountAction`].
    /// Who is signed in is not here: `Library` is the cache of that.
    pub(crate) account_action: settings::AccountAction,
    /// Every tab's chat runs its turns here, so a turn outlives the tab being
    /// in front. Dropping it at quit cancels every turn.
    pub(crate) running: Entity<RunningTurns>,
    /// The app hears the registry: a start or end repaints the status dots,
    /// and a commit reloads the documents it changed — see
    /// [`Luma::running_event`].
    _running: [gpui::Subscription; 2],
    /// The keyboard's home: one handle, tracked at whichever element
    /// [`Luma::focus_slot`] names this frame, so actions always have a
    /// dispatch path and a binding can scope to the region it runs through.
    pub(crate) focus: FocusHandle,
    /// The modal plane's own focus target. Keeping this distinct from the
    /// shell handle lets dismissal return to the exact field/button that
    /// opened the dialog instead of merely focusing its containing region.
    pub(crate) dialog_focus: FocusHandle,
    /// Stable handle for the first reachable dialog control. Besides making
    /// initial traversal deterministic, this lets the harness prove the trap
    /// moves and wraps rather than merely retaining the container focus.
    pub(crate) dialog_first_focus: FocusHandle,
    pub(crate) dialog_last_focus: FocusHandle,
    pub(crate) overlay_return_focus: Option<(FocusSlot, WeakFocusHandle)>,
    /// Which slot [`Self::focus`] was last taken for. A change of subject has
    /// to take the keyboard back; a field the user clicked into keeps it
    /// otherwise.
    pub(crate) focused_slot: FocusSlot,
    /// Correlates venue catalogue reloads and startup restoration. An answer
    /// belongs to the picker instance that requested it, never merely to
    /// whichever venue overlay happens to be visible when it lands.
    pub(crate) venue_picker_generation: u64,
    /// Agent commits invalidate parked tabs too; they reload when their venue
    /// comes back on screen.
    pub(crate) agent_stale: Vec<crate::workspace::Target>,
    /// Correlates a history read with the dialog that asked for it — a slow
    /// list arriving after the reader reopened the picker is a stale answer.
    pub(crate) chat_history_generation: u64,
    /// Correlates a sign-in round trip with the gate that made it. A code
    /// verified after the user chose to work offline is a stale answer.
    pub(crate) sign_in_generation: u64,
    /// Correlates per-venue track reads. Venue identity is checked as well;
    /// the generation distinguishes reopening the same venue twice.
    pub(crate) venue_selection_generation: u64,
    /// The `hdr_output` setting, as last read. The window applies it on each
    /// render (see [`Luma::render`]); it has an effect only on a display that
    /// accepts HDR.
    pub(crate) hdr_output_allowed: bool,
    /// The `dmx_output_enabled` setting, as last read: what the status bar
    /// says about output.
    pub(crate) dmx_output: bool,
}

impl Luma {
    /// The shell opens empty — no venue, no tabs — and the venue picker
    /// overlay opens itself over it. Every subject is reached by pressing
    /// something, which is what keeps "how did I get here" answerable from
    /// the click history alone.
    pub fn new(library: Library, cx: &mut Context<Self>) -> Self {
        let running = cx.new(|_| RunningTurns::default());
        let running_changed = cx.observe(&running, |_, _, cx| cx.notify());
        let running_events = cx.subscribe(&running, Self::running_event);
        let mut app = Self {
            library,
            track_import: None,
            source_preview: None,
            source_art: add_tracks::ArtCache::default(),
            next_track_import: 0,
            songs: None,
            song_list: false,
            song_list_close: None,
            agent_hidden: false,
            agent_slide: false,
            regions: shell::Regions::default(),
            // The first shell frame resolves the width without animation;
            // later visibility changes use the shared spring.
            agent_width: luma_ui::pane::PaneWidth::new(0.0),
            workspace: Modes::default(),
            venues: workspace::Venues::default(),
            saved_venues: saved_venues::SavedVenues::default(),
            saved_cameras: saved_cameras::empty(),
            deck_width: session_store::Stored::new(shell::DECK_WIDTH_KEY),
            shortcuts: session_store::Stored::new(keymap::OVERRIDES_KEY),
            venue_menu: luma_ui::dialog::Popup::default(),
            score_switcher: luma_ui::dialog::Popup::default(),
            settings: None,
            shell_presented: false,
            restoring_venue: false,
            session_refresh_failed: false,
            agent_split: shell::agent_split(),
            scale_factor: 1.0,
            visualizer: None,
            fullscreen: None,
            visualizer_focus: cx.focus_handle(),
            visualizer_hidden: false,
            visualizer_split: shell::visualizer_split(),
            score_editor_split: luma_ui::split::SplitFraction::new(0.65, 300., 240.),
            sign_in: None,
            refreshing_session: false,
            sync_status: sync_status::SyncState::default(),
            failure: failure::Failure::default(),
            update: update::AutoUpdate::new(),
            overlay: luma_ui::dialog::Popup::default(),
            account_action: settings::AccountAction::default(),
            running,
            _running: [running_changed, running_events],
            chat_history_generation: 0,
            focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            // Explicit tracked handles keep their own tab-stop bit in current
            // GPUI; the element's `.tab_stop(true)` only decorates implicitly
            // created handles. Mark these stable sentinels at their owner.
            dialog_first_focus: cx.focus_handle().tab_stop(true),
            dialog_last_focus: cx.focus_handle().tab_stop(true),
            overlay_return_focus: None,
            focused_slot: FocusSlot::Shell,
            venue_picker_generation: 0,
            agent_stale: Vec::new(),
            sign_in_generation: 0,
            venue_selection_generation: 0,
            hdr_output_allowed: true,
            dmx_output: false,
        };
        // Which door the app opens on. Nothing opens without a principal: a
        // stored session that still proves one goes straight in; one that
        // stopped proving anyone (see `Library::lapsed`) is asked online
        // whether it can be made to, behind a splash; anything else is the
        // gate. The gate is a first screen, not a failure, and it lands on
        // exactly what the signed-in branch opens.
        if app.library.user_id().is_some() {
            app.restore_venue(cx);
        } else if app.library.lapsed().is_some() {
            app.refresh_session(cx);
        } else {
            app.show_sign_in(false, cx);
        }
        app.load_display_settings(cx);
        app.load_shortcuts(cx);
        app.watch_session(cx);
        app.watch_sync(cx);
        app.watch_replica(cx);
        app.watch_updates(cx);
        app
    }

    /// Raise the gate the moment the backend learns the stored session is
    /// gone for good. The sync loop is what finds out — a refresh refused as
    /// spent or revoked — and nothing it could retry would change the answer,
    /// so the only door left is a new sign-in, shown as an expiry.
    fn watch_session(&mut self, cx: &mut Context<Self>) {
        let mut revoked = self.library.session_revoked();
        cx.spawn(async move |this, cx| {
            while revoked.recv().await.is_ok() {
                if this
                    .update(cx, |this, cx| this.show_sign_in(true, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// Reload what is on screen when rows arrive from another device.
    ///
    /// Every surface in this app was read once, into memory, from rows. A
    /// download rewrites those rows behind it with nothing on this device
    /// noticing — so the service says which tables moved and the shell reads
    /// them again.
    fn watch_replica(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.library.replica_changed();
        cx.spawn(async move |this, cx| loop {
            let tables = match changed.recv().await {
                Ok(tables) => tables,
                // Lagged: some notifications were dropped, which means *more*
                // changed, not less. Reload on the next one rather than
                // reloading on a guess.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            if this
                .update(cx, |this, cx| this.replica_changed(&tables, cx))
                .is_err()
            {
                return;
            }
        })
        .detach();
    }

    /// Whether the page on screen has an agent panel at all. Rig, Live and
    /// Arrange's score do; the song list and settings are pages without one —
    /// the panel is not put away there, it is not part of them.
    pub(crate) fn page_has_agent(&self) -> bool {
        self.settings.is_none() && !self.song_list_shown()
    }

    /// Whether the agent panel should be up: a page that has one, and the
    /// panel not put away by hand.
    pub(crate) fn agent_wanted(&self) -> bool {
        self.page_has_agent() && !self.agent_hidden
    }

    /// Show or hide the agent panel, by its toggle or ⌘B — the one change of
    /// the panel that slides. Nothing to toggle on a page without one.
    pub(crate) fn toggle_agent(&mut self) {
        if self.page_has_agent() {
            self.agent_hidden = !self.agent_hidden;
            self.agent_slide = true;
        }
    }
}

impl Render for Luma {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.scale_factor = window.scale_factor();
        // Render is where the window is at hand. The call does nothing unless
        // the value changed; when it did, the frame this render builds is the
        // first one presented in the new mode.
        window.set_hdr_output_allowed(self.hdr_output_allowed);
        // Nobody is signed in yet, so there is no library to show and no shell
        // to show it in — the gate is the app, not a plane over it.
        if self.refreshing_session {
            return signin::splash(window, "Signing in…");
        }
        if self.session_refresh_failed {
            return signin::refresh_error(window, cx);
        }
        if self.restoring_venue {
            return signin::splash(window, "Opening your library…");
        }
        if self.sign_in.is_some() {
            return signin::screen(self, window, cx);
        }
        // A window with no venue *is* the venue picker: keep the overlay up
        // while no venue is open and nothing else is up.
        if self.songs.is_none() && self.overlay.get().is_none() {
            self.show_venues(cx);
        }
        self.sync_venue_modes(cx);
        self.see_front_body();
        self.save_venues(cx);
        self.sync_visualizer(cx);
        self.save_camera(cx);
        self.sync_fullscreen(window, cx);
        self.take_focus(window, cx);

        let root_holds_focus = matches!(self.focus_slot(), FocusSlot::Shell);
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            // The floor. Every region either leaves it showing (the thread
            // column, the workspace panel) or raises itself off it (the
            // agent panel) — see `shell::regions`. Opaque, and from the ladder
            // rather than the glass tier, because a plane whose brightness is
            // decided by the desktop behind the window has no rung.
            .bg(ladder::background())
            .font_family(fonts::FAMILY)
            .text_color(ladder::foreground())
            .when(root_holds_focus, |root| root.track_focus(&self.focus))
            // The app's verbs, listened for above every region: an action
            // dispatched at the focused element bubbles to here, and each
            // handler is a no-op wherever it does not apply.
            .key_context(keymap::context::ROOT)
            .on_action(
                cx.listener(|this, _: &keymap::ToggleVisualizerFullscreen, window, cx| {
                    this.toggle_visualizer_fullscreen(window, cx);
                }),
            )
            .on_action(cx.listener(
                |this, _: &keymap::DismissVisualizerFullscreen, window, cx| {
                    this.dismiss_fullscreen(window, cx);
                },
            ))
            .on_action(
                cx.listener(|this, _: &keymap::DismissOverlay, _, cx| this.dismiss_overlay(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::OpenSettings, _, cx| this.open_settings(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::ToggleVisualizer, _, cx| this.toggle_visualizer(cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::DuplicateSubtree, _, cx| this.stage_duplicate(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::CancelBuild, window, cx| {
                this.dismiss_overlay(cx);
                // Escape can land while the chooser's field holds focus — the
                // binding outruns the field's own key handler — and the field
                // unmounts with the dialog. Focus left on an unmounted element
                // eats every later keystroke, `A` included, until a click
                // reseats it; hand the keyboard back to the tab instead.
                let focus = this.focus.clone();
                window.focus(&focus, cx);
            }))
            .on_action(
                cx.listener(|this, _: &keymap::AddStageElement, window, cx| {
                    this.stage_open_chooser_focused(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &keymap::UndoStage, _, cx| this.stage_undo(cx)))
            .on_action(cx.listener(|this, _: &keymap::RedoStage, _, cx| this.stage_redo(cx)))
            .on_action(cx.listener(|this, _: &keymap::ZoomStageIn, _, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.zoom(crate::visualizer::ZOOM_IN, None);
                    this.notify_stage(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &keymap::ZoomStageOut, _, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.zoom(crate::visualizer::ZOOM_OUT, None);
                    this.notify_stage(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &keymap::ExportStageCamera, _, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.export_camera();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &keymap::FocusStageSelection, _, cx| {
                if let Some(state) = this.visualizer_mut() {
                    if state.focus_selection() {
                        cx.notify();
                    }
                }
            }))
            .on_action(
                cx.listener(|this, _: &keymap::DeleteStageElement, _, cx| this.stage_delete(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::GizmoTranslate, _, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.set_gizmo_mode(luma_scene::GizmoMode::Translate);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::GizmoRotate, _, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.set_gizmo_mode(luma_scene::GizmoMode::Rotate);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::ToggleAgent, _, cx| {
                this.toggle_agent();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::ShowRig, _, cx| {
                this.set_mode(workspace::Mode::Rig, cx)
            }))
            .on_action(cx.listener(|this, _: &keymap::ShowArrange, _, cx| {
                this.set_mode(workspace::Mode::Arrange, cx)
            }))
            .on_action(cx.listener(|this, _: &keymap::ShowLive, _, cx| {
                this.set_mode(workspace::Mode::Live, cx)
            }))
            .on_action(
                cx.listener(|this, _: &keymap::ToggleSongList, _, cx| this.toggle_song_list(cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::OpenClipGraph, _, cx| this.open_clip_graph(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::PlayPause, _, cx| this.toggle_playback(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::FollowPlayhead, _, cx| this.toggle_follow(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::UndoClips, _, cx| this.undo_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::RedoClips, _, cx| this.redo_clips(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::ToggleLoopRegion, _, cx| {
                    this.toggle_loop_region(cx)
                }),
            )
            .on_action(cx.listener(|this, _: &keymap::DeleteClips, _, cx| this.delete_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::SplitClips, _, cx| this.split_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::CopyClips, _, cx| this.copy_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::CutClips, _, cx| this.cut_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::PasteClips, _, cx| this.paste_clips(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::DuplicateClips, _, cx| this.duplicate_clips(cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::MoveClipsUp, _, cx| this.move_clips_lane(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::MoveClipsDown, _, cx| {
                    this.move_clips_lane(true, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &keymap::FitLanes, _, cx| this.fit_lanes(cx)))
            .on_action(cx.listener(|this, _: &keymap::NextInsertOption, _, cx| {
                this.step_insert_menu(true, cx)
            }))
            .on_action(cx.listener(|this, _: &keymap::PrevInsertOption, _, cx| {
                this.step_insert_menu(false, cx)
            }))
            .on_action(cx.listener(|this, _: &keymap::CommitInsertOption, _, cx| {
                this.commit_insert_menu(cx)
            }))
            .child(fullscreen::content(self, window, cx))
            .into_any_element();
        root
    }
}
