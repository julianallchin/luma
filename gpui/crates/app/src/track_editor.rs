//! The track editor: one track's timeline, on a custom-painted canvas.
//!
//! Mirrors `src/features/track-editor/` — the same ruler, the same
//! rekordbox-style three-band waveform, the same beat grid and bar numbers,
//! the same lanes of clips over it, and the same transport underneath, which
//! on both hosts is Rust's `host_audio` and not the UI's.
//!
//! # The geometry is not re-derived, it is ported
//!
//! Every pixel this canvas puts down is `timeline-drawing.ts`'s arithmetic:
//! the 6px beat cull that compares against the last *drawn* beat, the
//! millisecond-rounded downbeat de-duplication, `ceil(80 / pixelsPerBar)` bar
//! labels, `floor(low[i] * halfHeight)` band bars, `max(4, …)` clip widths.
//! Those are pinned by golden vectors on the web side
//! (`utils/timeline-drawing.golden.test.ts`), which is why they are copied
//! rather than reasoned out again — a port that rounded one of them the other
//! way would draw a subtly different timeline for the same track, and nothing
//! would say so.
//!
//! Canvas 2D strokes are centred on the path, which is why the web's
//! coordinates are all `floor(x) + 0.5`. gpui paints quads, so [`hairline`]
//! turns that back into the box the stroke actually covered; the pixels are
//! the same, the spelling is not.
//!
//! The waveform is where the two hosts deliberately part, at both ends of the
//! zoom.
//!
//! *Below* one pixel per stored bucket the web overdraws a dozen bucket bars
//! into one column and the last one painted wins, so what shows is an arbitrary
//! bucket rather than the run — which shimmers under scroll as the phase
//! shifts. [`columns`] folds the run to its envelope instead: same silhouette,
//! no shimmer, and a twelfth of the quads, which is what put the frame inside
//! budget.
//!
//! *Above* it, the web has nothing left to draw with — `get_track_waveform` is
//! 30 000 buckets however long the track is, and a five-minute track runs out
//! of them at 100 pixels a second — so it stretches each bucket over several
//! pixels and shows detail that was averaged away at import. This host asks the
//! seam to measure the visible range instead, at a bucket per pixel
//! ([`Fine`]), and paints that.
//!
//! Both arrive as the *same three band envelopes in the same units* — the seam
//! normalises every resolution of a track against one set of band gains — so
//! [`paint_waveform`] has one drawing routine and crossing the threshold shows
//! finer detail and nothing else. That is a contract, not a coincidence: a
//! measured bucket covers a narrower slice of audio than a stored one, so its
//! peak can only be lower, and the difference is troughs opening up between
//! peaks that stay where they were. Nothing about the picture — colour,
//! ceiling, shape — may depend on which source drew it.
//!
//! # One working copy, one write
//!
//! Every gesture and every keyboard command edits [`Editor::clips`] — the
//! working copy — and nothing else. [`Luma::commit_clips`] publishes the whole
//! list as a single compare-and-swap against [`Editor::base`], which is the
//! last thing the seam said was stored.
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
//! What is still missing is the minimap.
//!
//! # Clip bodies are heatmap previews
//!
//! A clip's body carries its pattern's space-time heatmap — the same picture
//! the web timeline stretches over `drawAnnotations` — read once per open
//! through `generate_annotation_previews` and patched one clip at a time
//! through `preview_annotation` after each committed edit, coalesced per clip
//! so a burst of writes costs one trailing render. The seam hands back a tiny
//! RGBA grid (a column per sixteenth-beat, a row per primitive); it is baked
//! once into a block-per-cell image the GPU stretches over the body at any
//! zoom — see [`bake`] and [`paint_preview`]. Previews are decoration: a track whose patterns
//! have no graphs simply keeps the flat translucent fill, and so does any
//! clip too narrow to read.
//!
//! # The vertical bands, which are the whole pointer contract
//!
//! ```text
//!   0 .. 32     ruler        scrub the playhead
//!  32 .. 112    waveform     clear the selection — it does *not* scrub
//! 112 .. lanes  dead air     clears too
//! lanes ..      lanes        clip headers grab; everything else sweeps
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

use luma_lib::dispatch::CommandError;
use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::node_graph::{BeatGrid, BlendMode};
use luma_lib::models::patterns::{AnnotationPreview, PatternSummary};
use luma_lib::models::scores::TrackScore;
use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::waveforms::{BandEnvelopes, TrackWaveform};
use luma_lib::services::track_edits::TrackClip;

use crate::history::History;
use crate::shell::Body;
use crate::tabs::Target;
use crate::{LibraryError, Luma};

mod sheet;

/// The subset ladder and its reading, shared with the fixture picker so the
/// two surfaces cannot offer different rungs.
pub(crate) use sheet::{subset_label, SUBSETS};

// -- state --------------------------------------------------------------------

/// The screen's whole state: the track it is showing, everything the seam
/// returned for it, where the eye is, and where the transport is.
pub struct Editor {
    track_id: String,
    track_name: String,
    /// The venue whose score is open. Half of what a conversation about this
    /// track is scoped by — see [`crate::agent::scope_for`].
    venue_id: String,
    /// The score whose clips are on the timeline, and whether this host may
    /// write to it. `None` until the lookup lands, and still `None` for a
    /// track this venue has never annotated — there is no score to edit then,
    /// and the web app shows the same empty lanes.
    score: Option<Score>,
    /// Whether anything has yet decided what this tab is showing — set by
    /// [`rebase`], which is the only thing that ever writes [`Self::score`].
    ///
    /// Not derivable from `score.is_none()`, and that is the whole point: "no
    /// score chosen yet" and "chosen to be on none, because the score was
    /// deleted" are different facts, and the tab's own opening read lands
    /// *after* both. Without this latch a delete raced the initial listing and
    /// the timeline reopened the document that had just been archived — with a
    /// listing taken before it was.
    score_chosen: bool,
    waveform: Option<Rc<TrackWaveform>>,
    /// The measured envelope of the range on screen, at a bucket per pixel.
    /// `None` until the zoom outruns the stored envelope and the seam answers
    /// — see [`Fine`].
    fine: Option<Rc<Fine>>,
    /// The window a measurement is in flight for. One at a time: a zoom
    /// gesture wants a different window every frame and only the one it
    /// settles on is worth having, so the current want is re-issued when a
    /// measurement lands rather than queued behind it.
    fine_pending: Option<Cut>,
    /// The analysed grid, or `None` for a track that has not been analysed —
    /// in which case the header falls back to a clock ruler, as on the web.
    beats: Option<Rc<BeatGrid>>,
    /// The **working copy**: every clip as the screen currently has it, with
    /// its lane resolved. Rebuilt whenever the clips change and never during a
    /// draw — a lane is a function of *every* clip's `zIndex`, so working it
    /// out per clip per frame would be quadratic in the one thing that grows.
    ///
    /// Every gesture and every command edits this list and nothing else;
    /// [`Luma::commit_clips`] is the only thing that writes, and it writes the
    /// whole list at once.
    clips: Rc<[Clip]>,
    /// The last list the seam said was stored, which is the compare-and-swap
    /// token [`Luma::commit_clips`] publishes against.
    ///
    /// A second field rather than a flag on the working copy because these are
    /// two different facts — what is on screen and what is on disk — and a
    /// write has to name both. It is also what makes "nothing changed" a
    /// comparison rather than a guess.
    base: Rc<[TrackClip]>,
    /// Every pattern in the library, by id: the clip labels, and what a
    /// right-click offers to insert.
    patterns: Rc<Vec<PatternSummary>>,
    /// Heatmap previews by clip id, shared with the frame — the resample
    /// cache inside each entry is written at paint time, which is why the map
    /// sits behind the same interior-mutability arrangement as
    /// [`Self::canvas`].
    previews: Rc<RefCell<HashMap<SharedString, Preview>>>,
    /// Clips whose single-clip preview render is in flight.
    preview_inflight: HashSet<SharedString>,
    /// Clips edited again while their render was in flight. Re-issued when it
    /// lands — the render reads the clip's current state at issue, so one
    /// trailing render covers everything a burst of edits did.
    preview_queued: HashSet<SharedString>,
    /// Where the timeline has been, and where an undo took it back from.
    history: History<Snapshot>,
    /// The last cut or copy, in the shape a paste needs. Local to the screen,
    /// as the web store's is.
    clipboard: Option<Clipboard>,
    /// The span the transport is looping, if any. A property of playback and
    /// not of the score — it is never written back, and a read-only score can
    /// still be looped over.
    loop_region: Option<(f64, f64)>,
    /// The open insertion menu, if a right-click put one up.
    menu: Option<InsertMenu>,
    /// The menu's list scroll, so the keyboard cursor can pull an off-screen
    /// pattern into view — a cursor past the clip edge otherwise commits a row
    /// the user cannot see.
    menu_scroll: ScrollHandle,
    /// Every selected clip, in the order they were added. A list rather than
    /// one id because the web's shift-click, marquee and group drag all act on
    /// a set, and "one selected clip" is only the common case of that.
    selected: Vec<SharedString>,
    /// Where the next edit lands. Distinct from the selection: clicking a clip
    /// sets both, sweeping empty lane space sets only this.
    cursor: Option<Cursor>,
    /// Follow the playhead: keep it centred while the transport runs. `F` on
    /// the web, and persisted there — not here, because this host has no
    /// per-screen preference store yet.
    follow: bool,
    view: View,
    transport: Transport,
    gesture: Option<Gesture>,
    /// The latched zoom anchor: what was under the pointer when the gesture
    /// started, and when it was last fed. Held for the whole gesture so a
    /// momentum flick cannot walk the point it is zooming about.
    anchor: Option<Anchor>,
    /// The seek the throttle is holding back, and when the last one went out.
    /// A scrub writes the playhead every move and the transport at most once
    /// per [`SEEK_THROTTLE`] — the picture is free, the IPC is not.
    seek_pending: Option<f32>,
    seek_at: Option<std::time::Instant>,
    /// Where the canvas last painted, in window space. A mouse event arrives
    /// in window coordinates and has to be put back into timeline coordinates,
    /// which needs this; only `prepaint` knows it, and a `Cell` is how it gets
    /// written down there without notifying from inside a draw.
    canvas: Rc<Cell<Bounds<Pixels>>>,
    /// The working copy has moved away from [`Self::base`] and owes a write.
    /// A flag rather than a queue of edits: the unit of writing is the whole
    /// list, so there is only ever one thing outstanding.
    dirty: bool,
    /// A write is in flight. Serialized rather than concurrent: a write is a
    /// compare-and-swap against [`Self::base`], and a second one issued
    /// against the same base would be refused by whichever landed later.
    saving: bool,
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
    /// The args inspector over the timeline's right edge — see [`sheet`].
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
    /// Somebody else's score: visible, not writable. The web browser computes
    /// the same flag from `score.uid !== currentUserId`.
    pub(crate) read_only: bool,
}

/// Point the editor at `score`, or at nothing, with none of the last one left
/// behind.
///
/// `composited` is reset with the rest: it records what was last *sent to the
/// rig*, and after a rebase the rig is holding another document's scene —
/// keeping the old value would make the next reconcile compare against a list
/// that is not on the timeline any more and skip the install.
fn rebase(editor: &mut Editor, score: Option<Score>) {
    editor.score = score;
    editor.score_chosen = true;
    editor.selected.clear();
    editor.cursor = None;
    editor.history = History::default();
    editor.clipboard = None;
    editor.clips = Vec::new().into();
    editor.base = Vec::new().into();
    editor.composited = None;
    editor.dirty = false;
}

/// One clip, with everything a draw *and* a write need already resolved.
///
/// A superset of [`TrackClip`] rather than a projection of it: a gesture that
/// creates, splits or restacks clips has to hand the seam a complete row, and
/// a screen that kept only what it drew would have to go and re-read the rest.
#[derive(Clone)]
struct Clip {
    id: SharedString,
    pattern: SharedString,
    /// The pattern's name, or the same `Pattern <id>` fallback the web label
    /// falls back to when the catalogue does not know it.
    label: SharedString,
    color: Rgba,
    start: f64,
    end: f64,
    /// Which lane it sits in, counting down from the empty insertion lane at
    /// row 0. Derived from every clip's `zIndex` together — see [`lanes`].
    row: usize,
    z: i64,
    blend: BlendMode,
    args: serde_json::Value,
}

impl Clip {
    /// This clip as the seam's own row.
    fn to_track_clip(&self) -> TrackClip {
        TrackClip {
            id: self.id.to_string(),
            pattern_id: self.pattern.to_string(),
            start_time: self.start,
            end_time: self.end,
            z_index: self.z,
            blend_mode: self.blend,
            args: self.args.clone(),
        }
    }

    /// A copy of this clip at a new span, under a fresh local id.
    ///
    /// The id is a `new:`-prefixed UUID, which is what tells
    /// [`Luma::commit_clips`] the row is a create: the seam allocates the real
    /// id and hands it back in `id_map`, so nothing here may assume one.
    fn copy(&self, start: f64, end: f64, z: i64) -> Self {
        Self {
            id: format!("new:{}", uuid::Uuid::new_v4()).into(),
            start,
            end,
            z,
            ..self.clone()
        }
    }
}

/// One clip's heatmap, baked for the GPU — see [`bake`].
struct Preview {
    /// Two frames under one identity — frame 0 at [`BODY_ALPHA`], frame 1
    /// opaque — so selecting a clip picks a frame instead of a second image.
    /// The identity is the clip's for as long as it has a preview: a
    /// re-render refreshes one atlas tile instead of abandoning a trail of
    /// them — `STAGE_IMAGE_ID`'s trick in `visualizer.rs`, one per clip.
    image: Arc<RenderImage>,
    /// Whether the atlas has been told about these bytes. False for every
    /// fresh bake, so the painter refreshes the tile under the kept identity
    /// before drawing it.
    published: bool,
}

impl Preview {
    /// A seam row baked under `identity` — the retiring preview's, so the
    /// atlas tile is reused — or a fresh one. `None` for a row that is not
    /// `width * height` of RGBA: a seam drift this painter cannot draw, and
    /// skipping it leaves the flat fill, which is already what a missing
    /// preview means.
    fn decode(row: AnnotationPreview, identity: Option<ImageId>) -> Option<(SharedString, Self)> {
        if row.width == 0
            || row.height == 0
            || row.pixels.len() != (row.width * row.height * 4) as usize
        {
            return None;
        }
        let mut image = bake(row.width, row.height, &row.pixels);
        if let Some(id) = identity {
            image.id = id;
        }
        Some((
            row.annotation_id.into(),
            Self {
                image: Arc::new(image),
                published: false,
            },
        ))
    }
}

/// What the range on screen wants of the fine waveform — see
/// [`Editor::fine_need`].
enum FineNeed {
    /// What is held answers the range on screen, a measurement of it is
    /// already in flight, or this zoom has no use for one.
    Nothing,
    /// The view is back under the stored envelope's own resolution: the held
    /// window should go.
    Discard,
    /// Measure this.
    Measure(Cut),
}

/// A fine window's identity: the range measured, into how many buckets, for
/// which zoom. Named for the cut rather than the window because `Window` in
/// this module is gpui's.
///
/// `buckets` is redundant with the other three — it is `(end - start) * zoom` —
/// but a request and the answer to it are compared field by field, and the seam
/// clamps both the range and the count, so what came back is recorded rather
/// than re-derived.
#[derive(Clone, Copy, PartialEq)]
struct Cut {
    start: f64,
    end: f64,
    buckets: usize,
    zoom: f32,
}

/// One measured window of audio: a bucket per pixel over the range on screen,
/// which is the detail `get_track_waveform`'s fixed 30 000 buckets stopped
/// having once the zoom passed them.
///
/// Held with a margin either side of the view, so a pan is answered from what
/// is already here rather than by a round trip.
struct Fine {
    cut: Cut,
    /// Where the audio ends. A window butting against it still covers a view
    /// that runs past it — there is nothing out there to have measured.
    duration: f64,
    /// The same three envelopes `TrackWaveform::bands` carries and in the same
    /// units, over a shorter range and a finer grid. That is what lets
    /// [`paint_waveform`] pick a source and then forget which one it picked.
    bands: BandEnvelopes,
}

impl Fine {
    /// Whether this answers `start..end` at `zoom`.
    fn covers(&self, start: f64, end: f64, zoom: f32) -> bool {
        self.cut.zoom == zoom
            && self.cut.start <= start.max(0.)
            && self.cut.end >= end.min(self.duration)
    }

    /// Where its buckets sit on the timeline.
    fn grid(&self) -> Grid {
        Grid {
            origin: self.cut.start,
            per_second: self.bands.low.len() as f64 / (self.cut.end - self.cut.start),
            count: self.bands.low.len(),
        }
    }
}

/// How much of a viewport is measured either side of it. A pan of up to half a
/// screen is free; the cost is that every window measures twice the audio it
/// shows, which is microseconds of a scan over a buffer already in memory.
const FINE_MARGIN: f64 = 0.5;

/// What a cut or a copy took, in the two shapes the web's clipboard has.
///
/// The clips are whole rows so a paste can mint real clips from them, and the
/// offsets are relative to the region (or to the cursor) so a paste lands
/// wherever the cursor is now rather than where the copy happened.
struct Clipboard {
    /// `offsetFromStart`, `row` relative to the topmost copied clip, and the
    /// clip itself.
    items: Vec<(f64, usize, Clip)>,
    /// How long the whole clipboard is, which is what the cursor spans after a
    /// paste and how far a duplicate moves.
    span: f64,
}

/// One state the timeline can be put back to.
///
/// The selection and the cursor travel with the clips because they point into
/// them: an undo that restored a deleted clip but left the selection naming
/// what was there instead would put the next command somewhere the eye is not.
///
/// Whole snapshots rather than a log of inverse edits, because the whole list
/// is already the unit this screen edits *and* writes. A command is a
/// replacement, so its inverse is the list it replaced, and every command gets
/// undo for free instead of owing an inverse of its own. A clip list is a few
/// hundred small structs behind an `Rc`, so a step costs one clone of the
/// list's spine. The stack itself is [`crate::history::History`].
#[derive(Clone)]
struct Snapshot {
    clips: Rc<[Clip]>,
    selected: Vec<SharedString>,
    cursor: Option<Cursor>,
}

/// A right-click's pending insertion: where the clip would go, and the
/// patterns on offer.
///
/// `insert` distinguishes the web's two modes — dropping a clip *onto* the
/// lane under the pointer, or opening a *new* lane at the boundary the pointer
/// is within a quarter-lane of, shifting everything at or above it up.
#[derive(Clone, Copy)]
struct InsertMenu {
    start: f64,
    end: f64,
    row: usize,
    insert: bool,
    /// Which pattern the keyboard would commit. The pointer never reads it —
    /// a click names its own row — so this is the whole of what "the menu has
    /// a keyboard" means, and there is no second notion of focus for the two
    /// to disagree over.
    active: usize,
}

/// The selection cursor: a point in time, or a rectangle of time × lanes.
///
/// `start` and `end` are stored as the gesture produced them, not normalised,
/// because a right-to-left sweep is a real cursor and every reader takes its
/// own min and max — the web store does exactly this, and normalising here
/// would quietly change which end a later edit anchors to.
#[derive(Clone, Copy)]
struct Cursor {
    row: usize,
    row_end: Option<usize>,
    start: f64,
    end: Option<f64>,
}

impl Cursor {
    /// The time range, lowest first, or `None` for a point cursor.
    fn span(self) -> Option<(f64, f64)> {
        self.end
            .map(|end| (self.start.min(end), self.start.max(end)))
    }

    /// The lane band, lowest first. A point cursor is one lane.
    fn rows(self) -> (usize, usize) {
        let end = self.row_end.unwrap_or(self.row);
        (self.row.min(end), self.row.max(end))
    }
}

/// A latched zoom anchor: the time held under a point on the canvas, and when
/// the gesture last fed it.
#[derive(Clone, Copy)]
struct Anchor {
    offset: f32,
    time: f64,
    at: std::time::Instant,
}

/// How long a zoom gesture's anchor survives without another notch.
///
/// The web has two — 100 ms for a modified wheel, 120 ms for the trackpad
/// pinch that arrives as ctrl-wheel — for one reason, which is that a momentum
/// flick keeps delivering after the fingers lift. One number here, the longer
/// of the two: two idle timeouts for one gesture is a distinction the web
/// source does not defend.
const ANCHOR_IDLE: Duration = Duration::from_millis(120);

