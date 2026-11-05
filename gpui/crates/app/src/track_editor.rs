//! The track editor: one track's timeline, on a custom-painted canvas.
//!
//! A ruler, a rekordbox-style three-band waveform, the beat grid and bar
//! numbers, the lanes of clips over them, and the transport underneath. The
//! transport is the backend's `host_audio`, not the UI's.
//!
//! # The geometry rules are exact
//!
//! The canvas keeps a fixed set of rounding rules: the 6px beat cull compares
//! against the last *drawn* beat, downbeats are de-duplicated at millisecond
//! precision, bar labels step by `ceil(80 / pixelsPerBar)`, band bars are
//! `floor(low[i] * halfHeight)` and clips are at least 4px wide. Do not change
//! one of them casually. A different rounding draws a slightly different
//! timeline for the same track, and no error shows it.
//!
//! Coordinates are written as stroke centres, `floor(x) + 0.5`. gpui paints
//! quads, so [`hairline`] turns a centre back into the box the stroke covers.
//!
//! Both waveform strips query the same GPU peak hierarchy at every zoom. Audio
//! is filtered and uploaded once; navigation only changes the rendered range.
//!
//! # One working copy, one write
//!
//! Every gesture and every keyboard command edits [`Editor::clips`] — the
//! working copy — and nothing else. [`Luma::commit_clips`] publishes the whole
//! score in one write, compared against the document the seam last
//! confirmed.
//!
//! That is the reason there is no per-clip write here. A duplicate, a split, a
//! region delete or a paste each touch several clips at once, and the states
//! they pass through on the way — a clip deleted before its replacement
//! exists, a lane momentarily empty — are ones nobody asked for. Fanned out
//! into one call per clip they would be observable; as one candidate they are
//! not. The single-clip drag rides the same path, because a second write path
//! is a second set of failure modes for the same gesture.
//!
//! It is also what makes undo cheap. An edit here *is* a replacement of the
//! whole list, so its inverse is the list it replaced — [`History`] keeps
//! those and nothing else, and there is no per-command undo to write or to
//! get wrong.
//!
//! # Clip bodies are heatmap previews
//!
//! A clip's body carries its pattern's space-time heatmap, rendered through
//! `preview_score_clips` after each committed edit, coalesced per clip so a
//! burst of writes costs one trailing render. The seam hands back a tiny
//! RGBA grid (a column per sixteenth-beat, a row per primitive); the GPU
//! stretches it over the body at any zoom, a cell a block — see [`bake`](preview::bake) and
//! [`paint_preview`]. Previews are decoration: a track whose patterns
//! have no graphs simply keeps the flat translucent fill, and so does any
//! clip too narrow to read.
//!
//! An aim clip gives no light, so its body is curves instead: the pan and
//! tilt the solver sends each head, over the clip, as two bands. Heads that
//! move alike share one curve. The seam hands the curves back with the
//! preview; the painter only strokes them, at whatever width the body has —
//! see [`paint_aim`](preview::paint_aim).
//!
//! # The vertical bands, which are the whole pointer contract
//!
//! ```text
//!   0 .. 32     ruler        scrub the playhead
//!  32 .. 112    waveform     clear the selection — it does *not* scrub
//! 112 .. floor  lanes        clip headers grab; everything else sweeps
//! ```
//!
//! The lane block is **bottom-anchored** ([`Layout`]): z = 0 is pinned to the
//! floor of the canvas and new layers appear above what is already there, so
//! the layer everything is stacked over never moves under the eye. A stack
//! taller than the canvas therefore overflows *upward*, under the waveform,
//! and the three ways back to it are the bare wheel, the alt-wheel that sets
//! the lane height, and `H`, which picks the height the whole stack fits at.
//!
//! Only the top [`CLIP_HEADER`] pixels of a clip answer the pointer. Its body
//! is inert, and a press there sweeps a range like any other empty space.
//!
//! A clip is named in the automation tree by its *pattern*, so two clips of
//! one pattern are two nodes with one label, and the node's bounds are its
//! header bar rather than its drawn box — a script clicking the centre of the
//! drawn box would land in the inert body. Their edge handles are separate
//! nodes (`"<pattern> start"` / `"<pattern> end"`), which is what a script
//! drags to resize — the clip's own centre is nowhere near either edge.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::node::{agent_paint_node, Instrument, Role};
use luma_ui::Enabled;
use luma_ui::{float, ladder, paint};