/// How often a scrub is allowed to move the transport. `SEEK_THROTTLE_MS`.
const SEEK_THROTTLE: Duration = Duration::from_millis(32);

/// The snap capture radius, in screen pixels, for a cursor or an insertion —
/// `snapToGrid`'s `15`.
const SNAP_CAPTURE: f32 = 15.;
/// The same radius inside a clip drag, which the web tightens to `12`.
const SNAP_CAPTURE_DRAG: f32 = 12.;

/// The shortest a clip may be left by a resize.
///
/// Not `MIN_ANNOTATION_DURATION` (0.05 s): the web guards a resize at 0.1 s
/// and reserves the smaller floor for splits, pastes and insertions, which
/// are different commands and are not ported yet.
const MIN_RESIZE: f64 = 0.1;

/// `MIN_ANNOTATION_DURATION`: the shortest a clip a *command* produces may be.
/// A split half, a paste remnant or a region-cleared tail below this is
/// dropped rather than kept — deliberately smaller than [`MIN_RESIZE`], which
/// is what a hand at the edge of a clip is held to.
const MIN_CLIP: f64 = 0.05;

/// How close to a lane *boundary* a right-click has to be, in lanes, to mean
/// "open a new layer here" rather than "drop it on this lane".
const INSERT_BOUNDARY: f32 = 0.25;

/// One bar, for a track with no beat grid to ask: the web's default average
/// beat of 0.5 s times its default four beats to the bar.
const DEFAULT_BAR: f64 = 2.;

/// How long a clip a right-click inserts at `after` should be.
///
/// The next downbeat if there is one — so an inserted clip lands on the bar
/// line the eye can see — and the mean bar otherwise.
fn bar_length(beats: Option<&BeatGrid>, after: f64) -> f64 {
    let Some(grid) = beats.filter(|grid| !grid.beats.is_empty()) else {
        return DEFAULT_BAR;
    };
    if let Some(next) = grid
        .downbeats
        .iter()
        .map(|beat| f64::from(*beat))
        .find(|beat| *beat > after)
    {
        return next - after;
    }
    if grid.downbeats.len() > 1 {
        let first = f64::from(grid.downbeats[0]);
        let last = f64::from(grid.downbeats[grid.downbeats.len() - 1]);
        return (last - first) / (grid.downbeats.len() - 1) as f64;
    }
    let average = if grid.beats.len() > 1 {
        f64::from(grid.beats[grid.beats.len() - 1] - grid.beats[0]) / (grid.beats.len() - 1) as f64
    } else {
        0.5
    };
    average
        * if grid.beats_per_bar == 0 {
            4.
        } else {
            f64::from(grid.beats_per_bar)
        }
}

/// The epsilon a marquee's containment test allows, so a clip whose edge was
/// snapped to the same beat as the sweep's still counts as inside.
const CONTAINED_EPSILON: f64 = 0.001;

/// How close two loop bounds have to be to count as the same loop, which is
/// what turns the loop key into a toggle. The web's 1 ms tolerance.
const LOOP_EPSILON: f64 = 0.001;

/// `snapToGrid`: quantise `time` to the beat subdivision the zoom asks for, but
/// only when the quantised point is within `capture` screen pixels of it.
///
/// The capture radius is a parameter because the web has two for the same
/// conceptual gesture — 15 px for the selection cursor, 12 px inside a clip
/// drag — written twice with duplicated bodies. Ported as one function with
/// two call sites rather than as two functions.
fn snap(beats: Option<&BeatGrid>, time: f64, zoom: f32, capture: f32) -> f64 {
    let Some(snapped) = beat_snap(beats, time, zoom) else {
        return time;
    };
    if (snapped - time).abs() * f64::from(zoom) < f64::from(capture) {
        snapped
    } else {
        time
    }
}

/// The quantised point, before the capture test, or `None` when there is no
/// grid to quantise against.
///
/// The subdivision ladder is the web's, numbers not names: at or above 200
/// px/s and below 100 it is quarters of a beat, and in between it is halves.
/// The web spells the two ends `sixteenth` and `quarter` and gives both four
/// divisions, so the three-tier ladder it documents is really two tiers.
fn beat_snap(beats: Option<&BeatGrid>, time: f64, zoom: f32) -> Option<f64> {
    let grid = beats.filter(|grid| !grid.beats.is_empty())?;
    let beats = &grid.beats;
    if beats.len() == 1 {
        return Some(f64::from(beats[0]));
    }
    let average = f64::from(beats[beats.len() - 1] - beats[0]) / (beats.len() - 1) as f64;
    let index = beats
        .partition_point(|beat| f64::from(*beat) <= time)
        .saturating_sub(1);
    let prev = f64::from(beats[index]);
    let next = f64::from(*beats.get(index + 1)?);
    let length = if next - prev > 0. {
        next - prev
    } else {
        average
    };
    if !length.is_finite() || length <= 0. {
        return Some(prev);
    }
    let divisions = if (100. ..200.).contains(&zoom) {
        2.
    } else {
        4.
    };
    let step = ((time - prev) / length * divisions)
        .round()
        .clamp(0., divisions);
    Some((prev + step / divisions * length).clamp(prev, next))
}

/// Where the eye is: a horizontal zoom in pixels per second, and a scroll in
/// pixels. The same two numbers the web timeline's scroll container holds.
#[derive(Clone, Copy)]
struct View {
    zoom: f32,
    scroll: f32,
    /// `zoomY`: how tall a lane is, as a multiple of [`LANE_HEIGHT`]. The
    /// waveform and the ruler above it never scale with it — they are a
    /// navigation surface, not part of the annotation workspace.
    zoom_y: f32,
    /// How far the lane block has been lifted off the canvas floor, in pixels.
    ///
    /// The vertical scroll, measured from the **bottom** rather than from the
    /// top, because the lanes are bottom-anchored: at zero, z = 0 sits on the
    /// floor. Stated this way a new layer, a vertical zoom or a resize keeps
    /// the floor where it is without anybody recomputing a scroll — which is
    /// what the web does by hand every time, as `scrollTop = maxScrollTop`.
    lift: f32,
}

/// What the transport is doing. A mirror of the audio host's own state, which
/// is the authority — every field here was last read from a snapshot.
#[derive(Default)]
struct Transport {
    playing: bool,
    position: f32,
    duration: f32,
    /// A poll loop is running. Exactly one at a time, or every play would
    /// leave another behind.
    polling: bool,
}

/// What a wheel notch means, which is entirely a question of the modifier
/// held with it.
///
/// Named here rather than passed as a rate and a flag because the three are
/// exclusive and each takes a different axis: a call site that had to say
/// "this rate, but vertically" would be a call site that could say something
/// the canvas has no answer for.
#[derive(Clone, Copy)]
enum Wheel {
    /// Bare: the scroll container's own gesture, both axes.
    Scroll,
    /// Horizontal zoom at an exponential rate — the platform key's, or the
    /// trackpad pinch's.
    Zoom(f32),
    /// `altKey`: vertical zoom, which is the lane height.
    Lanes,
}

/// What the pointer is doing between a press and a release.
enum Gesture {
    /// Dragging the playhead over the ruler. The 32px strip only — the
    /// waveform below it clears the selection instead, which is the one
    /// surprise in the web's pointer map and the one this host used to get
    /// wrong.
    Scrub,
    /// Sweeping a rectangular time × lane range out of empty lane space.
    /// `row` and `start` are where the sweep began; the far corner is wherever
    /// the pointer is now.
    Marquee { row: usize, start: f64 },
    /// Dragging clips by their headers: moving them all sideways, or pulling
    /// one edge of each. `pressed` is the clip the pointer took hold of, which
    /// is the only one snapping is computed from — the rest keep their spacing
    /// by taking its snapped delta. `moved` distinguishes a drag from a press
    /// that only selected.
    Clips {
        pressed: SharedString,
        drag: Drag,
        origin: Point<Pixels>,
        initial: Rc<[Initial]>,
        /// The layer ladder as it stood when the pointer took hold.
        ///
        /// Captured with the positions, and for the same reason: a drag is
        /// recomputed from the press on every move, so every input to that
        /// arithmetic has to be the press's. Read from the working copy
        /// instead, a drag that mints a new layer would renumber the ladder it
        /// is being measured against and walk a clip a further lane on each
        /// move.
        layers: Rc<[i64]>,
        moved: bool,
    },
}

/// Which part of a clip a press took hold of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Move,
    Resize(Edge),
}

/// Where one dragged clip was when the pointer took hold. Every move is
/// computed from the press rather than from the last frame, so a drag out and
/// back lands exactly where it started.
struct Initial {
    id: SharedString,
    start: f64,
    end: f64,
    /// The lane it was in, 1-based as [`lanes`] resolves them — which is what
    /// [`row_to_z`] expects.
    row: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Start,
    End,
}

impl Edge {
    fn suffix(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
}

impl View {
    /// `MIN_ZOOM` / `MAX_ZOOM` from `utils/timeline-constants.ts`, in pixels
    /// per second.
    const MIN_ZOOM: f32 = 25.;
    const MAX_ZOOM: f32 = 500.;
    /// The web store's opening `zoom`.
    const DEFAULT_ZOOM: f32 = 50.;
    /// `ZOOM_SENSITIVITY`: the exponential rate a modified wheel notch scales
    /// by.
    const ZOOM_PER_PIXEL: f32 = 0.002;
    /// The rate a trackpad pinch scales by, which arrives as a ctrl-wheel.
    /// Five times [`Self::ZOOM_PER_PIXEL`] — hardcoded on the web, where the
    /// named constant is bypassed on that path, and kept because a pinch's
    /// deltas are a fifth the size of a wheel's.
    const ZOOM_PER_PIXEL_PINCH: f32 = 0.01;
    /// `MIN_ZOOM_Y` / `MAX_ZOOM_Y`, and `ZOOM_Y_SENSITIVITY` for the alt-wheel
    /// that walks between them.
    const MIN_ZOOM_Y: f32 = 0.5;
    const MAX_ZOOM_Y: f32 = 1.5;
    const ZOOM_Y_PER_PIXEL: f32 = 0.003;

    /// The time under a point `offset` pixels from the canvas's left edge.
    fn time_at(self, offset: f32) -> f64 {
        f64::from((offset + self.scroll) / self.zoom)
    }

    /// The time range a canvas `width` pixels wide shows.
    fn visible(&self, width: f32) -> (f64, f64) {
        (self.time_at(0.), self.time_at(width))
    }

    /// Where a time lands, in pixels from the canvas's left edge. Floored,
    /// because every coordinate in `timeline-drawing.ts` is.
    fn x_of(self, time: f64) -> f32 {
        (time as f32 * self.zoom - self.scroll).floor()
    }
}

impl Editor {
    /// The track being edited. The window title and the tab's chip are the
    /// only readers outside this module.
    pub(crate) fn track_name(&self) -> &str {
        &self.track_name
    }

    /// Disarm the loop region on the way out, reporting whether one was armed.
    /// The tab teardown's half of the close contract: the loop belongs to the
    /// transport, which outlives the tab, and a region left armed would wrap
    /// the *next* track at times that meant something on this one.
    pub(crate) fn take_loop_region(&mut self) -> bool {
        self.loop_region.take().is_some()
    }

    /// Whether an edit is allowed to land at all: a score this host owns, and
    /// no other reason to refuse.
    fn writable(&self) -> bool {
        self.score.as_ref().is_some_and(|score| !score.read_only)
    }

    /// Where the lanes are, on the canvas the last frame painted.
    fn layout(&self) -> Layout {
        Layout::new(
            lane_count(&self.clips),
            f32::from(self.canvas.get().size.height),
            self.view,
        )
    }

    /// Scroll the lanes, in pixels off the floor.
    ///
    /// The one place [`View::lift`] is written, so the bound a browser scroll
    /// container would apply for free is applied once here instead.
    fn set_lift(&mut self, lift: f32) {
        self.view.lift = lift.clamp(0., self.layout().max_lift);
    }

    /// `altKey` wheel: taller or shorter lanes, holding whatever the pointer
    /// was over.
    ///
    /// The anchor is rows-from-the-floor rather than a pixel, because that is
    /// the quantity the bottom-anchored block preserves: zoom about a pixel and
    /// the floor would drift out from under z = 0.
    fn zoom_lanes(&mut self, delta: f32, at: f32) {
        // Above the lanes the gesture means nothing — the web ignores an
        // alt-wheel over the waveform for the same reason.
        if at < TRACK_AREA_Y {
            return;
        }
        let height = f32::from(self.canvas.get().size.height);
        let rows = self.layout().rows_from_floor(at);
        self.view.zoom_y =
            (self.view.zoom_y * delta.exp()).clamp(View::MIN_ZOOM_Y, View::MAX_ZOOM_Y);
        // The floor is `height + lift` by construction, so holding `rows`
        // lanes between it and the pointer is one line rather than a delta.
        self.set_lift(rows * self.layout().lane + at - height);
    }

    /// `H`: the lane height that fits every layer on the canvas, clamped to
    /// what the vertical zoom allows.
    ///
    /// No anchor, so the block goes back to sitting on the floor — which on a
    /// canvas too short even at the minimum is where the layer everything is
    /// stacked over belongs.
    fn fit_lanes(&mut self) {
        let height = f32::from(self.canvas.get().size.height);
        let rows = lane_count(&self.clips) as f32;
        self.view.zoom_y = ((height - TRACK_AREA_Y) / (rows * LANE_HEIGHT))
            .clamp(View::MIN_ZOOM_Y, View::MAX_ZOOM_Y);
        self.view.lift = 0.;
    }

    /// The track and venue this conversation would be about, once there is a
    /// score to hang it on.
    pub(crate) fn subject(&self) -> Option<(String, String, String)> {
        let score = self.score.as_ref()?;
        Some((
            self.track_id.clone(),
            self.venue_id.clone(),
            score.id.clone(),
        ))
    }

    /// The room this timeline is being worked on in.
    pub(crate) fn venue_id(&self) -> &str {
        &self.venue_id
    }

    /// The score the stage above this timeline should be lit by — the open
    /// document, which is this screen's own fact and nobody else's to derive.
    pub(crate) fn lit(&self) -> Option<crate::visualizer::Lit> {
        let score = self.score.as_ref()?;
        Some(crate::visualizer::Lit {
            score: score.id.clone(),
            ordinal: score.ordinal,
        })
    }

    /// The range on screen, from the canvas the last frame painted.
    fn visible(&self) -> (f64, f64) {
        self.view.visible(f32::from(self.canvas.get().size.width))
    }

    /// The window worth measuring, or `None` while the stored envelope still
    /// has a bucket per pixel.
    ///
    /// That is most of the zoom range and every short track: 30 000 buckets
    /// over ninety seconds is 333 a second, and only past 333 pixels a second
    /// does a pixel have less than a bucket in it. Below the threshold there is
    /// nothing a measurement could add, and asking would be a round trip for
    /// data already in hand.
    fn fine_window(&self) -> Option<Cut> {
        let waveform = self.waveform.as_ref()?;
        let duration = waveform.duration_seconds;
        let stored = stored_grid(waveform, duration)?;
        if f64::from(self.view.zoom) <= stored.per_second {
            return None;
        }
        let (from, to) = self.visible();
        let margin = (to - from) * FINE_MARGIN;
        let start = (from - margin).max(0.);
        let end = (to + margin).min(duration);
        let buckets = ((end - start) * f64::from(self.view.zoom)).ceil();
        if !buckets.is_finite() || buckets < 1. {
            return None;
        }
        Some(Cut {
            start,
            end,
            buckets: buckets as usize,
            zoom: self.view.zoom,
        })
    }

    /// What the range on screen wants of the fine waveform.
    ///
    /// The prepaint's gate and the request itself are the same question, so
    /// they are the same function. Prepaint learns the canvas's width and has
    /// to ask for a window measured in it, but it learns that width again on
    /// every frame of a sidebar slide, and a deferred round trip through the
    /// app per frame to be told the window in hand already covers it is the
    /// whole cost of asking — [`FINE_MARGIN`] means it usually does.
    fn fine_need(&self) -> FineNeed {
        let Some(want) = self.fine_window() else {
            // Back under the stored envelope's own resolution, where measuring
            // would only reproduce it.
            return if self.fine.is_some() {
                FineNeed::Discard
            } else {
                FineNeed::Nothing
            };
        };
        if self.fine_pending.is_some() || self.drawn_buckets().is_some() {
            return FineNeed::Nothing;
        }
        FineNeed::Measure(want)
    }

    /// How many measured buckets the canvas is drawing from, or `None` when it
    /// is drawing the stored envelope.
    ///
    /// The paint's choice of source, the toolbar's resolution readout and the
    /// test for whether another measurement is worth asking for are all this
    /// one question, so the panel cannot claim a resolution the canvas is not
    /// drawing.
    fn drawn_buckets(&self) -> Option<usize> {
        let (from, to) = self.visible();
        self.fine
            .as_ref()
            .filter(|fine| fine.covers(from, to, self.view.zoom))
            .map(|fine| fine.cut.buckets)
    }

    /// The clip whose *header bar* covers `(time, y)` in `row`, if any.
    ///
    /// Only the top [`CLIP_HEADER`] pixels of a clip are grabbable — the body
    /// below is inert, and a press there is an empty-lane press. That is what
    /// leaves the body free to be a preview surface, and it is the difference
    /// between this canvas and one where a clip swallows its whole lane.
    fn clip_at(&self, time: f64, row: usize, y: f32) -> Option<&Clip> {
        if y >= self.layout().top(row) + 1. + CLIP_HEADER {
            return None;
        }
        self.clips
            .iter()
            .find(|clip| clip.row == row && time >= clip.start && time <= clip.end)
    }

    /// The furthest the view may scroll: the content's width less the
    /// canvas's. A browser scroll container applies this bound for free; here
    /// it is the one place scroll is written, so it applies it once.
    fn set_scroll(&mut self, scroll: f32) {
        let content = f64::from(self.transport.duration).max(0.) as f32 * self.view.zoom;
        let width = f32::from(self.canvas.get().size.width);
        self.view.scroll = scroll.clamp(0., (content - width).max(0.));
    }

    /// Rewrite every clip the drag is holding, from where they were when it
    /// started.
    ///
    /// One function for both a move and a resize because the difference
    /// between them is three lines of arithmetic over the same captured
    /// positions, and the guards — never below zero, never past the track,
    /// never shorter than [`MIN_RESIZE`] — are shared. Snapping is computed
    /// from the *pressed* clip alone and applied to the rest as a delta, which
    /// is what keeps a group's relative spacing exact.
    fn drag_clips(&mut self, gesture: &Gesture, delta: f64, rows: i32) {
        let Gesture::Clips {
            pressed,
            drag,
            initial,
            layers,
            ..
        } = gesture
        else {
            return;
        };
        let Some(anchor) = initial.iter().find(|clip| &clip.id == pressed) else {
            return;
        };
        let duration = f64::from(self.transport.duration).max(0.);
        let beats = self.beats.as_deref();
        let zoom = self.view.zoom;
        let snap = |time: f64| snap(beats, time, zoom, SNAP_CAPTURE_DRAG);

        // What the pressed clip's own edge did, as a delta the rest can take.
        // `None` where the pressed clip's guard refused the move, which
        // refuses it for the whole group rather than letting the others slide
        // without it.
        let shift = match drag {
            Drag::Move => Some(snap(anchor.start + delta).max(0.) - anchor.start),
            Drag::Resize(Edge::Start) => {
                let start = snap(anchor.start + delta);
                (start < anchor.end - MIN_RESIZE).then_some(start - anchor.start)
            }
            Drag::Resize(Edge::End) => {
                let end = snap(anchor.end + delta);
                (end > anchor.start + MIN_RESIZE).then_some(end - anchor.end)
            }
        };
        let Some(shift) = shift else { return };

        // Downward motion stops at the floor; upward is unclamped, and mints
        // z values above the current top. Rows count *down* the screen, so
        // "the lowest selected row" is the largest index.
        let rows = match drag {
            Drag::Move => {
                let lowest = initial.iter().map(|clip| clip.row).max().unwrap_or(0);
                rows.min((layers.len() as i32) - lowest as i32)
            }
            Drag::Resize(_) => 0,
        };

        let held: HashMap<&str, &Initial> = initial
            .iter()
            .map(|clip| (clip.id.as_ref(), clip))
            .collect();
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        for clip in &mut clips {
            let Some(was) = held.get(clip.id.as_ref()) else {
                continue;
            };
            match drag {
                Drag::Move => {
                    clip.start = (was.start + shift).max(0.);
                    clip.end = clip.start + (was.end - was.start);
                    // Applied for real rather than as a paint offset: the
                    // whole lane change is a function of the row the press
                    // captured, so recomputing it every move from that is
                    // idempotent and there is no second, visual-only
                    // representation to keep in step with this one.
                    clip.z = row_to_z(layers, was.row as i32 + rows - 1);
                }
                Drag::Resize(Edge::Start) => {
                    let moved = (was.start + shift).max(0.);
                    if moved < was.end - MIN_RESIZE {
                        clip.start = moved;
                    }
                }
                Drag::Resize(Edge::End) => {
                    let moved = (was.end + shift).min(duration);
                    if moved > was.start + MIN_RESIZE {
                        clip.end = moved;
                    }
                }
            }
        }
        self.replace_clips(clips);
    }

    /// Take a new working copy: re-derive every lane, and mark the score as
    /// owing a write.
    ///
    /// **The only way the clip list changes.** Every gesture and every command
    /// funnels through here, which is what makes "a lane is a function of
    /// every clip's z" a fact rather than a convention, and what keeps
    /// [`Editor::dirty`] from being something a caller can forget to set.
    fn replace_clips(&mut self, mut clips: Vec<Clip>) {
        self.mint_unknown_ids(&mut clips);
        let rows = rows_by_z(clips.iter().map(|clip| clip.z));
        for clip in &mut clips {
            clip.row = rows.get(&clip.z).copied().unwrap_or(1);
        }
        self.clips = clips.into();
        self.dirty = true;
    }

    /// Give a fresh `new:` id to any clip the seam has never heard of.
    ///
    /// The seam allocates clip identity and refuses a stored id it did not
    /// issue, so a create has to arrive as a draft. Everything that mints a
    /// clip here already does that — except an **undo of a delete**, which
    /// brings back a clip under the id the write that removed it retired. This
    /// is where that id is handed in and a draft taken out, which is what
    /// makes "an undo is just a list this screen used to have" true rather
    /// than nearly true.
    fn mint_unknown_ids(&mut self, clips: &mut [Clip]) {
        let stored: std::collections::HashSet<&str> =
            self.base.iter().map(|clip| clip.id.as_str()).collect();
        let mut minted: HashMap<SharedString, SharedString> = HashMap::new();
        for clip in clips.iter_mut() {
            if clip.id.starts_with("new:") || stored.contains(clip.id.as_ref()) {
                continue;
            }
            let fresh: SharedString = format!("new:{}", uuid::Uuid::new_v4()).into();
            minted.insert(clip.id.clone(), fresh.clone());
            clip.id = fresh;
        }
        if minted.is_empty() {
            return;
        }
        for id in &mut self.selected {
            if let Some(fresh) = minted.get(id) {
                *id = fresh.clone();
            }
        }
    }

    /// Where the timeline is now, as something an undo could return to.
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            clips: Rc::clone(&self.clips),
            selected: self.selected.clone(),
            cursor: self.cursor,
        }
    }

    /// Mark the point an undo comes back to, before running an edit.
    fn checkpoint(&mut self) {
        let now = self.snapshot();
        self.history.record(now);
    }

    /// Forget the last checkpoint when the edit it was taken for changed
    /// nothing.
    ///
    /// [`Self::replace_clips`] is the only thing that swaps the list and it
    /// always swaps in a fresh allocation, so pointer identity answers exactly
    /// the question "did anything run".
    fn abandon_checkpoint(&mut self) {
        let clips = Rc::clone(&self.clips);
        self.history
            .abandon_if(|was| Rc::ptr_eq(&was.clips, &clips));
    }

    /// Step back, or forward. `false` when there is nowhere to go.
    fn undo(&mut self) -> bool {
        let Some(was) = self.history.undo(self.snapshot()) else {
            return false;
        };
        self.restore(was);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.history.redo(self.snapshot()) else {
            return false;
        };
        self.restore(next);
        true
    }

    /// Put the timeline back to a snapshot, which is a rewrite of the working
    /// copy like any other and owes a write like any other.
    fn restore(&mut self, snapshot: Snapshot) {
        self.selected = snapshot.selected;
        self.cursor = snapshot.cursor;
        self.replace_clips(snapshot.clips.to_vec());
    }

    /// `setLoopRegion` / `clearLoopRegion`: loop the cursor's range, or take
    /// the loop off. Returns what the transport should be told.
    ///
    /// One key does both, and which one it does is a comparison rather than a
    /// mode: a cursor with no range has no loop to describe, and a cursor
    /// describing the loop already running is a request to stop it.
    fn toggle_loop(&mut self) -> Option<(f64, f64)> {
        let asked = self
            .cursor
            .and_then(Cursor::span)
            .filter(|(from, to)| to - from > LOOP_EPSILON);
        let same = matches!(
            (asked, self.loop_region),
            (Some(asked), Some(running))
                if (asked.0 - running.0).abs() <= LOOP_EPSILON
                    && (asked.1 - running.1).abs() <= LOOP_EPSILON
        );
        self.loop_region = asked.filter(|_| !same);
        self.loop_region
    }

    /// Re-derive the cursor from where the selected clips actually are.
    ///
    /// `syncCursorFromAnnotations`: the cursor follows a drag, so the range a
    /// later command acts on is the one the eye can see rather than the one
    /// the press left behind.
    fn sync_cursor(&mut self) {
        let mut rows = self
            .clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .map(|clip| clip.row);
        let Some(first) = rows.next() else {
            return;
        };
        let (row, row_end) = rows.fold((first, first), |(low, high), row| {
            (low.min(row), high.max(row))
        });
        let selected = || {
            self.clips
                .iter()
                .filter(|clip| self.selected.contains(&clip.id))
        };
        self.cursor = Some(Cursor {
            row,
            row_end: (row_end != row).then_some(row_end),
            start: selected().map(|clip| clip.start).fold(f64::MAX, f64::min),
            end: Some(selected().map(|clip| clip.end).fold(f64::MIN, f64::max)),
        });
    }

    /// Everything the marquee's rectangle fully contains. Partial overlaps are
    /// deliberately left out — a sweep selects what it covered, not what it
    /// touched.
    fn select_within(&mut self, rows: (usize, usize), span: (f64, f64)) {
        self.selected = self
            .clips
            .iter()
            .filter(|clip| {
                (rows.0..=rows.1).contains(&clip.row)
                    && clip.start >= span.0 - CONTAINED_EPSILON
                    && clip.end <= span.1 + CONTAINED_EPSILON
            })
            .map(|clip| clip.id.clone())
            .collect();
    }

    /// Centre the view on the playhead, if the eye is following it.
    ///
    /// Centring, not edge-paging: the web recentres whenever the drawn
    /// position differs from the centred one, so the playhead sits still and
    /// the timeline moves under it.
    fn follow_playhead(&mut self) {
        if !self.follow {
            return;
        }
        let width = f32::from(self.canvas.get().size.width);
        self.set_scroll(self.transport.position * self.view.zoom - width / 2.);
    }

    /// Take up the ids the seam allocated for the clips this screen created.
    ///
    /// Not cosmetic: a working-copy clip that kept its local `new:` id after
    /// the write that stored it would look like *another* create to the next
    /// write, and the score would grow a duplicate every time it was saved.
    fn adopt_ids(&mut self, minted: &std::collections::BTreeMap<String, String>) {
        if minted.is_empty() {
            return;
        }
        let rename = |id: &SharedString| -> SharedString {
            minted
                .get(id.as_ref())
                .map_or_else(|| id.clone(), |stored| stored.clone().into())
        };
        let clips: Vec<Clip> = self
            .clips
            .iter()
            .map(|clip| Clip {
                id: rename(&clip.id),
                ..clip.clone()
            })
            .collect();
        self.clips = clips.into();
        self.selected = self.selected.iter().map(rename).collect();
    }

    /// Adopt a whole track's previews, replacing whatever was here — the shape
    /// `generate_annotation_previews` answers in. A clip the seam did not name
    /// drops out; a clip it did keeps its atlas identity so the repaint
    /// refreshes a tile rather than minting one.
    fn install_previews(&mut self, rows: Vec<AnnotationPreview>) {
        let mut previews = self.previews.borrow_mut();
        let mut retired = std::mem::take(&mut *previews);
        for row in rows {
            let identity = retired
                .remove(row.annotation_id.as_str())
                .map(|previous| previous.image.id);
            if let Some((id, preview)) = Preview::decode(row, identity) {
                previews.insert(id, preview);
            }
        }
    }

    /// Adopt one clip's re-rendered preview — `preview_annotation`'s answer.
    fn install_preview(&mut self, row: AnnotationPreview) {
        let mut previews = self.previews.borrow_mut();
        let identity = previews
            .remove(row.annotation_id.as_str())
            .map(|previous| previous.image.id);
        if let Some((id, preview)) = Preview::decode(row, identity) {
            previews.insert(id, preview);
        }
    }

    /// Re-derive every clip's label from the patterns list.
    ///
    /// A label is a function of that list, and the list arrives on its own
    /// schedule — a score opened before it landed resolves every clip to
    /// `Pattern <uuid>` and, without this, never comes back. Relabelling
    /// rather than re-resolving because the working copy may hold edits the
    /// stored list has never seen; only the *name* is derived.
    fn relabel(&mut self) {
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        for clip in &mut clips {
            clip.label = label_of(&clip.pattern, &self.patterns);
        }
        self.clips = clips.into();
    }

    /// Clear both the selection and the cursor: what a press on anything that
    /// is not a clip does.
    fn deselect(&mut self) {
        self.selected.clear();
        self.cursor = None;
    }

    // -- the commands the keyboard asks for -----------------------------------
    //
    // Each one is a pure rewrite of the working copy: read the clips, work out
    // the list they should become, hand it to `replace_clips`. None of them
    // talks to the seam — publishing is `Luma::commit_clips`, once, for
    // whatever the gesture left behind. That is what makes a command that
    // touches five clips a single atomic write rather than five that can half
    // land.

    /// The z values the cursor's lane band covers.
    ///
    /// `getRegionInfo`'s z set: the rows are 1-based, so row *r* is the
    /// `r - 1`th layer counting down from the top.
    fn cursor_zs(&self) -> Vec<i64> {
        let Some(cursor) = self.cursor else {
            return Vec::new();
        };
        let layers = z_ladder(&self.clips);
        let (low, high) = cursor.rows();
        (low..=high)
            .filter_map(|row| layers.get(row.checked_sub(1)?).copied())
            .collect()
    }

    /// `deleteInRegion` when the cursor has a range, otherwise delete the
    /// selected clips whole.
    ///
    /// The two are one command because that is what one key does: a range on
    /// screen means "clear this rectangle", and no range means "remove what is
    /// selected". A region delete *clips* what it partly covers rather than
    /// removing it — see [`clear_region`].
    fn delete(&mut self) {
        match self.cursor.and_then(Cursor::span) {
            Some(span) => {
                let zs = self.cursor_zs();
                let clips = clear_region(&self.clips, span, &zs);
                self.replace_clips(clips);
                self.selected.clear();
                self.cursor = None;
            }
            None => {
                if self.selected.is_empty() {
                    return;
                }
                let clips = self
                    .clips
                    .iter()
                    .filter(|clip| !self.selected.contains(&clip.id))
                    .cloned()
                    .collect();
                self.replace_clips(clips);
                self.selected.clear();
            }
        }
    }

    /// `splitAtCursor`: cut every clip the cursor's time crosses, in the
    /// cursor's lane band, and select the right-hand halves.
    ///
    /// A split that would leave either half shorter than [`MIN_CLIP`] is
    /// skipped rather than clamped — a clip too short to see is not what the
    /// gesture asked for.
    fn split(&mut self) {
        let Some(cursor) = self.cursor else { return };
        let at = cursor.start;
        let zs = self.cursor_zs();
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        let mut halves = Vec::new();
        for clip in &mut clips {
            if !zs.contains(&clip.z) || at <= clip.start || at >= clip.end {
                continue;
            }
            if at - clip.start < MIN_CLIP || clip.end - at < MIN_CLIP {
                continue;
            }
            halves.push(clip.copy(at, clip.end, clip.z));
            clip.end = at;
        }
        if halves.is_empty() {
            return;
        }
        self.selected = halves.iter().map(|clip| clip.id.clone()).collect();
        clips.extend(halves);
        self.replace_clips(clips);
    }

    /// `moveAnnotationsVertical`: shift the selection one lane up or down.
    ///
    /// Up is unbounded — it mints a z above the current top. Down is
    /// all-or-nothing: if any selected clip is already on the floor the whole
    /// command is a no-op, so a multi-lane selection cannot collapse into one
    /// lane by being pushed against it.
    fn move_lane(&mut self, down: bool) {
        if self.selected.is_empty() {
            return;
        }
        let layers = z_ladder(&self.clips);
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        let held: Vec<usize> = clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .map(|clip| clip.row)
            .collect();
        if down && held.iter().any(|row| *row >= layers.len()) {
            return;
        }
        let step = if down { 1 } else { -1 };
        for clip in &mut clips {
            if self.selected.contains(&clip.id) {
                clip.z = row_to_z(&layers, clip.row as i32 + step - 1);
            }
        }
        self.replace_clips(clips);
        self.sync_cursor();
    }

    /// `copySelection`: take the region the cursor spans, or the clips that
    /// are selected.
    ///
    /// Region mode *clips* what it partly covers, so what is copied is exactly
    /// the rectangle on screen; object mode takes whole clips. Both need a
    /// cursor, because both store their offsets relative to one.
    fn copy(&mut self) {
        let Some(cursor) = self.cursor else { return };
        let items: Vec<(f64, usize, Clip)> = match cursor.span() {
            Some((from, to)) => {
                let zs = self.cursor_zs();
                let top = cursor.rows().0;
                self.clips
                    .iter()
                    .filter(|clip| zs.contains(&clip.z) && clip.start < to && clip.end > from)
                    .filter_map(|clip| {
                        let (start, end) = (clip.start.max(from), clip.end.min(to));
                        (end - start >= MIN_CLIP).then(|| {
                            (
                                start - from,
                                clip.row.saturating_sub(top),
                                clip.copy(start, end, clip.z),
                            )
                        })
                    })
                    .collect()
            }
            None => {
                let held: Vec<&Clip> = self
                    .clips
                    .iter()
                    .filter(|clip| self.selected.contains(&clip.id))
                    .collect();
                let top = held.iter().map(|clip| clip.row).min().unwrap_or(0);
                held.iter()
                    .map(|clip| {
                        (
                            clip.start - cursor.start,
                            clip.row - top,
                            clip.copy(clip.start, clip.end, clip.z),
                        )
                    })
                    .collect()
            }
        };
        if items.is_empty() {
            return;
        }
        let span = match cursor.span() {
            Some((from, to)) => to - from,
            None => items
                .iter()
                .map(|(offset, _, clip)| offset + (clip.end - clip.start))
                .fold(0., f64::max),
        };
        self.clipboard = Some(Clipboard { items, span });
    }

    /// `cutSelection`: copy, then take out what was copied.
    fn cut(&mut self) {
        self.copy();
        if self.clipboard.is_some() {
            self.delete();
        }
    }

    /// `paste`: drop the clipboard at the cursor, top-left anchored.
    ///
    /// The destination rectangle is cleared first with the same
    /// [`clear_region`] a delete uses — a paste is a replacement, not an
    /// overlay — and the topmost clipboard item lands on the cursor's row with
    /// the rest keeping their relative lanes below it.
    fn paste(&mut self) {
        let (Some(cursor), Some(board)) = (self.cursor, self.clipboard.as_ref()) else {
            return;
        };
        let span = board.span;
        let at = cursor.span().map_or(cursor.start, |(from, _)| from);
        let duration = f64::from(self.transport.duration).max(0.);
        let layers = z_ladder(&self.clips);
        let top = cursor.rows().0.max(1);

        let minted: Vec<Clip> = board
            .items
            .iter()
            .filter_map(|(offset, row, clip)| {
                let (start, end) = (at + offset, at + offset + (clip.end - clip.start));
                (end <= duration)
                    .then(|| clip.copy(start, end, row_to_z(&layers, (top + row) as i32 - 1)))
            })
            .collect();
        if minted.is_empty() {
            return;
        }
        let zs: Vec<i64> = minted.iter().map(|clip| clip.z).collect();
        let mut clips = clear_region(&self.clips, (at, at + span), &zs);
        self.selected = minted.iter().map(|clip| clip.id.clone()).collect();
        clips.extend(minted);
        self.replace_clips(clips);
        self.cursor = Some(Cursor {
            row: top,
            row_end: None,
            start: at,
            end: Some(at + span),
        });
    }

    /// `duplicate`: copy, move the cursor to the end of what was copied, and
    /// paste — which lands a copy immediately after the original.
    fn duplicate(&mut self) {
        self.copy();
        let Some(board) = self.clipboard.as_ref() else {
            return;
        };
        let span = board.span;
        let Some(cursor) = self.cursor else { return };
        let end = cursor
            .span()
            .map_or(cursor.start, |(_, to)| to)
            .max(cursor.start + span);
        // Re-derived from the topmost *selected* clip rather than kept from
        // the cursor, which may still be sitting where a drag started.
        let row = self
            .clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .map(|clip| clip.row)
            .min()
            .unwrap_or(cursor.rows().0);
        self.cursor = Some(Cursor {
            row,
            row_end: None,
            start: end,
            end: None,
        });
        self.paste();
    }

    /// `cloneAnnotationsInPlace`: leave a copy of everything the drag is
    /// holding exactly where it is, so the clips that move away are the
    /// originals and the copies stay put.
    fn clone_in_place(&mut self, ids: &[SharedString]) {
        let copies: Vec<Clip> = self
            .clips
            .iter()
            .filter(|clip| ids.contains(&clip.id))
            .map(|clip| clip.copy(clip.start, clip.end, clip.z))
            .collect();
        if copies.is_empty() {
            return;
        }
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        clips.extend(copies);
        self.replace_clips(clips);
    }

    /// Walk the insertion menu's active row. Clamped at both ends, as the
    /// web's is — a menu that wrapped would commit the wrong pattern to a hand
    /// that held the key a beat too long.
    fn step_menu(&mut self, down: bool) {
        let last = self.patterns.len().saturating_sub(1);
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        menu.active = if down {
            (menu.active + 1).min(last)
        } else {
            menu.active.saturating_sub(1)
        };
        self.menu_scroll.scroll_to_item(menu.active);
    }

    /// The pattern `Enter` would put down, with the insertion it belongs to.
    fn menu_choice(&self) -> Option<(InsertMenu, PatternSummary)> {
        let menu = self.menu?;
        Some((menu, self.patterns.get(menu.active)?.clone()))
    }

    /// Put a clip of `pattern` where `menu` says.
    ///
    /// Add mode drops it on the lane under the pointer; insert mode opens a
    /// *new* lane at the boundary by lifting every layer at or above it,
    /// which is the only command here that renumbers clips it did not touch.
    ///
    /// The target is an argument rather than a read of [`Self::menu`] because
    /// a menu item *is* "this pattern, at that spot": the press that chooses
    /// one is also a press that dismisses the menu, and a command that had to
    /// find the menu still open would be a command racing its own gesture.
    fn insert(&mut self, menu: InsertMenu, pattern: &PatternSummary) {
        self.menu = None;
        let layers = z_ladder(&self.clips);
        let z = row_to_z(&layers, menu.row as i32 - 1);
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        if menu.insert {
            for clip in &mut clips {
                if clip.z >= z {
                    clip.z += 1;
                }
            }
        }
        let minted = Clip {
            id: format!("new:{}", uuid::Uuid::new_v4()).into(),
            pattern: pattern.id.clone().into(),
            label: pattern.name.clone().into(),
            color: ladder::pattern(&pattern.id),
            start: menu.start,
            end: menu.end,
            row: 0,
            z,
            blend: BlendMode::Replace,
            args: serde_json::Value::Object(serde_json::Map::new()),
        };
        self.selected = vec![minted.id.clone()];
        self.cursor = Some(Cursor {
            row: menu.row.max(1),
            row_end: None,
            start: minted.start,
            end: Some(minted.end),
        });
        clips.push(minted);
        self.replace_clips(clips);
    }
}