use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::node_graph::{BeatGrid, BlendMode};
use luma_lib::models::patterns::{AimCurves, AnnotationPreview};
use luma_lib::models::tracks::{BeatValidationReason, BeatValidationVerdict, TrackBrowserRow};
use luma_lib::models::waveforms::TrackWaveform;
use luma_lib::settings::ViewMode;

use crate::history::History;
use crate::shell::Body;
use crate::tabs::Target;
use crate::{LibraryError, Luma};

mod beat_grid;
mod canvas;
mod commands;
mod composite;
mod document;
mod draw;
mod fades;
mod input;
mod insertion;
mod lanes;
use lanes::assign_rows;
mod layout;
mod minimap;
mod navigation;
pub(crate) mod picker;
mod playback;
mod playback_clock;
mod playback_surface;
mod preview;
mod ruler;
mod screen;
pub(crate) mod sheet;
pub(crate) use sheet::Audition;
mod tab;
mod toolbar;
mod waveform;
mod zoom_motion;

use beat_grid::{
    bar_length, bars_after, mean_lengths, shift_time, snap, Grid, SNAP_CAPTURE, SNAP_CAPTURE_DRAG,
};
use canvas::{canvas_element, clip_bounds, Scene};
use commands::{clear_region, row_to_z, z_ladder, Clipboard, Snapshot, MIN_CLIP};
use composite::sync_composite;
use draw::{clip_body, fade, ghost_box, hairline, label, LOOP_BAND};
use input::{listen, Edge, Gesture, Initial};
use insertion::{InsertChoice, InsertMenu};
use layout::{
    lane_count, Layout, View, CLIP_HEADER, HANDLE, HEADER_HEIGHT, LABEL_SIZE, LANE_HEIGHT,
    TRACK_AREA_Y, WAVEFORM_HEIGHT,
};
use navigation::{Anchor, Wheel};
use preview::{paint_preview, Installed, Preview, BODY_ALPHA};
use ruler::{paint_beat_grid, paint_time_ruler};
pub use screen::track_editor;
pub(crate) use screen::{clip_graph_page, fullscreen_transport, inspector};
pub(crate) use toolbar::clock;
use toolbar::toolbar;

// -- state --------------------------------------------------------------------