/// The seam's snapshot shape for a clip list.
///
/// `replace_track_scores` takes [`TrackScore`] rows and reads only the fields
/// [`TrackClip`] has — the revision it compares against is taken over those
/// alone — so the database bookkeeping a screen never sees is left empty
/// rather than invented.
fn rows_of(score_id: &str, clips: &[TrackClip]) -> Vec<TrackScore> {
    clips
        .iter()
        .map(|clip| TrackScore {
            id: clip.id.clone(),
            uid: None,
            score_id: score_id.to_string(),
            pattern_id: clip.pattern_id.clone(),
            start_time: clip.start_time,
            end_time: clip.end_time,
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
            args: clip.args.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}

/// The distinct `zIndex` values in the score, **descending** — so index 0 is
/// the topmost layer, which is the order [`row_to_z`] indexes by.
fn z_ladder(clips: &[Clip]) -> Vec<i64> {
    let mut z: Vec<i64> = clips.iter().map(|clip| clip.z).collect();
    z.sort_unstable_by(|left, right| right.cmp(left));
    z.dedup();
    z
}

/// `rowToZ`: which `zIndex` a lane index means, where the index is 0-based
/// from the top of the occupied layers.
///
/// Off either end it *mints*: above the top it counts up from the highest z,
/// below the bottom it counts down from the lowest. That is what lets a clip
/// be dragged into a lane that does not exist yet, and it is why lane moves
/// need no create-a-layer command of their own.
fn row_to_z(layers: &[i64], row: i32) -> i64 {
    let Some((&top, &bottom)) = layers.first().zip(layers.last()) else {
        return 0;
    };
    if row < 0 {
        return top - i64::from(row);
    }
    match layers.get(row as usize) {
        Some(z) => *z,
        None => bottom - (i64::from(row) - (layers.len() as i64 - 1)),
    }
}

/// `resolveOverlaps` + `applyOverlapActions`: the clip list with `span`
/// cleared out of the lanes `zs` names.
///
/// One function rather than the web's plan-then-apply pair because nothing
/// here inspects the plan — the two halves exist on the web so a caller can
/// count what it is about to do, and no caller does. What survives is the
/// interesting part: a clip the region *partly* covers is trimmed or split
/// rather than deleted, and a remnant shorter than [`MIN_CLIP`] is dropped
/// instead of being left as a sliver nothing can grab.
fn clear_region(clips: &[Clip], span: (f64, f64), zs: &[i64]) -> Vec<Clip> {
    let (from, to) = span;
    let mut out = Vec::with_capacity(clips.len());
    for clip in clips {
        let touched = zs.contains(&clip.z) && clip.start < to && clip.end > from;
        if !touched {
            out.push(clip.clone());
            continue;
        }
        let (left, right) = (from - clip.start, clip.end - to);
        if left >= MIN_CLIP {
            let mut head = clip.clone();
            head.end = from;
            out.push(head);
        }
        if right >= MIN_CLIP {
            // The tail keeps the original id when the head did not take it, so
            // a clip merely trimmed at its start stays the same clip.
            let mut tail = if left >= MIN_CLIP {
                clip.copy(to, clip.end, clip.z)
            } else {
                clip.clone()
            };
            tail.start = to;
            out.push(tail);
        }
    }
    out
}

/// Resolve which lane each distinct `zIndex` sits in, exactly as the web
/// timeline's `rowMap` does: the values sorted ascending, then inverted so the
/// highest z is the *highest* lane on screen, and row 0 left empty as the
/// insertion lane above them. Rows are therefore 1-based.
fn rows_by_z(z: impl Iterator<Item = i64>) -> HashMap<i64, usize> {
    let mut z: Vec<i64> = z.collect();
    z.sort_unstable();
    z.dedup();
    let max_row = z.len().saturating_sub(1);
    z.iter()
        .enumerate()
        .map(|(index, value)| (*value, max_row - index + 1))
        .collect()
}

/// How many lanes the canvas draws: the occupied ones plus the empty insertion
/// lane above them. A press below the last of them is a press on nothing, and
/// the lane stripes stop there too — one rule, so the paint and the hit test
/// cannot come to disagree about where the floor is.
fn lane_count(clips: &[Clip]) -> usize {
    clips
        .iter()
        .map(|clip| clip.row + 1)
        .max()
        .unwrap_or(1)
        .max(1)
}

/// Resolve the clips a load or a write returned into what the canvas draws.
fn resolve(clips: &[TrackClip], patterns: &[PatternSummary]) -> Rc<[Clip]> {
    let rows = rows_by_z(clips.iter().map(|clip| clip.z_index));
    clips
        .iter()
        .map(|clip| Clip {
            id: clip.id.clone().into(),
            pattern: clip.pattern_id.clone().into(),
            label: label_of(&clip.pattern_id, patterns),
            color: ladder::pattern(&clip.pattern_id),
            start: clip.start_time,
            end: clip.end_time,
            row: rows.get(&clip.z_index).copied().unwrap_or(1),
            z: clip.z_index,
            blend: clip.blend_mode,
            args: clip.args.clone(),
        })
        .collect()
}

/// What a clip is called: its pattern's name, or the id spelled out for a
/// pattern the list does not have.
fn label_of(pattern_id: &str, patterns: &[PatternSummary]) -> SharedString {
    patterns
        .iter()
        .find(|pattern| pattern.id == pattern_id)
        .map_or_else(
            || format!("Pattern {pattern_id}"),
            |pattern| pattern.name.clone(),
        )
        .into()
}

// -- navigation, gestures and writes ------------------------------------------
//
// These hang off `Luma` because opening a track is five `Library` calls plus a
// screen transition, and `Luma` owns both.

impl Luma {
    /// Open a track's timeline as a workspace tab, from the sidebar row that
    /// named it. Reopening a track that already has a tab reveals it and
    /// reloads nothing: the tab's identity is its target.
    ///
    /// The five reads are started together and awaited in order: they do not
    /// depend on each other, and each is already its own task on the Tokio
    /// runtime, so sequencing the `await`s costs nothing and keeps the
    /// assignment in one place. The clips are the exception — they are keyed
    /// by a score id that only the first read knows.
    pub(crate) fn open_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let Some(track) = browser.find(track_id) else {
            return;
        };
        self.selected_track = Some(track.id.clone());
        // Picking the track *is* changing the strip's subject, and the tab
        // this gesture is about to open belongs to the arriving set. Syncing
        // here rather than waiting for the next draw is what keeps it out of
        // the outgoing track's remembered tabs — the swap is a no-op whenever
        // the scope has not actually moved, so paying for it here is free.
        self.sync_workspace_scope(cx);
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let target = Target::TrackEditor {
            track: track_id.to_string(),
            venue: venue_id.clone(),
        };
        if self.workspace.body_mut(&target).is_some() {
            self.workspace.select(&target);
            cx.notify();
            return;
        }

        let waveform = self.library.track_waveform(track_id);
        let beats = self.library.track_beats(track_id);
        let scores = self.library.scores_across_venues(track_id);
        let patterns = self.library.patterns();
        let audio = self.library.load_audio(track_id);

        let state = Box::new(Editor {
            track_id: track.id.clone(),
            track_name: track_title(&track),
            venue_id: venue_id.clone(),
            score: None,
            waveform: None,
            fine: None,
            fine_pending: None,
            beats: None,
            clips: Vec::new().into(),
            base: Vec::new().into(),
            patterns: Rc::new(Vec::new()),
            previews: Rc::new(RefCell::new(HashMap::new())),
            preview_inflight: HashSet::new(),
            preview_queued: HashSet::new(),
            history: History::default(),
            clipboard: None,
            loop_region: None,
            menu: None,
            menu_scroll: ScrollHandle::new(),
            selected: Vec::new(),
            cursor: None,
            follow: false,
            view: View {
                zoom: View::DEFAULT_ZOOM,
                scroll: 0.,
                zoom_y: 1.,
                lift: 0.,
            },
            transport: Transport::default(),
            gesture: None,
            canvas: Rc::new(Cell::new(Bounds::default())),
            anchor: None,
            seek_pending: None,
            seek_at: None,
            dirty: false,
            saving: false,
            error: None,
            loaded: false,
            score_chosen: false,
            composited: None,
            compositing: false,
            sheet: sheet::State::default(),
        });
        self.open_tab(target.clone(), move || Body::TrackEditor(state), cx);

        // The previews ride their own task because they are the one slow read
        // — the seam evaluates every clip's pattern over its span — and the
        // timeline is complete without them. A failure installs nothing and
        // says nothing: a library whose patterns have no graphs answers this
        // with an error, and that is a track with flat clip bodies, not a
        // broken screen.
        let previews = self.library.annotation_previews(track_id, &venue_id);
        let preview_target = target.clone();
        cx.spawn(async move |this, cx| {
            let Ok(rows) = previews.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.edit_track_tab(&preview_target, cx, |editor| editor.install_previews(rows));
            })
            .ok();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let waveform = waveform.await;
            let beats = beats.await;
            let patterns = patterns.await;
            let scores = scores.await;
            let audio = audio.await;

            let patterns = patterns.unwrap_or_default();

            this.update(cx, |this, cx| {
                let user = this.library.user_id();
                // Which score the timeline opens on, decided inside the tab
                // edit and acted on outside it — the clip read is a second
                // trip, and `edit_track_tab` holds the tab, not the app.
                let mut open = None;
                // Addressed to the tab the load was started for, not to
                // whichever tab is visible when it lands.
                this.edit_track_tab(&target, cx, |editor| {
                    editor.loaded = true;
                    match waveform {
                        Ok(waveform) => {
                            editor.transport.duration = waveform.duration_seconds as f32;
                            editor.waveform = Some(Rc::new(waveform));
                        }
                        Err(error) => editor.error = Some(error.to_string()),
                    }
                    editor.beats = beats.ok().flatten().map(Rc::new);
                    if let Err(error) = audio {
                        editor.error = Some(error.to_string());
                    }
                    match scores {
                        // The venue in hand, in the order the seam listed —
                        // `scores.updated_at DESC`, which in practice is
                        // creation order, because a clip write touches
                        // `track_scores` and never bumps the score row (see
                        // `list_scores_for_track`). Only when nothing has been
                        // chosen yet: a gesture that named a *particular*
                        // score got here first — or took one away — and this
                        // listing must not overrule either. See
                        // [`Editor::score_chosen`].
                        Ok(summaries) if !editor.score_chosen => {
                            open = crate::tracks::scores::rows(&summaries, user.as_deref())
                                .iter()
                                .find(|row| row.venue_id.as_ref() == editor.venue_id)
                                .map(crate::tracks::scores::open_row);
                        }
                        Ok(_) => {}
                        Err(error) => editor.error = Some(error.to_string()),
                    }
                    editor.patterns = Rc::new(patterns);
                    // The score may already be on the timeline: opening one
                    // from the sidebar's scores level gets there before this
                    // load does, and its clips resolved against an empty list.
                    editor.relabel();
                });
                if let Some(score) = open {
                    this.load_score(target.clone(), score, cx);
                }
                this.poll_transport(cx);
                // A long track at the opening zoom can already be past the
                // stored envelope's resolution, so the first measurement is
                // asked for with the waveform rather than waiting for a
                // gesture. It needs the canvas's width, which the first
                // prepaint supplies — this is a no-op before then and the
                // prepaint asks again.
                this.ensure_fine_waveform(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Show `score_id`'s clips on the timeline at `target`.
    ///
    /// The one way a score reaches the canvas: the first open takes it as
    /// soon as the listing lands, and the rail takes it again on every
    /// switch, so "which score am I looking at" has a single answer and a
    /// single code path that sets it.
    ///
    /// Everything that was *about* the outgoing score goes with it — the
    /// selection, the cursor, the undo stack, the clipboard. They name clip
    /// ids that the arriving score does not contain, and carrying them over
    /// would leave an undo that restores clips into a document they were
    /// never in.
    ///
    /// The rig follows. The stage is lit by *this* score
    /// ([`Editor::lit`]), so a switch here is a switch there — done by the
    /// stage's own reconcile rather than from inside this call, which knows
    /// nothing about whether a stage is even mounted.
    pub(crate) fn load_score(&mut self, target: Target, score: Score, cx: &mut Context<Self>) {
        // Whatever the outgoing score still owes goes out first. The write
        // names its own score, so it lands on the document it was made
        // against however long it takes to return.
        self.commit_clips(cx);
        let pending = self.library.track_scores(&score.id);
        let score_id = score.id.clone();
        self.edit_track_tab(&target, cx, |editor| rebase(editor, Some(score)));
        cx.spawn(async move |this, cx| {
            let clips = pending.await;
            this.update(cx, |this, cx| {
                this.edit_track_tab(&target, cx, |editor| {
                    // The tab may have moved on — onto another score, or off
                    // every score because this one was deleted while the read
                    // was out. Either way this answer is about a document the
                    // timeline is no longer showing, and neither its clips nor
                    // its error belong here.
                    if editor.score.as_ref().map(|open| open.id.as_str()) != Some(score_id.as_str())
                    {
                        return;
                    }
                    match clips {
                        Ok(clips) => {
                            let clips: Vec<TrackClip> = clips.iter().map(TrackClip::from).collect();
                            editor.clips = resolve(&clips, &editor.patterns);
                            editor.base = clips.into();
                            editor.composited = Some(Rc::clone(&editor.clips));
                        }
                        Err(error) => editor.error = Some(error.to_string()),
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Which score the tab for `(track, venue)` is showing, if there is one.
    ///
    /// The sidebar's scores level asks this to decide which row wears the
    /// selection ring: the open document is the editor's fact, not the
    /// listing's, and a second copy of it in the sidebar would be a second
    /// answer to the same question.
    /// Take the editor off `score_id` if that is what it is showing, leaving it
    /// in the same defined state a tab has before any score is chosen: no
    /// score, no clips, nothing selected, and — through
    /// [`Editor::lit`] — no scene on the rig.
    ///
    /// Called *before* the seam has answered the delete, deliberately. The
    /// alternative is a timeline that keeps drawing a document that no longer
    /// exists for as long as the round trip takes, and then blinks; "the score
    /// is gone" is true the moment the operator agreed to it.
    pub(crate) fn unload_score(&mut self, target: &Target, score_id: &str, cx: &mut Context<Self>) {
        let showing = match self.workspace.body(target) {
            Some(Body::TrackEditor(editor)) => editor
                .score
                .as_ref()
                .is_some_and(|open| open.id == score_id),
            _ => false,
        };
        if !showing {
            return;
        }
        self.edit_track_tab(target, cx, |editor| rebase(editor, None));
    }

    pub(crate) fn open_score_id(&self, track_id: &str, venue_id: &str) -> Option<String> {
        let target = Target::TrackEditor {
            track: track_id.to_string(),
            venue: venue_id.to_string(),
        };
        match self.workspace.body(&target)? {
            Body::TrackEditor(editor) => editor.score.as_ref().map(|score| score.id.clone()),
            _ => None,
        }
    }

    /// Run `edit` against one editor tab, wherever it sits in the strip. The
    /// async loads come through here so a waveform landing late cannot write
    /// into whichever tab happens to be visible.
    fn edit_track_tab(
        &mut self,
        target: &Target,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Editor),
    ) {
        if let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) {
            edit(editor);
            cx.notify();
        }
    }

    /// Start or stop playback, and read the transport back either way — the
    /// audio host is the authority on whether it is playing, so the button's
    /// own state is never assumed.
    pub(crate) fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body() else {
            return;
        };
        let playing = state.transport.playing;
        // Seeking first is what makes Play resume from a scrub the screen made
        // while stopped: the host plays from wherever *it* is, and a stopped
        // playhead only moved on screen.
        let steps: Vec<Transition> = if playing {
            vec![Box::pin(self.library.pause())]
        } else {
            vec![
                Box::pin(self.library.seek(state.transport.position)),
                Box::pin(self.library.play()),
            ]
        };
        cx.spawn(async move |this, cx| {
            let mut failed = None;
            for step in steps {
                if let Err(error) = step.await {
                    failed = Some(error.to_string());
                    break;
                }
            }
            this.update(cx, |this, cx| match failed {
                Some(message) => {
                    this.with_track_editor(cx, |editor| editor.error = Some(message));
                }
                None => this.poll_transport(cx),
            })
            .ok();
        })
        .detach();
    }

    /// Follow the playhead until it stops moving.
    ///
    /// The desktop app is told where the transport is by a `host-audio://state`
    /// event; nothing broadcasts one here, so this host asks. The loop runs
    /// only while something is playing and exits as soon as the host says it
    /// has stopped, so a parked editor costs nothing — and [`Transport::polling`]
    /// keeps a second play from starting a second loop.
    fn poll_transport(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.transport.polling {
            return;
        }
        state.transport.polling = true;
        let mut pending = self.library.transport_after(Duration::ZERO);
        cx.spawn(async move |this, cx| loop {
            let snapshot = pending.await;
            let again = this
                .update(cx, |this, cx| this.apply_transport(snapshot, cx))
                .unwrap_or(false);
            if !again {
                return;
            }
            let next = this.read_with(cx, |this, _| this.library.transport_after(POLL));
            match next {
                Ok(next) => pending = next,
                Err(_) => return,
            }
        })
        .detach();
    }

    /// Take one transport reading. Returns whether the loop should ask again —
    /// only while the editor is still up and the host is still playing.
    fn apply_transport(
        &mut self,
        snapshot: Result<HostAudioSnapshot, LibraryError>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return false;
        };
        let Ok(snapshot) = snapshot else {
            state.transport.polling = false;
            return false;
        };
        state.transport.playing = snapshot.is_playing;
        // A scrub in progress owns the playhead; adopting the host's position
        // under the pointer would fight the drag.
        if !matches!(state.gesture, Some(Gesture::Scrub)) {
            state.transport.position = snapshot.current_time;
        }
        // The loaded segment's length is the authority once audio is in hand:
        // a waveform's `durationSeconds` is the decoded file's, and the two
        // disagree by a frame or two.
        if snapshot.is_loaded && snapshot.duration_seconds > 0. {
            state.transport.duration = snapshot.duration_seconds;
        }
        state.transport.polling = snapshot.is_playing;
        state.follow_playhead();
        cx.notify();
        snapshot.is_playing
    }

    /// Toggle following the playhead, and take up the new setting at once —
    /// turning it on with the transport stopped should still centre what is
    /// already on screen.
    pub(crate) fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            editor.follow = !editor.follow;
            editor.follow_playhead();
        });
    }

    /// `Delete` / `Backspace`: clear the cursor's region, or remove the
    /// selected clips.
    pub(crate) fn delete_clips(&mut self, cx: &mut Context<Self>) {
        self.track_command(Editor::delete, cx);
    }

    /// `Cmd+E`: split every clip the cursor's time crosses.
    pub(crate) fn split_clips(&mut self, cx: &mut Context<Self>) {
        self.track_command(Editor::split, cx);
    }

    /// `Alt+Arrow`: move the selection one lane.
    pub(crate) fn move_clips_lane(&mut self, down: bool, cx: &mut Context<Self>) {
        self.track_command(|editor| editor.move_lane(down), cx);
    }

    /// `Cmd+C`. Not a write, so it does not go through [`Self::track_command`]
    /// — copying somebody else's score is reading it.
    pub(crate) fn copy_clips(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, Editor::copy);
    }

    pub(crate) fn cut_clips(&mut self, cx: &mut Context<Self>) {
        self.track_command(Editor::cut, cx);
    }

    pub(crate) fn paste_clips(&mut self, cx: &mut Context<Self>) {
        self.track_command(Editor::paste, cx);
    }

    pub(crate) fn duplicate_clips(&mut self, cx: &mut Context<Self>) {
        self.track_command(Editor::duplicate, cx);
    }

    /// `Cmd+Z` / `Cmd+Shift+Z`: step the timeline back, or forward again.
    pub(crate) fn undo_clips(&mut self, cx: &mut Context<Self>) {
        self.track_edit(
            |editor| {
                editor.undo();
            },
            cx,
        );
    }

    pub(crate) fn redo_clips(&mut self, cx: &mut Context<Self>) {
        self.track_edit(
            |editor| {
                editor.redo();
            },
            cx,
        );
    }

    /// `Cmd+L`: loop the cursor's range, or clear the loop it already
    /// describes.
    ///
    /// Not [`Self::track_command`]: a loop belongs to the transport rather
    /// than to the score, so it writes nothing, undoes nothing, and a
    /// read-only score can still be looped over.
    pub(crate) fn toggle_loop_region(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        let region = state.toggle_loop();
        cx.notify();
        let pending = self
            .library
            .set_loop_region(region.map(|(from, to)| (from as f32, to as f32)));
        cx.background_spawn(async move {
            pending.await.ok();
        })
        .detach();
    }

    /// Run one editing command against the working copy and publish whatever
    /// it left behind.
    ///
    /// Every keyboard verb goes through here, so "a command is a rewrite of
    /// the clip list followed by exactly one write" is stated once instead of
    /// at eleven call sites — and a read-only score refuses all of them in one
    /// place rather than eleven. The checkpoint is here for the same reason:
    /// undo is a property of *being* a command, not something eleven commands
    /// each have to remember.
    fn track_command(&mut self, command: impl FnOnce(&mut Editor), cx: &mut Context<Self>) {
        self.track_edit(
            |editor| {
                editor.checkpoint();
                command(editor);
                editor.abandon_checkpoint();
            },
            cx,
        );
    }

    /// Rewrite the working copy of a score this host may write to, and publish
    /// it. The step an undo takes is one of these that does *not* record a
    /// checkpoint — it is already moving along the stack.
    fn track_edit(&mut self, edit: impl FnOnce(&mut Editor), cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if !state.writable() {
            return;
        }
        self.with_track_editor(cx, edit);
        self.commit_clips(cx);
    }

    /// A right-click: work out where a clip would go and offer the patterns.
    ///
    /// `computeInsertionTarget`. The span is one bar — the next downbeat if
    /// there is one, the mean downbeat interval otherwise — and the vertical
    /// answer is two-valued: within a quarter of a lane of a *boundary* the
    /// gesture opens a new layer there, and anywhere else it drops onto the
    /// lane under the pointer.
    fn timeline_insert_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            editor.menu = None;
            if !editor.writable() {
                return;
            }
            let canvas = editor.canvas.get();
            let time = editor.view.time_at(f32::from(at.x - canvas.origin.x));
            let y = f32::from(at.y - canvas.origin.y);
            let layout = editor.layout();
            if y < layout.start.max(TRACK_AREA_Y) {
                return;
            }

            let beats = editor.beats.as_deref();
            let start = snap(beats, time, editor.view.zoom, SNAP_CAPTURE).max(0.);
            let end = (start + bar_length(beats, start)).min(f64::from(editor.transport.duration));
            if end - start < MIN_CLIP {
                return;
            }

            let layers = z_ladder(&editor.clips).len();
            let offset = ((y - layout.start) / layout.lane).max(0.);
            let boundary = offset.round();
            let insert = (offset - boundary).abs() < INSERT_BOUNDARY
                && (1. ..=layers as f32).contains(&boundary);
            editor.menu = Some(InsertMenu {
                start,
                end,
                row: if insert { boundary } else { offset.floor() } as usize,
                insert,
                active: 0,
            });
            editor.menu_scroll.scroll_to_item(0);
        });
    }

    /// A double-click on a clip opens its pattern's graph.
    ///
    /// The hit test is the whole lane row, not the header band: this is the
    /// one clip gesture that is not a drag, so there is nothing for the inert
    /// body to leave room for.
    fn timeline_open_pattern(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body() else {
            return;
        };
        let canvas = state.canvas.get();
        let time = state.view.time_at(f32::from(at.x - canvas.origin.x));
        let Some(row) = state.layout().row_at(f32::from(at.y - canvas.origin.y)) else {
            return;
        };
        let Some(clip) = state
            .clips
            .iter()
            .find(|clip| clip.row == row && time >= clip.start && time <= clip.end)
        else {
            return;
        };
        let Some(pattern) = state
            .patterns
            .iter()
            .find(|pattern| pattern.id == clip.pattern.as_ref())
            .cloned()
        else {
            return;
        };
        self.open_pattern(pattern, cx);
    }

    /// `ArrowUp` / `ArrowDown` in the insertion menu. A no-op with no menu
    /// open, which is what makes the bare arrows safe to bind at all: the web
    /// timeline leaves them unbound everywhere else, and so does this.
    pub(crate) fn step_insert_menu(&mut self, down: bool, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| editor.step_menu(down));
    }

    /// `Enter` in the insertion menu: put down whichever pattern the arrows
    /// left active, exactly as a click on that row would.
    pub(crate) fn commit_insert_menu(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body() else {
            return;
        };
        let Some((menu, pattern)) = state.menu_choice() else {
            return;
        };
        self.insert_pattern(menu, pattern, cx);
    }

    /// `H`: fit every lane on the canvas.
    pub(crate) fn fit_lanes(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, Editor::fit_lanes);
    }

    /// Close an open insertion menu, if there is one. What `Escape` means
    /// before it means anything else — [`Luma::dismiss_overlay`] asks first,
    /// so the key puts down what the editor has open before it touches an
    /// overlay.
    pub(crate) fn dismiss_insert_menu(&mut self) -> bool {
        match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(state)) => state.menu.take().is_some(),
            _ => false,
        }
    }

    /// Close whichever menu the args sheet has up, if it has one. `Escape`'s
    /// first business once nothing is floating over the screen.
    pub(crate) fn dismiss_sheet_menu(&mut self) -> bool {
        match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(state)) => state.sheet.dismiss_menu(),
            _ => false,
        }
    }

    /// Clear the clip selection, if there is one — which is what sends the
    /// args sheet away. Reported so `Escape` knows the key was spent.
    ///
    /// The sheet is not closed directly: it *is* the selection, rendered, and
    /// a second way to dismiss it would be a state the timeline's own
    /// highlight could disagree with.
    pub(crate) fn clear_clip_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let mut cleared = false;
        self.with_track_editor(cx, |editor| {
            cleared = editor.sheet.is_open();
            if cleared {
                editor.deselect();
            }
        });
        if cleared {
            cx.notify();
        }
        cleared
    }

    /// Commit an insertion on the pattern the pointer chose.
    fn insert_pattern(
        &mut self,
        menu: InsertMenu,
        pattern: PatternSummary,
        cx: &mut Context<Self>,
    ) {
        self.track_command(|editor| editor.insert(menu, &pattern), cx);
    }

    /// A press on the canvas: take the playhead, a clip, or a sweep of empty
    /// lane.
    ///
    /// The vertical dispatch is the web's, band by band. The ruler scrubs.
    /// Everything between it and the first lane — the waveform, and the empty
    /// insertion lane under it — clears the selection, which is the behavior
    /// that reads as surprising and is the one a person relies on to get back
    /// to nothing selected. Below the last lane does the same.
    fn timeline_press(&mut self, at: Point<Pixels>, keys: &Modifiers, cx: &mut Context<Self>) {
        let mut seek = None;
        let (shift, alt) = (keys.shift, keys.alt);
        self.with_track_editor(cx, |editor| {
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let time = editor.view.time_at(offset);
            let y = f32::from(at.y - canvas.origin.y);
            // A press anywhere dismisses an open insertion menu, the way the
            // web's full-screen backdrop does.
            editor.menu = None;

            if y < HEADER_HEIGHT {
                editor.gesture = Some(Gesture::Scrub);
                editor.transport.position =
                    time.clamp(0., f64::from(editor.transport.duration)) as f32;
                editor.follow_playhead();
                seek = Some(editor.transport.position);
                return;
            }

            let Some(row) = editor.layout().row_at(y) else {
                editor.deselect();
                return;
            };

            let Some(clip) = editor.clip_at(time, row, y) else {
                // Empty lane: a point cursor here, and a rectangle if the
                // pointer goes on to move.
                let start = snap(
                    editor.beats.as_deref(),
                    time,
                    editor.view.zoom,
                    SNAP_CAPTURE,
                );
                editor.selected.clear();
                editor.cursor = Some(Cursor {
                    row,
                    row_end: None,
                    start,
                    end: None,
                });
                editor.gesture = Some(Gesture::Marquee { row, start });
                return;
            };

            let (id, start, end) = (clip.id.clone(), clip.start, clip.end);
            let already = editor.selected.contains(&id);
            match (already, shift) {
                (false, false) => editor.selected = vec![id.clone()],
                (false, true) => editor.selected.push(id.clone()),
                (true, true) => editor.selected.retain(|selected| selected != &id),
                // An already-selected clip pressed without a modifier keeps
                // the whole selection, which is what lets a group be dragged
                // by any one of its members.
                (true, false) => {}
            }
            editor.cursor = Some(Cursor {
                row,
                row_end: None,
                start,
                end: Some(end),
            });
            // Read-only stops here: the selection is a view of the score, the
            // drag is a write to it.
            if !editor.writable() {
                return;
            }

            // Within a handle's width of either end, in *pixels* — a handle is
            // a fixed size on screen, so what it covers in seconds is a
            // function of the zoom.
            let handle = f64::from(HANDLE / editor.view.zoom);
            let drag = if time - start < handle {
                Drag::Resize(Edge::Start)
            } else if end - time < handle {
                Drag::Resize(Edge::End)
            } else {
                Drag::Move
            };
            // A press on an unselected clip drags that clip alone; a press on
            // a selected one drags everything selected with it.
            let held: Vec<SharedString> = if already {
                editor.selected.clone()
            } else {
                vec![id.clone()]
            };
            // `captureBeforeDrag`: the point an undo comes back to is where
            // the clips stood when the pointer took hold, not wherever a
            // mousemove last left them. Dropped again on release if the
            // gesture turned out to be a press.
            editor.checkpoint();
            // Alt on a move duplicates: the copies are minted where the clips
            // stand *now* and the originals are what the pointer takes away,
            // so the picture under the cursor is continuous and the copy is
            // the thing left behind.
            if alt && drag == Drag::Move {
                editor.clone_in_place(&held);
            }
            let initial: Rc<[Initial]> = editor
                .clips
                .iter()
                .filter(|clip| held.contains(&clip.id))
                .map(|clip| Initial {
                    id: clip.id.clone(),
                    start: clip.start,
                    end: clip.end,
                    row: clip.row,
                })
                .collect();
            editor.gesture = Some(Gesture::Clips {
                pressed: id,
                drag,
                origin: at,
                initial,
                layers: z_ladder(&editor.clips).into(),
                moved: false,
            });
        });
        if let Some(seconds) = seek {
            self.seek(seconds, cx);
        }
    }

    /// A pointer move. Registered on the window rather than on the canvas so a
    /// drag that wanders off it keeps tracking — hence the early return, which
    /// is what keeps an idle mouse anywhere in the app from redrawing this
    /// screen.
    fn timeline_drag(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) if state.gesture.is_some() => {}
            _ => return,
        }
        let mut seek = None;
        self.with_track_editor(cx, |editor| {
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let time = editor.view.time_at(offset);
            let y = f32::from(at.y - canvas.origin.y);
            // Taken out for the duration: a gesture describes what to do to
            // the editor, and doing it needs the editor whole.
            let Some(mut gesture) = editor.gesture.take() else {
                return;
            };
            if let Gesture::Clips { moved, .. } = &mut gesture {
                *moved = true;
            }
            match &gesture {
                Gesture::Scrub => {
                    editor.transport.position =
                        time.clamp(0., f64::from(editor.transport.duration)) as f32;
                    // The playhead moved, so a following eye owes it a
                    // re-centre — the transport poll does this while playing,
                    // and stopped there is no poll to do it.
                    editor.follow_playhead();
                    seek = Some(editor.transport.position);
                }
                &Gesture::Marquee { row, start } => {
                    let end = snap(
                        editor.beats.as_deref(),
                        time,
                        editor.view.zoom,
                        SNAP_CAPTURE,
                    );
                    let current = editor.layout().nearest_row(y);
                    let cursor = Cursor {
                        row,
                        row_end: (current != row).then_some(current),
                        start,
                        end: Some(end),
                    };
                    editor.cursor = Some(cursor);
                    if let Some(span) = cursor.span() {
                        editor.select_within(cursor.rows(), span);
                    }
                }
                &Gesture::Clips { origin, .. } => {
                    let delta = f64::from(f32::from(at.x - origin.x) / editor.view.zoom);
                    let rows = (f32::from(at.y - origin.y) / editor.layout().lane).round() as i32;
                    editor.drag_clips(&gesture, delta, rows);
                    editor.sync_cursor();
                }
            }
            editor.gesture = Some(gesture);
        });
        if let Some(seconds) = seek {
            self.scrub_seek(seconds, cx);
        }
    }

    /// A release. An edge that actually moved is written back; a press that
    /// only selected is not, because nothing changed.
    fn timeline_release(&mut self, cx: &mut Context<Self>) {
        match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) if state.gesture.is_some() => {}
            _ => return,
        }
        let mut save = false;
        let mut flush = None;
        self.with_track_editor(cx, |editor| {
            match editor.gesture.take() {
                Some(Gesture::Clips { .. }) => {
                    editor.abandon_checkpoint();
                    save = editor.dirty;
                }
                // A scrub owes the transport whatever the throttle was still
                // holding: the last position the pointer reached is the one
                // the audio has to land on, and it is the one most likely to
                // have been swallowed.
                Some(Gesture::Scrub) => flush = editor.seek_pending.take(),
                _ => {}
            }
        });
        if let Some(seconds) = flush {
            self.seek(seconds, cx);
        }
        if save {
            self.commit_clips(cx);
        }
    }

    /// A wheel notch over the canvas.
    ///
    /// A bare wheel scrolls both axes — the web's is the scroll container's,
    /// and this canvas has to stand in for one. A modified wheel zooms, and
    /// the horizontal zoom is anchored on a *latched* point: whatever was
    /// under the pointer when the gesture started stays under it until the
    /// wheel goes quiet for [`ANCHOR_IDLE`]. Recomputing the anchor per event
    /// is what lets a momentum flick walk it across the track.
    fn timeline_wheel(
        &mut self,
        at: Point<Pixels>,
        delta: Point<f32>,
        wheel: Wheel,
        cx: &mut Context<Self>,
    ) {
        self.with_track_editor(cx, |editor| {
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let rate = match wheel {
                Wheel::Scroll => {
                    editor.anchor = None;
                    let scroll = editor.view.scroll - delta.x;
                    editor.set_scroll(scroll);
                    editor.set_lift(editor.view.lift + delta.y);
                    return;
                }
                Wheel::Lanes => {
                    editor.anchor = None;
                    editor.zoom_lanes(
                        delta.y * View::ZOOM_Y_PER_PIXEL,
                        f32::from(at.y - canvas.origin.y),
                    );
                    return;
                }
                Wheel::Zoom(rate) => rate,
            };
            if delta.y == 0. {
                return;
            }
            let now = std::time::Instant::now();
            let anchor = match editor.anchor {
                Some(anchor) if now.duration_since(anchor.at) < ANCHOR_IDLE => anchor,
                _ => Anchor {
                    offset,
                    time: editor.view.time_at(offset),
                    at: now,
                },
            };
            // Exponential in the scroll distance, so a fast flick and a slow
            // one over the same distance land in the same place.
            editor.view.zoom =
                (editor.view.zoom * (delta.y * rate).exp()).clamp(View::MIN_ZOOM, View::MAX_ZOOM);
            let scroll = anchor.time as f32 * editor.view.zoom - anchor.offset;
            editor.set_scroll(scroll);
            editor.anchor = Some(Anchor { at: now, ..anchor });
        });
        self.ensure_fine_waveform(cx);
    }

    /// Move the transport from a scrub, at most once per [`SEEK_THROTTLE`].
    ///
    /// The playhead is already where the pointer put it — the picture costs
    /// nothing. What is being rationed is the round trip to the audio host,
    /// which a pointer walk would otherwise issue once a frame.
    fn scrub_seek(&mut self, seconds: f32, cx: &mut Context<Self>) {
        let mut send = false;
        self.with_track_editor(cx, |editor| {
            let now = std::time::Instant::now();
            match editor.seek_at {
                Some(last) if now.duration_since(last) < SEEK_THROTTLE => {
                    editor.seek_pending = Some(seconds);
                }
                _ => {
                    editor.seek_at = Some(now);
                    editor.seek_pending = None;
                    send = true;
                }
            }
        });
        if send {
            self.seek(seconds, cx);
        }
    }

    /// Measure the range on screen, if the stored envelope has run out of
    /// buckets for it and this measurement is not already in hand or in flight.
    ///
    /// Called from wherever the view can move — the two gestures that move it,
    /// and the prepaint that first tells the canvas how wide it is. The old
    /// measurement is kept until the new one lands, so a scrub or a pan draws
    /// the coarse envelope at worst and never a blank bed.
    /// Whether [`Self::ensure_fine_waveform`] would do anything, asked without
    /// a deferred round trip through the app — see [`Editor::fine_need`].
    fn wants_fine_waveform(&self) -> bool {
        match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) => !matches!(state.fine_need(), FineNeed::Nothing),
            _ => false,
        }
    }

    fn ensure_fine_waveform(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        let want = match state.fine_need() {
            FineNeed::Nothing => return,
            FineNeed::Discard => {
                state.fine = None;
                return;
            }
            FineNeed::Measure(want) => want,
        };
        state.fine_pending = Some(want);
        let duration = state
            .waveform
            .as_ref()
            .map_or(0., |waveform| waveform.duration_seconds);
        let pending = self.library.track_waveform_window(
            &state.track_id,
            want.start,
            want.end,
            want.buckets as u32,
        );
        cx.spawn(async move |this, cx| {
            let measured = pending.await;
            this.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.fine_pending = None;
                    // A failed measurement is not an error the screen shows:
                    // the coarse envelope is still a waveform, and the next
                    // view change asks again.
                    if let Ok(measured) = measured {
                        editor.fine = Some(Rc::new(Fine {
                            cut: Cut {
                                start: measured.start_seconds,
                                end: measured.end_seconds,
                                buckets: measured.bands.low.len(),
                                zoom: want.zoom,
                            },
                            duration,
                            bands: measured.bands,
                        }));
                    }
                });
                // The view may have moved on while this was in the air.
                this.ensure_fine_waveform(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Move the transport, optimistically: the playhead is already where the
    /// pointer put it, and the seek is what makes the audio agree.
    fn seek(&mut self, seconds: f32, cx: &mut Context<Self>) {
        let pending = self.library.seek(seconds);
        cx.background_spawn(async move {
            pending.await.ok();
        })
        .detach();
    }

    /// Publish the working copy: one compare-and-swap over the whole clip
    /// list, whatever the gesture or the command changed.
    ///
    /// **The only write this screen makes.** A gesture that moves five clips,
    /// splits three and deletes one is one write, so it cannot half land and
    /// the score never passes through a state the editor's own rules forbid.
    /// The candidate is refused if [`Editor::base`] is no longer what is
    /// stored, which is the whole of this host's conflict story: there is no
    /// merge, and the honest recovery is to say so and let a reopen re-read.
    ///
    /// One write is in flight at a time — a second issued against the same
    /// base would be refused by whichever landed later — and anything the user
    /// did meanwhile is still on [`Editor::dirty`] and goes out on its return.
    fn commit_clips(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        let Some(score) = state.score.as_ref().map(|score| score.id.clone()) else {
            return;
        };
        if state.saving || !state.dirty || !state.writable() {
            return;
        }
        let candidate: Vec<TrackClip> = state.clips.iter().map(Clip::to_track_clip).collect();
        state.dirty = false;
        if candidate == *state.base {
            return;
        }
        state.saving = true;
        state.error = None;
        // Which clips this write changes in a way their heatmap can see —
        // created, or moved/re-argued; a restack or a blend change draws the
        // same preview. Decided against the base *now*, because by the time
        // the write lands the base is the candidate; renders are issued only
        // once the write has, under whatever ids the seam minted.
        let previous: HashMap<&str, &TrackClip> = state
            .base
            .iter()
            .map(|clip| (clip.id.as_str(), clip))
            .collect();
        let stale: Vec<String> = candidate
            .iter()
            .filter(|clip| {
                previous.get(clip.id.as_str()).map_or(true, |base| {
                    base.pattern_id != clip.pattern_id
                        || base.start_time != clip.start_time
                        || base.end_time != clip.end_time
                        || base.args != clip.args
                })
            })
            .map(|clip| clip.id.clone())
            .collect();
        let removed: Vec<String> = state
            .base
            .iter()
            .filter(|clip| !candidate.iter().any(|kept| kept.id == clip.id))
            .map(|clip| clip.id.clone())
            .collect();
        drop(previous);
        // Minted per *write*, not per attempt: this id is what lets the seam
        // replay a durable outcome rather than guess from a later snapshot.
        let operation = uuid::Uuid::new_v4().to_string();
        let pending = self.library.replace_clips(
            &score,
            &state.track_id,
            &rows_of(&score, &state.base),
            &rows_of(&score, &candidate),
            &operation,
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut again = false;
                let mut reload = false;
                let mut refresh: Vec<SharedString> = Vec::new();
                this.with_track_editor(cx, |editor| {
                    editor.saving = false;
                    // A write belongs to the score it was made against. The
                    // rail can put a different one on the timeline while this
                    // is in the air, and adopting the returned list then would
                    // overwrite the arriving score with the outgoing one's.
                    if editor.score.as_ref().map(|open| open.id.as_str()) != Some(score.as_str()) {
                        return;
                    }
                    match result {
                        Ok(saved) => {
                            // The stored edit is what makes these renders
                            // worth having: under the seam's ids for the
                            // creates, and with the deleted clips' pictures
                            // let go.
                            refresh = stale
                                .iter()
                                .map(|id| {
                                    saved
                                        .id_map
                                        .get(id)
                                        .cloned()
                                        .unwrap_or_else(|| id.clone())
                                        .into()
                                })
                                .collect();
                            let mut previews = editor.previews.borrow_mut();
                            for id in &removed {
                                previews.remove(id.as_str());
                            }
                            drop(previews);
                            editor.base = saved.clips.clone().into();
                            editor.adopt_ids(&saved.id_map);
                            // The authoritative list is adopted whole only
                            // when nothing is outstanding — under the pointer
                            // it would undo a drag the user can see, and with
                            // an edit still queued it would undo that.
                            if !editor.dirty && editor.gesture.is_none() {
                                editor.clips = resolve(&saved.clips, &editor.patterns);
                            }
                        }
                        // The stale-base refusal is the one failure with a
                        // recovery. Every later write would lose the same race
                        // while the base stays stale, and there is no merge —
                        // so the stored list is re-read and this write is
                        // dropped, said plainly rather than left to a reopen.
                        Err(error) => match error.command() {
                            Some(CommandError::Conflict { .. }) => {
                                reload = true;
                                editor.error = Some(WRITE_CONFLICT.into());
                            }
                            _ => editor.error = Some(error.to_string()),
                        },
                    }
                    if reload {
                        // Anything queued was edited against the same stale
                        // base, so it goes with the rest of the working copy.
                        editor.dirty = false;
                    }
                    again = editor.dirty;
                });
                for id in refresh {
                    this.refresh_clip_preview(id, cx);
                }
                if reload {
                    this.reload_clips(cx);
                } else if again {
                    this.commit_clips(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the stored clip list into the open editor, after a write lost
    /// the race.
    ///
    /// The screen is kept — the waveform, the beats, the transport and the
    /// message explaining what happened all survive; only the clips and the
    /// base they are compared against are replaced.
    fn reload_clips(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body() else {
            return;
        };
        let Some(score) = state.score.as_ref().map(|score| score.id.clone()) else {
            return;
        };
        let pending = self.library.track_scores(&score);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| match result {
                    Ok(rows) => {
                        let clips: Vec<TrackClip> = rows.iter().map(TrackClip::from).collect();
                        editor.clips = resolve(&clips, &editor.patterns);
                        editor.base = clips.into();
                    }
                    Err(error) => editor.error = Some(error.to_string()),
                });
            })
            .ok();
        })
        .detach();
    }

    /// Run `edit` against the track editor, if that is still what is showing.
    /// A load or a write that lands after the user navigated away is a no-op.
    /// Re-render one clip's heatmap from the working copy's row.
    ///
    /// Coalesced per clip, the way the web store's `updatePreview` is: an edit
    /// landing while this clip's render is in flight queues one trailing
    /// re-issue instead of a backlog, and because the render reads the clip's
    /// *current* state when it is issued, that one trailing render covers
    /// everything the burst did.
    fn refresh_clip_preview(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.preview_inflight.contains(&id) {
            state.preview_queued.insert(id);
            return;
        }
        // A clip deleted since the edit that asked has no body to preview.
        let Some(clip) = state.clips.iter().find(|clip| clip.id == id) else {
            return;
        };
        let (pattern, start, end, args) = (
            clip.pattern.clone(),
            clip.start,
            clip.end,
            clip.args.clone(),
        );
        state.preview_inflight.insert(id.clone());
        let pending = self.library.preview_clip(
            &state.track_id,
            &state.venue_id,
            &id,
            &pattern,
            start,
            end,
            &args,
        );
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut again = false;
                this.with_track_editor(cx, |editor| {
                    editor.preview_inflight.remove(&id);
                    again = editor.preview_queued.remove(&id);
                    // A render that failed keeps the previous thumbnail — a
                    // stale picture over a truthful clip box, exactly what the
                    // screen shows while a slow render is still on its way.
                    if let Ok(row) = result {
                        editor.install_preview(row);
                    }
                });
                if again {
                    this.refresh_clip_preview(id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn with_track_editor(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Editor)) {
        if let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() {
            edit(state);
            cx.notify();
        }
    }
}

/// One step of a transport change. Boxed because a play is two commands and a
/// pause is one, and the two arms of that choice are different opaque future
/// types that only a `dyn` can hold in one list.
pub(crate) type Transition =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LibraryError>>>>;

/// What a lost race says. The reload it describes is automatic, so the
/// sentence has to account for the edit that went missing with it.
const WRITE_CONFLICT: &str =
    "another writer saved this score first — reloaded, and this change was not kept";

/// How often the playhead is re-read while playing. 30 Hz — twice the rate the
/// desktop app's broadcaster emits at, because a poll's phase is arbitrary and
/// halving the period halves the worst-case lag.
const POLL: Duration = Duration::from_millis(33);

/// The web browser's fallback chain for a track with no title.
fn track_title(track: &TrackBrowserRow) -> String {
    if let Some(title) = track.title.as_ref().filter(|title| !title.is_empty()) {
        return title.clone();
    }
    std::path::Path::new(&track.file_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| track.file_path.clone())
}

// -- geometry -----------------------------------------------------------------
//
// `utils/timeline-constants.ts`, at `zoomY == 1` — which is the only zoom the
// ruler and the waveform have. Vertical zoom scales the lanes and nothing
// else, so these are constants and [`Layout`] is what varies.

/// The ruler strip: `HEADER_HEIGHT`.
const HEADER_HEIGHT: f32 = 32.;
/// `WAVEFORM_HEIGHT`. Fixed even under vertical zoom on the web too — it is a
/// navigation surface, not part of the annotation workspace.
const WAVEFORM_HEIGHT: f32 = 80.;
/// `TRACK_HEIGHT`: one lane.
const LANE_HEIGHT: f32 = 80.;
/// `layout.trackAreaY`: where the scrubbing surface ends and the lanes begin.
const TRACK_AREA_Y: f32 = HEADER_HEIGHT + WAVEFORM_HEIGHT;
/// `ANNOTATION_HEADER_H`: the opaque strip at the top of a clip.
const CLIP_HEADER: f32 = 18.;
/// The grab width of a clip's edge, in screen pixels. `handleSize` in
/// `components/timeline.tsx`.
const HANDLE: f32 = 8.;
/// The ruler's and the clip labels' type size.
const LABEL_SIZE: f32 = 10.;

/// Where the lane block sits on a canvas of a given height.
///
/// `computeBottomAnchoredLayout`: the lanes are pinned to the **floor** of the
/// viewport and grow upward, so z = 0 — the layer everything else is stacked
/// over — is always at the bottom edge and a new layer appears above what is
/// already there rather than pushing it down. Once there are more lanes than
/// the canvas is tall the ones that do not fit run off the *top*, under the
/// waveform, and [`View::lift`] is what reaches them: still bottom-anchored,
/// which is the whole point of anchoring it there.
///
/// Above lane 0 — the empty insertion lane — sits one further lane of dead
/// air, which is the web's `trackStartY = trackAreaY + trackHeight`. It scales
/// with the lanes because it is one of them in every arithmetic that counts
/// the content's height.
#[derive(Clone, Copy)]
struct Layout {
    /// The top of lane 0, the empty insertion lane. Negative once the lanes
    /// overflow the canvas and the block is sitting on the floor.
    start: f32,
    /// One lane's height: `round(TRACK_HEIGHT * zoomY)`.
    lane: f32,
    rows: usize,
    /// The furthest the lanes may be lifted off the floor before the topmost
    /// one is fully on screen. Zero whenever they all fit.
    max_lift: f32,
}

impl Layout {
    fn new(rows: usize, height: f32, view: View) -> Self {
        let lane = (LANE_HEIGHT * view.zoom_y).round();
        let natural = TRACK_AREA_Y + (rows + 1) as f32 * lane;
        let max_lift = (natural - height).max(0.);
        Self {
            start: height - rows as f32 * lane + view.lift.clamp(0., max_lift),
            lane,
            rows,
            max_lift,
        }
    }

    /// The top of one lane.
    fn top(self, row: usize) -> f32 {
        self.start + row as f32 * self.lane
    }

    /// The bottom edge of the lowest lane — the floor z = 0 sits on.
    fn floor(self) -> f32 {
        self.top(self.rows)
    }

    /// How many lanes there are between the floor and a point on the canvas.
    ///
    /// The anchor a vertical zoom holds: the lanes grow from the floor, so the
    /// fraction of a lane under the pointer is what has to survive a change of
    /// lane height.
    fn rows_from_floor(self, y: f32) -> f32 {
        (self.floor() - y) / self.lane
    }

    /// Which lane a point `y` pixels down the canvas falls in, or `None` above
    /// the lanes and below the last of them.
    ///
    /// The waveform is the ceiling as well as [`Self::start`]: a lifted block
    /// runs *under* it, and a lane whose arithmetic reaches up there is one
    /// the pointer cannot see and must not answer for.
    fn row_at(self, y: f32) -> Option<usize> {
        if y < self.start.max(TRACK_AREA_Y) {
            return None;
        }
        let row = ((y - self.start) / self.lane) as usize;
        (row < self.rows).then_some(row)
    }

    /// The nearest lane to a point, for a gesture that may not have started
    /// on one — a marquee dragged off the top or the bottom still has a row.
    fn nearest_row(self, y: f32) -> usize {
        if y < self.start {
            return 0;
        }
        (((y - self.start) / self.lane) as usize).min(self.rows.saturating_sub(1))
    }

    /// The band of the canvas the lanes are allowed to paint in and answer the
    /// pointer from: everything below the fixed navigation surface.
    fn band(self, canvas: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(TRACK_AREA_Y)),
            size: size(
                canvas.size.width,
                (canvas.size.height - px(TRACK_AREA_Y)).max(px(0.)),
            ),
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// Keep the render engine's installed scene in step with the working copy.
///
/// The visualizer samples an *installed* scene ([`Library::sample_universe`]),
/// and installing is a command — so without this, every edit the timeline made
/// was invisible on the rig until the view was re-opened. Reconciled rather
/// than issued per gesture: the render pass is the one place every path that
/// can change a clip has already converged, so no edit site has to remember to
/// invalidate, and a frame's worth of picker motion is one install.
///
/// The comparison is the *scene's* inputs, not the list's identity: a write
/// landing re-resolves the working copy into an equal list, and re-compiling
/// for that would put one composite behind every save.
///
/// Which *document* is installed is not this reconcile's question — a score
/// switch is the stage's ([`crate::visualizer::Visualizer::relight`]), which
/// is also the only path that survives the editor tab not being the one on
/// screen. This keeps the open score's scene abreast of edits to it.
fn sync_composite(editor: &mut Editor, cx: &mut Context<Luma>) {
    if editor.compositing {
        return;
    }
    let Some(last) = editor.composited.as_ref() else {
        return;
    };
    if same_scene(last, &editor.clips) {
        return;
    }
    let sent = Rc::clone(&editor.clips);
    let clips: Vec<TrackClip> = sent.iter().map(Clip::to_track_clip).collect();
    // Addressed to the score, so a working copy can never land on the document
    // next to the one it was edited against.
    let Some(score) = editor.score.as_ref().map(|score| score.id.clone()) else {
        return;
    };
    editor.compositing = true;
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| {
            this.library.composite_score(&score, Some(clips))
        }) else {
            return;
        };
        pending.await.ok();
        this.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                editor.compositing = false;
                // What was sent, not what is current: an edit made while this
                // was in flight is exactly what the next reconcile must see.
                editor.composited = Some(sent);
            });
        })
        .ok();
    })
    .detach();
}

/// Do these two lists compile to the same scene?
///
/// Every field the compositor reads and no others — a clip's lane, label and
/// colour are pictures of it, and moving one lights nothing differently.
fn same_scene(a: &[Clip], b: &[Clip]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.id == b.id
                && a.pattern == b.pattern
                && a.start == b.start
                && a.end == b.end
                && a.z == b.z
                && a.blend == b.blend
                && a.args == b.args
        })
}

/// Render the screen: a toolbar strip over the canvas.
///
/// The same split as the graph editor, and for the same reason — a panel is a
/// stack of boxes and gpui lays boxes out well, while a timeline is a
/// coordinate system nothing gpui lays out could express without a box per
/// clip per beat.
pub fn track_editor(
    state: &mut Editor,
    app: &Entity<Luma>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Div {
    // Reconcile the args sheet against the selection before drawing it — the
    // render pass is where every path that can move the subject converges,
    // and the one place a `Window` is in hand to build its entities and to
    // ask for the slide's next frame.
    let sheet_revealed = sheet::sync(state, window, cx);
    // …and the rig against the working copy, for the same reason: an edit is
    // only real once the scene it changed has been installed.
    sync_composite(state, cx);
    let state = &*state;
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        // An error only takes the screen when there is nothing behind it to
        // show. A *write* that came back refused — a stale base, an overlap
        // the seam forbids — leaves a perfectly good timeline on screen, and
        // replacing it with a sentence would throw away the picture the user
        // needs in order to understand the refusal. Those read out on the
        // toolbar instead.
        .child(match (&state.error, state.waveform.is_some()) {
            (Some(message), false) => luma_ui::plate(message.clone(), ladder::danger()),
            (None, false) => {
                luma_ui::plate("Loading track…".to_string(), ladder::muted_foreground())
            }
            (_, true) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .relative()
                .overflow_hidden()
                .child(canvas_element(state, app))
                .children(state.menu.map(|target| insert_menu(state, target, app)))
                // The args inspector, over the timeline's right edge and
                // inside the tab rather than on the app's overlay plane: it
                // belongs to this screen, and it must not outlive a tab swap
                // or paint over the shell's chrome.
                .children(sheet::sheet(state, sheet_revealed, app))
                .into_any_element(),
        })
}

/// The patterns a right-click offers, and where its clip would land.
///
/// A docked drawer rather than a popover at the pointer: the *target* is drawn
/// on the canvas as a ghost, which is where the eye needs it, and a list that
/// follows the pointer would be a second positioning system for one menu. The
/// web anchors the menu itself — that difference is deliberate, and the
/// insertion it commits is identical.
fn insert_menu(state: &Editor, target: InsertMenu, app: &Entity<Luma>) -> Div {
    div()
        .absolute()
        // Opaque to the pointer: the canvas listens for presses over its whole
        // hitbox and dismisses the menu on any of them, and a *Normal* hitbox
        // stacked on top does not stop that — so the press that chose an item
        // would tear the item down before its own click could land.
        .occlude()
        .top_0()
        .right_0()
        .w(px(220.))
        .max_h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(ladder::control())
        .border_1()
        .border_color(ladder::control_border())
        .child(luma_ui::silkscreen("INSERT PATTERN".to_string()).into_any_element())
        // A real scroller under the drawer's header, gutters outside it
        // (`float::viewport`'s contract): the library outgrows the canvas, and
        // `step_menu`'s `scroll_to_item` keeps the row `Enter` would commit on
        // screen instead of clipped below the drawer's edge.
        .child(
            float::viewport()
                // Content-sized and shrinkable, not `flex_1`: a dialog card
                // hands the viewport a definite height to fill, while this
                // drawer's height IS its content — a zero flex basis (or the
                // list's own `h_full`, a percentage of this auto-height
                // wrapper) collapses the rows. Past `max_h_full` the shrink
                // engages and the scroller takes over.
                .flex_initial()
                .flex()
                .flex_col()
                .child(
                    float::list()
                        .id("insert-menu-list")
                        .h_auto()
                        .min_h_0()
                        .flex_initial()
                        .overflow_y_scroll()
                        .track_scroll(&state.menu_scroll)
                        .children(state.patterns.iter().enumerate().map(|(index, pattern)| {
                            let app = app.clone();
                            let chosen = pattern.clone();
                            let name: SharedString = pattern.name.clone().into();
                            // The active row wears the ladder's hover fill, so
                            // what `Enter` would commit is the row a pointer
                            // would be over — one affordance for "this one",
                            // whichever moved it there.
                            luma_ui::luma_button(&pattern.name, Enabled::Yes)
                                .when(index == target.active, |el| el.bg(ladder::hover()))
                                .id(SharedString::from(format!("insert-{}", pattern.id)))
                                .w_full()
                                .on_click(move |_, _, cx| {
                                    let chosen = chosen.clone();
                                    app.update(cx, |this, cx| {
                                        this.insert_pattern(target, chosen, cx);
                                    });
                                })
                                .agent_node(Role::Row, name)
                        })),
                ),
        )
}

/// The way back, what is open, the transport, and whether a write is in the
/// air.
fn toolbar(state: &Editor, app: &Entity<Luma>) -> Div {
    let transport = app.clone();
    let playing = state.transport.playing;
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(16.))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.track_name.clone())
                .agent_node(Role::Text, state.track_name.clone()),
        )
        .child(
            luma_ui::luma_button(if playing { "Pause" } else { "Play" }, Enabled::Yes)
                .id("transport")
                // One button, two labels — the label *is* the state, so a
                // script reads what the transport is doing from the same place
                // a person does.
                .on_click(move |_, _, cx| transport.update(cx, |this, cx| this.toggle_playback(cx)))
                .agent_node(Role::Button, if playing { "Pause" } else { "Play" }),
        )
        .child(luma_ui::silkscreen(format!(
            "{} / {}",
            clock(state.transport.position),
            clock(state.transport.duration)
        )))
        .child(luma_ui::silkscreen(format!("{} CLIPS", state.clips.len())))
        .when(!state.selected.is_empty(), |el| {
            el.child(luma_ui::silkscreen(format!(
                "{} SELECTED",
                state.selected.len()
            )))
        })
        // The cursor's own readout: a point reads as one time, a range as the
        // span it covers. It is the only account of where an edit would land,
        // which on the web is left to the picture alone.
        .when_some(state.cursor, |el, cursor| {
            el.child(luma_ui::silkscreen(match cursor.span() {
                Some((from, to)) => format!("CURSOR {from:.2}-{to:.2}"),
                None => format!("CURSOR {:.2}", cursor.start),
            }))
        })
        .when_some(state.loop_region, |el, (from, to)| {
            el.child(luma_ui::silkscreen(format!("LOOP {from:.2}-{to:.2}")))
        })
        .when(state.follow, |el| {
            el.child(luma_ui::silkscreen("FOLLOW".to_string()))
        })
        // How many buckets the canvas actually has, once that is more than the
        // stored envelope carries — the way an instrument reads out its own
        // range rather than leaving you to guess it.
        //
        // A readout and *only* a readout: the picture is the same three bands
        // in the same units either side of the threshold, so this is the one
        // place the source shows.
        .when_some(state.drawn_buckets(), |el, buckets| {
            el.child(luma_ui::silkscreen(format!("FINE {buckets}")))
        })
        .child(div().flex_1())
        // Which score is on the timeline, by the handle the sidebar names it
        // by.
        .when_some(state.score.as_ref(), |el, score| {
            el.child(luma_ui::silkscreen(format!("SCORE #{}", score.ordinal)))
        })
        .when(state.score.is_none() && state.loaded, |el| {
            el.child(luma_ui::silkscreen("NO SCORE".to_string()))
        })
        .when(state.writable() && (state.saving || state.dirty), |el| {
            el.child(luma_ui::silkscreen("SAVING".to_string()))
        })
        // A refused write, over the timeline it was refused for.
        .when_some(
            state.error.clone().filter(|_| state.waveform.is_some()),
            |el, message| {
                el.child(
                    div()
                        .text_size(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(ladder::danger())
                        .child(message.clone())
                        .agent_node(Role::Text, message),
                )
            },
        )
        .when(
            state.score.as_ref().is_some_and(|score| score.read_only),
            |el| el.child(luma_ui::silkscreen("READ ONLY".to_string())),
        )
}