/// The screen's whole state: the track it is showing, everything the seam
/// returned for it, where the eye is, and where the transport is.
pub struct Editor {
    track_id: String,
    track_name: String,
    /// The venue the score belongs to.
    venue_id: String,
    /// The score whose clips are on the timeline, and whether this host may
    /// write to it. Fixed for the tab's life: the score is the tab's identity
    /// (see [`Editor::target`]), so another score is another tab.
    score: Score,
    /// The tab's chat: this score's conversations.
    pub(crate) chat: crate::agent::TabChat,
    waveform: Option<Rc<TrackWaveform>>,
    gpu_waveform: Rc<RefCell<waveform::Resource>>,
    timeline_waveform: waveform::Strip,
    overview_waveform: waveform::Strip,
    minimap_drag: Option<minimap::Drag>,
    /// The analysed grid, or `None` for a track that has not been analysed —
    /// in which case the header falls back to a clock ruler.
    beats: Option<Rc<BeatGrid>>,
    beat_verdict: BeatValidationVerdict,
    beat_reason: Option<BeatValidationReason>,
    beat_reason_menu: luma_ui::float::MenuVisibility,
    beat_validation_pending: bool,
    beat_validation_error: Option<String>,
    /// The **working copy**: every clip as the screen currently has it, with
    /// its lane resolved. Rebuilt whenever the clips change and never during a
    /// draw. Rows depend on every clip's layer priority, so rebuilding them
    /// per frame would repeat work for unchanged clips.
    ///
    /// Every gesture and every command edits this list and nothing else;
    /// [`Luma::commit_clips`] is the only thing that writes, and it writes the
    /// whole list's changes at once.
    clips: Rc<[Clip]>,
    /// The document as last read or saved: what a save's changes are taken
    /// against.
    graph_score: Option<luma_patterns::Score>,
    /// Previews by clip id, shared with the frame — each entry's `published`
    /// flag is written at paint time, which is why the map
    /// sits behind the same interior-mutability arrangement as
    /// [`Self::canvas`].
    previews: Rc<RefCell<HashMap<SharedString, Installed>>>,
    /// Clips whose single-clip preview render is in flight.
    preview_inflight: HashSet<SharedString>,
    preview_errors: HashMap<SharedString, String>,
    /// Clips edited again while their render was in flight. Re-issued when it
    /// lands — the render reads the clip's current state at issue, so one
    /// trailing render covers everything a burst of edits did.
    preview_queued: HashSet<SharedString>,
    /// Where the timeline has been, and where an undo took it back from.
    history: History<Snapshot>,
    /// The last cut or copy, in the shape a paste needs. Local to the screen.
    clipboard: Option<Clipboard>,
    /// The span the transport is looping, if any. A property of playback and
    /// not of the score — it is never written back, and a read-only score can
    /// still be looped over.
    loop_region: Option<(f64, f64)>,
    /// The target and keyboard cursor of the open insertion dialog.
    menu: Option<InsertMenu>,
    menu_query: String,
    menu_search: Entity<luma_ui::text_input::TextInput>,
    _menu_subscription: Subscription,
    /// The menu's list scroll, so the keyboard cursor can pull an off-screen
    /// pattern into view — a cursor past the clip edge otherwise commits a row
    /// the user cannot see.
    menu_scroll: ScrollHandle,
    /// Every selected clip, in the order they were added. A list rather than
    /// one id because shift-click, marquee and group drag all act on
    /// a set, and "one selected clip" is only the common case of that.
    selected: Vec<SharedString>,
    /// In Direct, the selected clip's graph is up where the timeline was —
    /// see [`clip_graph_page`]. Meaningless with nothing selected, which
    /// [`Editor::clip_graph_shown`] folds in.
    graph_open: bool,
    /// Preserve the first hit while opening the inspector changes the canvas.
    pressed_clip: Option<(SharedString, Point<Pixels>)>,
    /// Where the next edit lands. Distinct from the selection: clicking a clip
    /// sets both, sweeping empty lane space sets only this.
    cursor: Option<Cursor>,
    /// Follow the playhead: keep it centred while the transport runs. Not
    /// persisted, because this host has no per-screen preference store yet.
    follow: bool,
    view: View,
    transport: Transport,
    playback_surface: Option<Entity<playback_surface::Surface>>,
    gesture: Option<Gesture>,
    /// The part of an alpha line the pointer is over, for the cursor.
    alpha_hover: Option<fades::Part>,
    /// The latched zoom anchor: what was under the pointer when the gesture
    /// started, and when it was last fed. Held for the whole gesture so a
    /// momentum flick cannot walk the point it is zooming about.
    anchor: Option<Anchor>,
    zoom_motion: Option<zoom_motion::Zoom>,
    /// The seek the throttle is holding back, and when the last one went out.
    /// A scrub writes the playhead every move and the transport at most once
    /// per [`SEEK_THROTTLE`](input::SEEK_THROTTLE) — the picture is free, the IPC is not.
    seek_pending: Option<f32>,
    seek_at: Option<std::time::Instant>,
    /// Where the canvas last painted, in window space. A mouse event arrives
    /// in window coordinates and has to be put back into timeline coordinates,
    /// which needs this; only `prepaint` knows it, and a `Cell` is how it gets
    /// written down there without notifying from inside a draw.
    canvas: Rc<Cell<Bounds<Pixels>>>,
    /// The working copy has moved away from the saved document and owes a
    /// write. A flag rather than a queue of edits: a write sends everything
    /// that differs from the saved base, so there is only ever one thing
    /// outstanding.
    dirty: bool,
    /// A write is in flight. Serialized rather than concurrent: two writes
    /// against one base in the air at once would land in an order nobody
    /// chose.
    saving: bool,
    /// Writes that have come back. A reload read that saw this change under
    /// it may hold the document from before our own write.
    writes: u64,
    error: Option<String>,
    /// Whether the screen's first load has finished. Written in one assignment
    /// with the data, so "still loading" and "nothing here" cannot be confused.
    loaded: bool,
    /// The clip list the render engine's installed scene was compiled from.
    ///
    /// The rig is lit by a *scene*, and installing one is a command — so an
    /// edit that never re-composited is an edit the visualizer cannot show,
    /// however faithfully the timeline draws it. Holding what was sent makes
    /// "is the rig behind the working copy" a comparison rather than a guess,
    /// and lets a repaint that changed nothing cost nothing. `None` until the
    /// first load lands.
    composited: Option<Rc<[Clip]>>,
    /// A composite is in flight. One at a time, for the reason every other
    /// in-flight flag here exists: two installs racing would leave whichever
    /// landed later lighting the rig, and that is not necessarily the later
    /// edit. The reconcile re-issues from what it sees when this clears.
    compositing: bool,
    /// The clip controls in the editing area — see [`sheet`].
    sheet: sheet::State,
}