/// `M:SS`, the same clock the browser's TIME column reads in.
pub(crate) fn clock(seconds: f32) -> String {
    let total = if seconds.is_finite() {
        seconds.max(0.)
    } else {
        0.
    };
    format!("{}:{:02}", (total / 60.) as u64, (total % 60.) as u64)
}

/// One element for the whole timeline.
///
/// Everything the paint needs is captured by value — one refcounted clip list,
/// one refcounted waveform, one `Copy` view — so a frame draws a consistent
/// picture without reaching back into the app to ask what it looks like. The
/// pointer handlers do the reverse: they carry no picture at all, only the
/// entity to send the gesture to, because by the time one runs the frame it
/// was registered in is already gone.
fn canvas_element(state: &Editor, app: &Entity<Luma>) -> impl IntoElement {
    let scene = Scene {
        clips: Rc::clone(&state.clips),
        previews: Rc::clone(&state.previews),
        waveform: state.waveform.clone(),
        fine: state.fine.clone(),
        beats: state.beats.clone(),
        view: state.view,
        playhead: state.transport.position,
        selected: state.selected.clone(),
        cursor: state.cursor,
        loop_region: state.loop_region,
        menu: state.menu,
    };
    let registered = scene.clone();
    let canvas_bounds = Rc::clone(&state.canvas);
    let app = app.clone();
    let resized = app.clone();

    div().flex_1().overflow_hidden().child(
        canvas(
            move |bounds, window, cx| {
                // Where the canvas ended up is what turns a window-space mouse
                // position back into a time, and only prepaint knows it. A
                // press can arrive before the next paint but never before the
                // next prepaint, so this is also the only place it is safe to
                // write.
                //
                // How *wide* it is is also what a fine window is measured in,
                // so a width this canvas has not seen before is the other
                // moment one has to be asked for. Deferred, because a draw may
                // not notify — and gated on the window in hand not answering
                // the new width, because a sidebar slide hands this a new width
                // every frame and all but a few of them are already covered.
                let widened = canvas_bounds.replace(bounds).size.width != bounds.size.width;
                if widened && resized.read(cx).wants_fine_waveform() {
                    let resized = resized.clone();
                    cx.defer(move |cx| {
                        resized.update(cx, |this, cx| this.ensure_fine_waveform(cx));
                    });
                }
                register(&registered, bounds, window, cx);
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, cx| {
                paint(bounds, &scene, window, cx);
                listen(&app, &hitbox, window);
            },
        )
        .size_full(),
    )
}

/// Everything one frame draws, resolved and refcounted.
#[derive(Clone)]
struct Scene {
    clips: Rc<[Clip]>,
    previews: Rc<RefCell<HashMap<SharedString, Preview>>>,
    waveform: Option<Rc<TrackWaveform>>,
    fine: Option<Rc<Fine>>,
    beats: Option<Rc<BeatGrid>>,
    view: View,
    playhead: f32,
    selected: Vec<SharedString>,
    cursor: Option<Cursor>,
    loop_region: Option<(f64, f64)>,
    menu: Option<InsertMenu>,
}

impl Scene {
    /// Where the lanes sit on this canvas. Derived rather than carried: it is
    /// a function of the clip list and the height, and a frame that stored it
    /// could disagree with the frame that drew it.
    fn layout(&self, canvas: Bounds<Pixels>) -> Layout {
        Layout::new(
            lane_count(&self.clips),
            f32::from(canvas.size.height),
            self.view,
        )
    }

    /// One clip's box in window space.
    fn clip_box(&self, canvas: Bounds<Pixels>, clip: &Clip) -> Bounds<Pixels> {
        let x = self.view.x_of(clip.start);
        let width = ((clip.end - clip.start) as f32 * self.view.zoom)
            .floor()
            .max(4.);
        let layout = self.layout(canvas);
        Bounds {
            origin: point(
                canvas.origin.x + px(x),
                canvas.origin.y + px(layout.top(clip.row) + 1.),
            ),
            size: size(px(width), px(layout.lane - 2.)),
        }
    }

    fn playhead_box(&self, canvas: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: point(
                canvas.origin.x + px(self.view.x_of(f64::from(self.playhead))),
                canvas.origin.y,
            ),
            size: size(px(1.), canvas.size.height),
        }
    }
}

/// Name what a script can act on: the scrubbing surface, every clip, both of
/// every clip's edge handles, and the playhead.
///
/// The handles are their own nodes because a clip's *centre* — which is where
/// the harness clicks and where a drag starts from — is nowhere near either
/// edge, so a script could not otherwise reach the one control this screen
/// exists to offer.
fn register(scene: &Scene, canvas: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    // Two surfaces, not one: the ruler scrubs and the waveform clears the
    // selection, so a script that means to move the playhead has somewhere to
    // press that is not the waveform.
    agent_paint_node(
        Role::Card,
        "Ruler",
        Bounds {
            origin: canvas.origin,
            size: size(canvas.size.width, px(HEADER_HEIGHT)),
        },
        window,
        cx,
    );
    agent_paint_node(
        Role::Card,
        "Waveform",
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(HEADER_HEIGHT)),
            size: size(canvas.size.width, px(WAVEFORM_HEIGHT)),
        },
        window,
        cx,
    );
    // Everything below is on the lane surface, which the fixed navigation
    // surface above it covers once the lanes are taller than the canvas. The
    // mask is what tells a script so: a node the waveform hides collapses to
    // no height, which is already the harness's word for "there is no point on
    // screen that would hit this".
    let layout = scene.layout(canvas);
    window.with_content_mask(
        Some(ContentMask {
            bounds: layout.band(canvas),
        }),
        |window| {
            // Each lane as a row, so the empty space between clips is
            // addressable. A press there is a real gesture — it sweeps a range
            // — and a surface a script cannot name is a gesture it cannot make.
            for lane in 0..layout.rows {
                agent_paint_node(
                    Role::Row,
                    format!("Lane {lane}"),
                    Bounds {
                        origin: point(canvas.origin.x, canvas.origin.y + px(layout.top(lane))),
                        size: size(canvas.size.width, px(layout.lane)),
                    },
                    window,
                    cx,
                );
            }
            for clip in scene.clips.iter() {
                let box_ = scene.clip_box(canvas, clip);
                // The *grabbable* extent, not the drawn one: only a clip's
                // header bar answers the pointer, so a node covering its whole
                // lane would send every scripted click into the inert body.
                let header = Bounds {
                    origin: box_.origin,
                    size: size(box_.size.width, px(CLIP_HEADER)),
                };
                agent_paint_node(Role::Card, clip.label.clone(), header, window, cx);
                // The body stays inert to the pointer; this node is evidence,
                // not a control. It exists exactly when the clip has a decoded
                // heatmap, which is the only way a headless probe can tell a
                // preview surface from the flat fallback fill.
                if scene.previews.borrow().contains_key(&clip.id) {
                    agent_paint_node(
                        Role::Card,
                        format!("{} preview", clip.label),
                        clip_body(box_),
                        window,
                        cx,
                    );
                }
                for edge in [Edge::Start, Edge::End] {
                    let x = match edge {
                        Edge::Start => box_.origin.x,
                        Edge::End => box_.origin.x + box_.size.width - px(HANDLE),
                    };
                    agent_paint_node(
                        Role::Slider,
                        format!("{} {}", clip.label, edge.suffix()),
                        Bounds {
                            origin: point(x, header.origin.y),
                            size: size(px(HANDLE), header.size.height),
                        },
                        window,
                        cx,
                    );
                }
            }
            // The cursor is a control in the sense that matters here: it is
            // where the next edit lands, and nothing else on the canvas
            // reports it.
            if let Some(cursor) = scene.cursor {
                let (min_row, max_row) = cursor.rows();
                let (from, to) = cursor.span().unwrap_or((cursor.start, cursor.start));
                let left = scene.view.x_of(from);
                agent_paint_node(
                    Role::Slider,
                    "Cursor",
                    Bounds {
                        origin: point(
                            canvas.origin.x + px(left),
                            canvas.origin.y + px(layout.top(min_row)),
                        ),
                        size: size(
                            px((scene.view.x_of(to) - left).max(2.)),
                            px((max_row - min_row + 1) as f32 * layout.lane),
                        ),
                    },
                    window,
                    cx,
                );
            }
        },
    );
    // A slider is the closest thing in the closed role vocabulary to a mark
    // whose position along an axis *is* its value, which is what a script
    // watches to know where the transport got to.
    agent_paint_node(
        Role::Slider,
        "Playhead",
        scene.playhead_box(canvas),
        window,
        cx,
    );
}

/// Register this frame's pointer handlers.
///
/// Press and scroll are scoped to the canvas's hitbox; move and release are
/// not. A drag that wanders off the canvas must keep tracking, and must end
/// when the button comes up wherever that happens — see the same note in
/// `graph.rs`, which this mirrors.
fn listen(app: &Entity<Luma>, hitbox: &Hitbox, window: &mut Window) {
    let pressed = app.clone();
    let inside = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !inside.is_hovered(window) {
            return;
        }
        let at = event.position;
        // Three gestures share one press, told apart by button and count, the
        // way the platform tells them apart: the right button offers an
        // insertion, a second left click opens the clip's pattern, and a first
        // left click is the pointer contract in `timeline_press`.
        match (event.button, event.click_count) {
            (MouseButton::Right, _) => {
                pressed.update(cx, |this, cx| this.timeline_insert_menu(at, cx));
            }
            (MouseButton::Left, 2) => {
                pressed.update(cx, |this, cx| this.timeline_open_pattern(at, cx));
            }
            (MouseButton::Left, _) => {
                let keys = event.modifiers;
                pressed.update(cx, |this, cx| this.timeline_press(at, &keys, cx));
            }
            _ => {}
        }
    });

    let dragged = app.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            let at = event.position;
            dragged.update(cx, |this, cx| this.timeline_drag(at, cx));
        }
    });

    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            released.update(cx, |this, cx| this.timeline_release(cx));
        }
    });

    let zoomed = app.clone();
    let over = hitbox.clone();
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        // `should_handle_scroll` and not `is_hovered`: gpui suppresses hover
        // for the whole of a *keyboard* input modality, so that arrowing
        // through a list does not light up whatever the parked cursor happens
        // to be over. A wheel is not hover — it names its own position — and a
        // canvas that asked the hover question would go deaf to the wheel from
        // the first keystroke until the next time the pointer moved.
        if phase != DispatchPhase::Bubble || !over.should_handle_scroll(window) {
            return;
        }
        let wheel = event.delta.pixel_delta(window.line_height());
        // The web timeline zooms on a modified wheel and scrolls on a bare
        // one, because a bare wheel there is the scroll container's. Two
        // modifiers, two rates: the platform key is a wheel and control is a
        // trackpad pinch, which sends a fifth the distance for the same
        // gesture.
        //
        // The sign is already right without a negation: gpui reports a wheel
        // in the direction the *content* moves, which is the opposite of the
        // DOM's `deltaY`, and the web's rate carries a minus for exactly that.
        let gesture = if event.modifiers.alt {
            Wheel::Lanes
        } else if event.modifiers.control {
            Wheel::Zoom(View::ZOOM_PER_PIXEL_PINCH)
        } else if event.modifiers.secondary() {
            Wheel::Zoom(View::ZOOM_PER_PIXEL)
        } else {
            Wheel::Scroll
        };
        let delta = point(f32::from(wheel.x), f32::from(wheel.y));
        let at = event.position;
        zoomed.update(cx, |this, cx| this.timeline_wheel(at, delta, gesture, cx));
    });
}

/// Paint the timeline in the web tile renderer's order: ground, ruler,
/// waveform, lanes and clips, playhead.
///
/// The beat grid goes down *before* the waveform and the clips, so its lines
/// run under both — which is what the web's `renderTile` does, and what makes
/// a clip's translucent body show the beats through it.
fn paint(bounds: Bounds<Pixels>, scene: &Scene, window: &mut Window, cx: &mut App) {
    window.paint_quad(fill(bounds, ladder::background()));
    window.paint_quad(fill(
        Bounds {
            origin: bounds.origin,
            size: size(bounds.size.width, px(HEADER_HEIGHT)),
        },
        ladder::band(),
    ));

    let view = scene.view;
    let start = view.time_at(0.);
    let end = view.time_at(f32::from(bounds.size.width));

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        match &scene.beats {
            Some(beats) => paint_beat_grid(bounds, beats, view, start, end, window, cx),
            None => paint_time_ruler(bounds, view, start, end, window, cx),
        }
        // The hairline under the header, which both rulers stop at.
        window.paint_quad(fill(
            Bounds {
                origin: point(bounds.origin.x, bounds.origin.y + px(HEADER_HEIGHT)),
                size: size(bounds.size.width, px(1.)),
            },
            ladder::border(),
        ));
        paint_waveform(bounds, scene, start, end, window);
        // The lanes are masked to the band below the waveform, because that is
        // what "the lanes scroll and the navigation surface does not" means in
        // pixels: a lifted block runs *under* the waveform rather than over it.
        let layout = scene.layout(bounds);
        window.with_content_mask(
            Some(ContentMask {
                bounds: layout.band(bounds),
            }),
            |window| {
                paint_lanes(bounds, layout, window);
                for clip in scene.clips.iter() {
                    if clip.end < start || clip.start > end {
                        continue;
                    }
                    paint_clip(
                        scene.clip_box(bounds, clip),
                        clip,
                        &scene.previews,
                        scene.selected.contains(&clip.id),
                        window,
                        cx,
                    );
                }
                if let Some(cursor) = scene.cursor {
                    paint_cursor(bounds, layout, cursor, scene.view, start, end, window);
                }
                if let Some(menu) = scene.menu {
                    paint_insertion(bounds, layout, menu, scene.view, window);
                }
            },
        );
        if let Some(region) = scene.loop_region {
            paint_loop(bounds, region, scene.view, window);
        }
        paint_playhead(bounds, scene, start, end, window);
    });
}

/// The box a canvas 2D stroke of `width` centred on `x + 0.5` actually covers.
/// Every coordinate in `timeline-drawing.ts` is written that way; this is the
/// one place the spelling is converted.
fn hairline(canvas: Bounds<Pixels>, x: f32, top: f32, bottom: f32, width: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            canvas.origin.x + px(x + 0.5 - width / 2.),
            canvas.origin.y + px(top),
        ),
        size: size(px(width), px(bottom - top)),
    }
}

/// The same color at a different alpha, for the places the web stacks a
/// `globalAlpha` over a token.
fn fade(color: Rgba, alpha: f32) -> Hsla {
    let mut color: Hsla = color.into();
    color.a = alpha;
    color
}

/// `drawBeatGrid`: every beat as a faint line, every downbeat heavier, and a
/// bar number on every `barLabelStep`-th one.
fn paint_beat_grid(
    canvas: Bounds<Pixels>,
    beats: &BeatGrid,
    view: View,
    start: f64,
    end: f64,
    window: &mut Window,
    cx: &mut App,
) {
    let height = f32::from(canvas.size.height);
    let step = bar_label_step(beats, view.zoom);
    // Millisecond-rounded, which is what de-duplicates a downbeat against the
    // beat it sits on. Exact float equality would draw both, a hair apart.
    let downbeats: std::collections::HashSet<i64> = beats
        .downbeats
        .iter()
        .map(|time| (f64::from(*time) * 1000.).round() as i64)
        .collect();

    let mut last: Option<f32> = None;
    for beat in &beats.beats {
        let beat = f64::from(*beat);
        if beat < start || beat > end {
            continue;
        }
        if downbeats.contains(&((beat * 1000.).round() as i64)) {
            continue;
        }
        let x = view.x_of(beat);
        // Against the last *drawn* beat, so a run of suppressed downbeats does
        // not reset the spacing. Pinned by the `dense*` golden cases.
        if last.is_some_and(|last| x - last < MIN_BEAT_SPACING) {
            continue;
        }
        last = Some(x);
        window.paint_quad(fill(
            hairline(canvas, x, HEADER_HEIGHT, height, 1.),
            fade(ladder::primary(), 0.25),
        ));
        if view.zoom > 100. {
            window.paint_quad(fill(
                hairline(canvas, x, HEADER_HEIGHT - 5., HEADER_HEIGHT, 1.),
                fade(ladder::primary(), 0.25),
            ));
        }
    }

    // A bar number is drawn to the *right* of its downbeat, so a downbeat just
    // left of the viewport still owes it a label.
    let bleed = 40. / f64::from(view.zoom);
    for (index, downbeat) in beats.downbeats.iter().enumerate() {
        let downbeat = f64::from(*downbeat);
        if downbeat < start - bleed || downbeat > end {
            continue;
        }
        let x = view.x_of(downbeat);
        let major = index % step == 0;
        let (alpha, width, tick) = if major {
            (0.6, 2., 12.)
        } else {
            (0.35, 1., 8.)
        };
        window.paint_quad(fill(
            hairline(canvas, x, HEADER_HEIGHT - tick, height, width),
            fade(ladder::primary(), alpha),
        ));
        if major {
            label(
                canvas,
                x + 4.,
                HEADER_HEIGHT - 10.,
                &(index + 1).to_string().into(),
                ladder::foreground(),
                window,
                cx,
            );
        }
    }
}

/// The smallest gap between two drawn beats, in pixels.
const MIN_BEAT_SPACING: f32 = 6.;

/// `getBarLabelStep`: label every Nth bar, so labels stay ~80px apart.
///
/// The bar's length comes from the gap between the first two downbeats alone —
/// not from the average beat — which is what the single-downbeat fallback is
/// for. Both branches are pinned by goldens.
fn bar_label_step(beats: &BeatGrid, zoom: f32) -> usize {
    let average = if beats.beats.len() > 1 {
        (beats.beats[beats.beats.len() - 1] - beats.beats[0]) / (beats.beats.len() - 1) as f32
    } else {
        0.5
    };
    let bar = if beats.downbeats.len() > 1 {
        beats.downbeats[1] - beats.downbeats[0]
    } else {
        average
            * if beats.beats_per_bar == 0 {
                4.
            } else {
                beats.beats_per_bar as f32
            }
    };
    let pixels_per_bar = bar * zoom;
    (80. / pixels_per_bar.max(1.)).ceil().max(1.) as usize
}

/// `drawTimeRuler`: the clock ruler an unanalysed track falls back to.
fn paint_time_ruler(
    canvas: Bounds<Pixels>,
    view: View,
    start: f64,
    end: f64,
    window: &mut Window,
    cx: &mut App,
) {
    let interval = if view.zoom < 50. { 5 } else { 1 };
    let first = (start / f64::from(interval)).floor() as i64 * i64::from(interval);
    let mut tick = first;
    while (tick as f64) <= end {
        let x = view.x_of(tick as f64);
        let major = tick % 10 == 0;
        window.paint_quad(fill(
            hairline(
                canvas,
                x,
                HEADER_HEIGHT - if major { 10. } else { 5. },
                HEADER_HEIGHT,
                1.,
            ),
            if major {
                ladder::border()
            } else {
                ladder::card()
            },
        ));
        if major {
            label(
                canvas,
                x + 3.,
                HEADER_HEIGHT - 12.,
                &format!("{}:{:02}", tick / 60, tick % 60).into(),
                ladder::muted_foreground(),
                window,
                cx,
            );
        }
        tick += i64::from(interval);
    }
}