/// The score being edited, how it is spoken about, and whether this host owns
/// it.
///
/// Minted by the sidebar's scores level ([`crate::tracks::scores`]), which is
/// the one place a listing row becomes an open document.
#[derive(Clone)]
pub(crate) struct Score {
    pub(crate) id: String,
    /// Its display ordinal within `(track, venue)` — the `#2` the sidebar and
    /// the toolbar name it by. A position, not identity: [`Self::id`] is the
    /// key.
    pub(crate) ordinal: i64,
    /// Somebody else's score: visible, not writable.
    pub(crate) read_only: bool,
}

/// One clip, with everything a draw *and* a write need already resolved.
///
/// A superset of the authored [`luma_patterns::Clip`] rather than a projection
/// of it: a gesture that creates, splits or restacks clips has to hand the
/// seam a complete row, and a screen that kept only what it drew would have to
/// go and re-read the rest.
#[derive(Clone)]
struct Clip {
    id: SharedString,
    /// The kind of the graph's output node: `color`, `aim` or `strobe`.
    output: SharedString,
    /// The clip's name, or its output kind while it has none.
    label: SharedString,
    /// The graph's one-line summary, drawn after the name.
    summary: SharedString,
    color: Rgba,
    start: f64,
    end: f64,
    /// Which lane it sits in, counting down from the empty insertion lane at
    /// row 0. One row is one lighting priority — see [`lanes`].
    row: usize,
    z: i64,
    blend: BlendMode,
    core: Option<luma_patterns::Clip>,
}

impl Clip {
    /// A copy of this clip at a new span, under a fresh local id.
    fn copy(&self, start: f64, end: f64, z: i64) -> Self {
        Self {
            id: format!("new:{}", uuid::Uuid::new_v4()).into(),
            start,
            end,
            z,
            ..self.clone()
        }
    }

    /// Read the pictures of the clip — output, label, summary, color — off
    /// its authored body again, after an edit to it.
    fn refresh(&mut self) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let output = core
            .graph
            .output_kind()
            .unwrap_or(luma_patterns::clip_graph::Kind::Color);
        self.output = output.name().into();
        self.label = if core.name.is_empty() {
            output.label().into()
        } else {
            core.name.clone().into()
        };
        self.summary = core.graph.summary().into();
        self.color = ladder::pattern(output.name());
    }
}

/// The selection cursor: a point in time, or a rectangle of time × lanes.
///
/// `start` and `end` are stored as the gesture produced them, not normalised,
/// because a right-to-left sweep is a real cursor and every reader takes its
/// own min and max. Normalising here would quietly change which end a later
/// edit anchors to.
#[derive(Clone, Copy)]
struct Cursor {
    row: usize,
    row_end: Option<usize>,
    start: f64,
    end: Option<f64>,
}

impl Cursor {
    /// The time range, lowest first, or `None` for a point cursor. A sweep
    /// that ends where it began — straight down the lanes, or snapped back
    /// to its start — is a point too: a line over its lane band, never a
    /// range that holds no time.
    fn span(self) -> Option<(f64, f64)> {
        self.end
            .filter(|&end| end != self.start)
            .map(|end| (self.start.min(end), self.start.max(end)))
    }

    /// The lane band, lowest first. A point cursor is one lane unless a
    /// vertical sweep gave it a band.
    fn rows(self) -> (usize, usize) {
        let end = self.row_end.unwrap_or(self.row);
        (self.row.min(end), self.row.max(end))
    }
}

/// Audio-host state plus a display clock for motion between host snapshots.
/// The host corrects position; animation frames advance the visible playhead.
#[derive(Default)]
struct Transport {
    clock: playback_clock::Clock,
    playing: bool,
    position: f32,
    duration: f32,
    session: Option<u64>,
    ready: bool,
    polling: Option<gpui::Task<()>>,
    status_second: u64,
}

/// One step of a transport change. Boxed because a play is two commands and a
/// pause is one, and the two arms of that choice are different opaque future
/// types that only a `dyn` can hold in one list.
pub(crate) type Transition =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LibraryError>>>>;

/// How often the playhead is re-read while playing. 30 Hz — twice the rate the
/// desktop app's broadcaster emits at, because a poll's phase is arbitrary and
/// halving the period halves the worst-case lag.
const POLL: Duration = Duration::from_millis(33);