/// `drawWaveform`: three stacked band envelopes over a recessed bed, or the
/// min/max sample pairs when a track has no bands.
fn paint_waveform(
    canvas: Bounds<Pixels>,
    scene: &Scene,
    start: f64,
    end: f64,
    window: &mut Window,
) {
    let width = f32::from(canvas.size.width);
    let top = HEADER_HEIGHT;
    window.paint_quad(fill(
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(top)),
            size: size(canvas.size.width, px(WAVEFORM_HEIGHT)),
        },
        ladder::card(),
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(
                canvas.origin.x,
                canvas.origin.y + px(top + WAVEFORM_HEIGHT - 0.5),
            ),
            size: size(canvas.size.width, px(1.)),
        },
        // The line under the waveform is a seam, not a plane: it divides the
        // bed from the lanes below it, so it derives from both rather than
        // being a value anyone chose.
        ladder::seam(ladder::card(), ladder::background()),
    ));

    let Some(waveform) = &scene.waveform else {
        return;
    };
    let duration = waveform.duration_seconds;
    let centre = top + WAVEFORM_HEIGHT / 2.;
    let half = (WAVEFORM_HEIGHT - 8.) / 2.;
    let view = scene.view;

    // No horizontal cull: [`columns`] walks the canvas's own pixels, so a
    // column is on screen by construction and a second bound here would only
    // be somewhere for the two to disagree.
    let mut bar = |column: &Column, top: f32, height: f32, color: Rgba| {
        if height <= 0. {
            return;
        }
        window.paint_quad(fill(
            Bounds {
                origin: point(canvas.origin.x + px(column.x), canvas.origin.y + px(top)),
                size: size(px(column.span), px(height)),
            },
            color,
        ));
    };

    let (Some(stored), Some(stored_bands)) = (stored_grid(waveform, duration), &waveform.bands)
    else {
        return;
    };

    // **One picture at every zoom.** The two sources are the same three band
    // envelopes in the same units — the backend normalises both against the
    // track's stored band gains — so which one is in hand changes how much
    // audio a bucket covers and nothing else. Pick it, then forget it: past
    // this line there is a grid and three bands, and no second way to draw
    // them.
    //
    // A measured window is the only source with a bucket per pixel, and it is
    // asked for only where the stored envelope has stopped having one. Until
    // one arrives — and while a pan is outrunning the one in hand — the stored
    // envelope is still the same waveform, so the bed is never blank and the
    // swap is invisible.
    let (grid, bands) = match scene
        .fine
        .as_ref()
        .filter(|fine| fine.covers(start, end, view.zoom))
    {
        Some(fine) => (fine.grid(), &fine.bands),
        None => (stored, stored_bands),
    };

    for column in columns(grid, view, width) {
        // `floor` is monotone, so the tallest bar's floored height is the
        // floored peak — this is the same number a per-bucket walk drew,
        // not an approximation of it.
        let height = |band: &[f32]| {
            (band[column.buckets.clone()]
                .iter()
                .fold(0., |peak: f32, value| peak.max(*value))
                * half)
                .floor()
        };
        // Painted low to high so the quieter bands read as an outline
        // around the louder ones, which is the rekordbox look.
        for (band, color) in [
            (&bands.low, ladder::waveform_low()),
            (&bands.mid, ladder::waveform_mid()),
            (&bands.high, ladder::waveform_high()),
        ] {
            let height = height(band);
            bar(&column, centre - height, height * 2., color);
        }
    }
}

/// One pixel column of the waveform, and the buckets that land in it.
///
/// Never empty: a column with no buckets under it is not drawn at all, rather
/// than drawn as a hole.
struct Column {
    /// The column's left edge — a pixel index, so columns tile without seams.
    x: f32,
    /// The drawn width, `barWidth` in `drawWaveform`. One pixel, always.
    span: f32,
    buckets: std::ops::Range<usize>,
}

/// A run of buckets laid on the timeline: `count` of them, evenly spaced, the
/// first starting `origin` seconds in.
///
/// The stored envelope's grid starts at zero and spans the track; a measured
/// window's starts wherever it was cut and spans only itself. Both are these
/// three numbers, and [`range`](Grid::range) is the only question anything asks
/// of either — which is what "one bucket-walking system" means in code.
#[derive(Clone, Copy)]
struct Grid {
    origin: f64,
    per_second: f64,
    count: usize,
}

impl Grid {
    /// The half-open bucket range a time range covers.
    ///
    /// Empty only when the range falls wholly outside the grid. Inside it the
    /// `floor`/`ceil` pair always spans at least one bucket, so a pixel that
    /// has audio under it always has a bucket to draw — the property the
    /// missing bars came from losing.
    fn range(self, start: f64, end: f64) -> std::ops::Range<usize> {
        let from = ((start - self.origin) * self.per_second).floor().max(0.);
        let to = ((end - self.origin) * self.per_second).ceil().max(0.);
        if !from.is_finite() || !to.is_finite() {
            return 0..0;
        }
        (from as usize).min(self.count)..(to as usize).min(self.count)
    }
}

/// The grid of a track's stored envelope: `FULL_WAVEFORM_SIZE` buckets over the
/// whole track, however long it is. `None` for a waveform with no band
/// envelopes, which is a waveform with nothing to draw — the backend
/// recomputes such a row rather than serving it, so this is the empty case and
/// not a legacy one.
fn stored_grid(waveform: &TrackWaveform, duration: f64) -> Option<Grid> {
    let count = waveform.bands.as_ref()?.low.len();
    if count == 0 || !duration.is_finite() || duration <= 0. {
        return None;
    }
    Some(Grid {
        origin: 0.,
        per_second: count as f64 / duration,
        count,
    })
}

/// The visible pixel columns, each with the buckets that land in it.
///
/// **Walks pixels, not buckets, and that is the whole point.** A column is one
/// pixel of the canvas, so the columns tile the strip exactly: every pixel is
/// drawn once, no pixel twice, and there is no arrangement of grid and zoom
/// that leaves a hole.
///
/// Walking buckets instead — emitting one column per bucket and folding the
/// runs that share a floored `x` — leaves a gap at any pixel no bucket happens
/// to floor onto, and a measured window makes that routine. It is cut at
/// *about* a bucket per pixel and never exactly: the range it answers is
/// clamped to the audio it found, so its density is a hair under the zoom that
/// asked, and a hair under one bucket per pixel is a missing bar every few
/// hundred columns. Those were the gaps in the drawn waveform.
///
/// The quad count is what a frame costs here — gpui charges a `BoundsTree`
/// insert per quad — and a pixel walk is what bounds it: three quads per
/// visible pixel, whatever the zoom and whichever source is drawing.
fn columns(grid: Grid, view: View, width: f32) -> impl Iterator<Item = Column> {
    (0..width.max(0.) as usize).filter_map(move |x| {
        let x = x as f32;
        // Half-open in time exactly as it is in pixels, so two adjacent
        // columns cannot both claim the instant on their shared edge.
        let buckets = grid.range(view.time_at(x), view.time_at(x + 1.));
        (!buckets.is_empty()).then_some(Column {
            x,
            span: 1.,
            buckets,
        })
    })
}

/// `drawAnnotations`' ground: the alternating lane fills and the hairline
/// under each. What is *not* a lane is left as the canvas's own ground.
fn paint_lanes(canvas: Bounds<Pixels>, layout: Layout, window: &mut Window) {
    let width = canvas.size.width;
    let strip = |top: f32, height: f32, color: Hsla, window: &mut Window| {
        window.paint_quad(fill(
            Bounds {
                origin: point(canvas.origin.x, canvas.origin.y + px(top)),
                size: size(width, px(height)),
            },
            color,
        ));
    };

    // The dead-air lane above lane 0 and the floor below the last one are
    // painted by *not* painting: a region with no lane in it is the ground,
    // and the ground is already down. Darkening them would be depth below the
    // floor, which this ladder does not have.
    for lane in 0..layout.rows {
        let top = layout.top(lane);
        let alpha = if lane % 2 == 0 { 0.2 } else { 0.15 };
        strip(top, layout.lane, fade(ladder::card(), alpha), window);
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    canvas.origin.x,
                    canvas.origin.y + px(top + layout.lane - 0.5),
                ),
                size: size(width, px(1.)),
            },
            ladder::border(),
        ));
    }
}

/// One clip: an opaque header plate over a translucent body — the heatmap
/// where one has landed, the pattern colour where none has — inside a border,
/// with grab handles at both ends once it is selected.
fn paint_clip(
    box_: Bounds<Pixels>,
    clip: &Clip,
    previews: &RefCell<HashMap<SharedString, Preview>>,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let body_alpha = if selected { 1. } else { BODY_ALPHA };
    let header = Bounds {
        origin: box_.origin,
        size: size(box_.size.width, px(CLIP_HEADER)),
    };
    window.paint_quad(fill(header, clip.color));
    if !paint_preview(clip_body(box_), clip, previews, selected, window) {
        window.paint_quad(fill(clip_body(box_), fade(clip.color, body_alpha)));
    }
    window.paint_quad(quad(
        box_,
        Corners::default(),
        transparent_black(),
        Edges::all(px(if selected { 1.5 } else { 1. })),
        fade(ladder::foreground(), if selected { 0.9 } else { 0.35 }),
        BorderStyle::Solid,
    ));
    // The line between the header and the body.
    window.paint_quad(fill(
        Bounds {
            origin: point(box_.origin.x, box_.origin.y + px(CLIP_HEADER)),
            size: size(box_.size.width, px(1.)),
        },
        fade(ladder::foreground(), 0.35),
    ));

    if selected {
        for x in [
            box_.origin.x,
            box_.origin.x + box_.size.width - px(HANDLE_MARK),
        ] {
            window.paint_quad(fill(
                Bounds {
                    origin: point(x, box_.origin.y),
                    size: size(px(HANDLE_MARK), px(CLIP_HEADER)),
                },
                fade(ladder::foreground(), 0.9),
            ));
            // Three grip dots down the middle of each plate. A 1px-radius arc
            // on the web, which is a 2px square here — the shape is the affordance,
            // and the difference is invisible at this size.
            let centre = box_.origin.y + px(CLIP_HEADER / 2.);
            for step in -1..=1 {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(
                            x + px(HANDLE_MARK / 2.) - px(1.),
                            centre + px(step as f32 * GRIP_SPACING) - px(1.),
                        ),
                        size: size(px(2.), px(2.)),
                    },
                    fade(ladder::background(), 0.5),
                ));
            }
        }
    }

    // Below this the label is a smudge and shaping is the most expensive thing
    // on the canvas, so the shape stays and the text goes.
    if f32::from(box_.size.width) <= 30. {
        return;
    }
    // The clip clips its own label, which is what the web's `ctx.clip` does:
    // a name too long for the header is cut off at the edge.
    let text = Bounds {
        origin: point(box_.origin.x + px(8.), box_.origin.y),
        size: size(box_.size.width - px(16.), px(CLIP_HEADER)),
    };
    window.with_content_mask(Some(ContentMask { bounds: text }), |window| {
        paint::line(
            point(
                box_.origin.x + px(9.),
                box_.origin.y + px(12. - LABEL_SIZE * paint::ASCENT),
            ),
            &clip.label,
            LABEL_SIZE,
            FontWeight::NORMAL,
            ink(clip.color),
            window,
            cx,
        );
    });
}

/// The translucency of an unselected clip's body — the web's `bodyAlpha`.
/// What lets the beat grid, painted first, read through both the flat fill
/// and the heatmap; selection goes opaque, on both hosts.
const BODY_ALPHA: f32 = 0.75;

/// A clip's body: everything under the header plate.
fn clip_body(box_: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds {
        origin: point(box_.origin.x, box_.origin.y + px(CLIP_HEADER)),
        size: size(box_.size.width, box_.size.height - px(CLIP_HEADER)),
    }
}

/// Texels per heatmap cell in the uploaded image, each way.
///
/// The sprite atlas samples linearly, so a cell drawn wider than its texels
/// melts into its neighbours over one texel's width. Four texels a side keeps
/// that seam under a quarter of a cell at any zoom the view allows, for
/// sixteen times the bytes of the bare grid — a 512-column, 32-row preview
/// (the seam's ceiling) is 1 MiB a frame, and a track's whole score is
/// uploaded once and then only drawn.
const CELL_TEXELS: u32 = 4;

/// Stretch a clip's heatmap over its body.
///
/// Answers whether it painted, so the caller can put the flat fill down when
/// there is nothing to stretch — no decoded preview, or a body too narrow to
/// read (the web's own floor: under 8px the heatmap is noise).
///
/// One `paint_image` of the whole body, whatever the zoom: the picture was
/// baked at [`CELL_TEXELS`] a cell when it arrived, and the GPU does the
/// stretching from there. A clip that runs off the canvas is masked by the
/// lane band around this call, not trimmed here.
fn paint_preview(
    body: Bounds<Pixels>,
    clip: &Clip,
    previews: &RefCell<HashMap<SharedString, Preview>>,
    selected: bool,
    window: &mut Window,
) -> bool {
    if f32::from(body.size.width) < 8. || f32::from(body.size.height) <= 0. {
        return false;
    }
    let mut previews = previews.borrow_mut();
    let Some(preview) = previews.get_mut(&clip.id) else {
        return false;
    };
    if !preview.published {
        // A re-render lands under the clip's kept atlas identity, and the
        // atlas answers paints from its cache, so it has to be told: same-size
        // tiles are refreshed in place, resized ones evicted for the paint
        // below to re-insert.
        window.update_image(&preview.image).ok();
        preview.published = true;
    }
    window
        .paint_image(
            body,
            body,
            Corners::default(),
            Arc::clone(&preview.image),
            usize::from(selected),
            false,
        )
        .ok();
    true
}

/// A seam heatmap as the two-frame [`RenderImage`] the painter draws: every
/// cell a [`CELL_TEXELS`]-square block, frame 0 at [`BODY_ALPHA`], frame 1
/// opaque, RGBA swapped to the BGRA the sprite atlas stores.
fn bake(width: u32, height: u32, pixels: &[u8]) -> RenderImage {
    let (texels_x, texels_y) = (width * CELL_TEXELS, height * CELL_TEXELS);
    let row_bytes = (texels_x * 4) as usize;
    let mut body = vec![0u8; row_bytes * texels_y as usize];
    let mut opaque = vec![0u8; row_bytes * texels_y as usize];
    for row in 0..height as usize {
        // Build the block's first row, then copy it down the rest.
        let target = row * CELL_TEXELS as usize * row_bytes;
        for column in 0..width as usize {
            let source = (row * width as usize + column) * 4;
            let [r, g, b, a] = [
                pixels[source],
                pixels[source + 1],
                pixels[source + 2],
                pixels[source + 3],
            ];
            let faded = (f32::from(a) * BODY_ALPHA) as u8;
            for texel in 0..CELL_TEXELS as usize {
                let at = target + (column * CELL_TEXELS as usize + texel) * 4;
                opaque[at..at + 4].copy_from_slice(&[b, g, r, a]);
                body[at..at + 4].copy_from_slice(&[b, g, r, faded]);
            }
        }
        for repeat in 1..CELL_TEXELS as usize {
            let to = target + repeat * row_bytes;
            body.copy_within(target..target + row_bytes, to);
            opaque.copy_within(target..target + row_bytes, to);
        }
    }
    let frame = |bytes: Vec<u8>| {
        image::Frame::new(
            image::RgbaImage::from_raw(texels_x, texels_y, bytes)
                .expect("sized as texels_x * texels_y * 4 above"),
        )
    };
    RenderImage::new(vec![frame(body), frame(opaque)])
}

/// The drawn width of a selected clip's grab mark. Narrower than [`HANDLE`],
/// which is what the pointer gets — the web draws 6px and grabs 8px.
const HANDLE_MARK: f32 = 6.;

/// The gap between two grip dots on a grab plate.
const GRIP_SPACING: f32 = 4.;

/// `drawSelectionCursor`: where the next edit lands, over the lane band it
/// covers. A point cursor is a 2px line; a range is a filled rectangle inside
/// a 2px outline.
fn paint_cursor(
    canvas: Bounds<Pixels>,
    layout: Layout,
    cursor: Cursor,
    view: View,
    start: f64,
    end: f64,
    window: &mut Window,
) {
    let (min_row, max_row) = cursor.rows();
    let top = layout.top(min_row);
    let height = (max_row - min_row + 1) as f32 * layout.lane;
    let accent = ladder::accent();

    let Some((from, to)) = cursor.span() else {
        if cursor.start < start || cursor.start > end {
            return;
        }
        window.paint_quad(fill(
            hairline(canvas, view.x_of(cursor.start), top, top + height, 2.),
            accent,
        ));
        return;
    };
    if to < start || from > end {
        return;
    }
    let (left, right) = (view.x_of(from), view.x_of(to));
    let box_ = Bounds {
        origin: point(canvas.origin.x + px(left), canvas.origin.y + px(top)),
        size: size(px(right - left), px(height)),
    };
    window.paint_quad(fill(box_, fade(accent, 0.15)));
    window.paint_quad(quad(
        box_,
        Corners::default(),
        transparent_black(),
        Edges::all(px(2.)),
        accent,
        BorderStyle::Solid,
    ));
}

/// The span the transport is looping: a wash over everything below the ruler,
/// with a line down each bound.
///
/// A colour of its own rather than a value off the grey ladder, and the one
/// place on this canvas that has one: the loop is the only mark here that
/// describes *playback* rather than the score, and a wash in the same family
/// as the clips under it would read as another layer of them.
fn paint_loop(canvas: Bounds<Pixels>, region: (f64, f64), view: View, window: &mut Window) {
    let (from, to) = region;
    let (left, right) = (view.x_of(from), view.x_of(to));
    let width = f32::from(canvas.size.width);
    if right < 0. || left > width {
        return;
    }
    let height = f32::from(canvas.size.height);
    let (clipped_left, clipped_right) = (left.max(0.), right.min(width));
    window.paint_quad(fill(
        Bounds {
            origin: point(
                canvas.origin.x + px(clipped_left),
                canvas.origin.y + px(HEADER_HEIGHT),
            ),
            size: size(px(clipped_right - clipped_left), px(height - HEADER_HEIGHT)),
        },
        fade(LOOP_BAND, 0.12),
    ));
    for edge in [left, right] {
        window.paint_quad(fill(
            hairline(canvas, edge, HEADER_HEIGHT, height, 1.),
            fade(LOOP_BAND, 0.7),
        ));
    }
}

/// The loop band's yellow, `rgb(234 179 8)` — `timeline-drawing.ts`'s literal,
/// which is not on the grey ladder because nothing else on this canvas needs
/// a hue.
const LOOP_BAND: Rgba = Rgba {
    r: 234. / 255.,
    g: 179. / 255.,
    b: 8. / 255.,
    a: 1.,
};

/// Where a right-click's clip would land.
///
/// Add mode fills the target lane and outlines the span; insert mode draws a
/// line along the boundary a new layer would open at — the two are told apart
/// by their shape, because the pointer is in the same place for both.
fn paint_insertion(
    canvas: Bounds<Pixels>,
    layout: Layout,
    menu: InsertMenu,
    view: View,
    window: &mut Window,
) {
    let accent = ladder::accent();
    let left = view.x_of(menu.start);
    let width = (view.x_of(menu.end) - left).max(2.);
    if menu.insert {
        window.paint_quad(fill(
            hairline(
                canvas,
                left,
                layout.top(menu.row) - 1.,
                layout.top(menu.row) + 1.,
                width,
            ),
            accent,
        ));
        return;
    }
    let box_ = Bounds {
        origin: point(
            canvas.origin.x + px(left),
            canvas.origin.y + px(layout.top(menu.row)),
        ),
        size: size(px(width), px(layout.lane)),
    };
    window.paint_quad(fill(box_, fade(accent, 0.1)));
    window.paint_quad(quad(
        box_,
        Corners::default(),
        transparent_black(),
        Edges::all(px(1.)),
        fade(accent, 0.4),
        BorderStyle::Solid,
    ));
}

/// Black on a light plate, white on a dark one. `isLightColor`'s sRGB
/// luminance, the same threshold.
fn ink(on: Rgba) -> Rgba {
    let luminance = 0.299 * on.r + 0.587 * on.g + 0.114 * on.b;
    if luminance > 0.5 {
        rgb(0x000000)
    } else {
        rgb(0xffffff)
    }
}

/// `drawPlayhead`: a full-height line with a pointer at the top.
fn paint_playhead(
    canvas: Bounds<Pixels>,
    scene: &Scene,
    start: f64,
    end: f64,
    window: &mut Window,
) {
    let time = f64::from(scene.playhead);
    if time < start || time > end {
        return;
    }
    let x = scene.view.x_of(time);
    window.paint_quad(fill(
        hairline(canvas, x, 0., f32::from(canvas.size.height), 1.),
        ladder::playhead(),
    ));
    let tip = point(canvas.origin.x + px(x + 0.5), canvas.origin.y);
    let mut head = PathBuilder::fill();
    head.move_to(point(tip.x - px(6.), tip.y));
    head.line_to(point(tip.x + px(6.), tip.y));
    head.line_to(point(tip.x, tip.y + px(8.)));
    head.close();
    if let Ok(head) = head.build() {
        window.paint_path(head, ladder::playhead());
    }
}

/// One canvas-2D `fillText`, at the baseline the web draws it on.
fn label(
    canvas: Bounds<Pixels>,
    x: f32,
    baseline: f32,
    text: &SharedString,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    paint::line(
        point(
            canvas.origin.x + px(x),
            canvas.origin.y + px(baseline - LABEL_SIZE * paint::ASCENT),
        ),
        text,
        LABEL_SIZE,
        FontWeight::NORMAL,
        color,
        window,
        cx,
    );
}