impl Editor {
    /// The tab's chip: the track, and which of its scores. Two scores of one
    /// track are two tabs, so the track alone would not tell them apart.
    pub(crate) fn title(&self) -> String {
        format!("{} · #{}", self.track_name, self.score.ordinal)
    }

    /// What an export of the open score takes from the timeline: its name,
    /// its length in seconds, and the score as the stage lights it — the
    /// working copy, unsaved edits included. `None` before the track's length
    /// is known.
    pub(crate) fn export_source(&self) -> Option<crate::export_dialog::Source> {
        let duration = self.transport.duration;
        (duration > 0.).then(|| crate::export_dialog::Source {
            track_name: self.track_name.clone(),
            duration,
            score_id: self.score.id.clone(),
            score: self.graph_candidate().ok(),
        })
    }

    /// Whether an edit is allowed to land at all: a score this host owns, and
    /// no other reason to refuse.
    fn writable(&self) -> bool {
        !self.score.read_only
    }

    /// The tab this editor is: its score, in its venue.
    pub(crate) fn target(&self) -> Target {
        Target::Score {
            venue: self.venue_id.clone(),
            track: self.track_id.clone(),
            score: self.score.id.clone(),
        }
    }

    /// The timeline as a chat message reports it: the playhead, the cursor,
    /// and the clips they touch. Read once per message sent.
    pub(crate) fn agent_context(&self) -> luma_lib::agent::EditorState {
        let cursor = self.cursor.map(|cursor| {
            let (first_lane, last_lane) = cursor.rows();
            let (start, end) = match cursor.span() {
                Some((start, end)) => (start, Some(end)),
                None => (cursor.start, None),
            };
            luma_lib::agent::CursorSpan {
                start,
                end,
                first_lane,
                last_lane,
            }
        });
        let clips = self.clips.iter().map(|clip| luma_lib::agent::ClipSpan {
            id: clip.id.to_string(),
            name: clip.label.to_string(),
            lane: clip.row,
            z: clip.z,
            start: clip.start,
            end: clip.end,
            selected: self.selected.contains(&clip.id),
        });
        luma_lib::agent::EditorState::capture(
            f64::from(self.transport.position),
            cursor,
            clips,
            self.beats.as_deref(),
        )
    }

    /// The room this timeline is being worked on in.
    pub(crate) fn venue_id(&self) -> &str {
        &self.venue_id
    }

    /// The score the stage above this timeline should be lit by — the open
    /// document, which is this screen's own fact and nobody else's to derive.
    pub(crate) fn lit(&self) -> crate::visualizer::Lit {
        crate::visualizer::Lit {
            score: self.score.id.clone(),
            ordinal: self.score.ordinal,
        }
    }

    /// The track this timeline edits.
    pub(crate) fn track_id(&self) -> &str {
        &self.track_id
    }

    /// Take a new working copy: re-derive every lane, and mark the score as
    /// owing a write.
    ///
    /// **The only way the clip list changes.** Every gesture and every command
    /// funnels through here, which is what makes "a lane is a function of
    /// every clip's priority" a fact rather than a convention.
    /// This also keeps
    /// [`Editor::dirty`] from being something a caller can forget to set.
    fn replace_clips(&mut self, mut clips: Vec<Clip>) {
        assign_rows(&mut clips);
        self.clips = clips.into();
        self.dirty = true;
    }

    /// Clear both the selection and the cursor: what a press on anything that
    /// is not a clip does.
    fn deselect(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.graph_open = false;
    }

    /// Whether Direct shows the selected clip's graph instead of the timeline.
    ///
    /// A selection the graph was opened on can also be emptied from elsewhere
    /// — an undo, the agent deleting the clip — and then the timeline is what
    /// is left to show. The page closes for good then, rather than waiting to
    /// spring back on the next clip a click selects.
    pub(crate) fn clip_graph_shown(&mut self) -> bool {
        if self.selected.is_empty() {
            self.graph_open = false;
        }
        self.graph_open
    }
}

#[cfg(test)]
mod tests {
    use super::Cursor;

    #[test]
    fn a_vertical_sweep_is_a_line_over_its_lanes() {
        let sweep = |end: f64| Cursor {
            row: 1,
            row_end: Some(3),
            start: 4.,
            end: Some(end),
        };
        assert_eq!(sweep(4.).span(), None);
        assert_eq!(sweep(4.).rows(), (1, 3));
        assert_eq!(sweep(2.).span(), Some((2., 4.)));
    }
}
