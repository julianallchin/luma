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
//! A clip's body carries its pattern's space-time heatmap, rendered one clip at a
//! time through `preview_score_clip` after each committed edit, coalesced per
//! clip so a burst of writes costs one trailing render. The seam hands back a tiny
//! RGBA grid (a column per sixteenth-beat, a row per primitive); it is baked
//! once into a block-per-cell image the GPU stretches over the body at any
//! zoom — see [`bake`] and [`paint_preview`]. Previews are decoration: a track whose patterns
//! have no graphs simply keeps the flat translucent fill, and so does any
//! clip too narrow to read.
//!
//! An aim clip gives no light, so its body is curves instead: the pan and
//! tilt the solver sends each head, over the clip, as two bands. Heads that
//! move alike share one curve. The seam hands the curves back with the
//! preview; the painter only strokes them, at whatever width the body has —
//! see [`paint_aim`].
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

use crate::history::History;
use crate::shell::Body;
use crate::tabs::Target;
use crate::{LibraryError, Luma};

mod document;
mod fades;
mod lanes;
use lanes::assign_rows;
mod minimap;
pub(crate) mod picker;
mod playback;
mod playback_clock;
mod playback_surface;
mod sheet;
pub(crate) use sheet::Audition;
mod trace;
mod waveform;
mod zoom_motion;

// -- state --------------------------------------------------------------------

/// The screen's whole state: the track it is showing, everything the seam
/// returned for it, where the eye is, and where the transport is.
pub struct Editor {
    track_id: String,
    track_name: String,
    /// The track's audio file, which playback decodes from the top.
    audio_path: std::path::PathBuf,
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
    /// whole list at once.
    clips: Rc<[Clip]>,
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
    /// per [`SEEK_THROTTLE`] — the picture is free, the IPC is not.
    seek_pending: Option<f32>,
    seek_at: Option<std::time::Instant>,
    /// Where the canvas last painted, in window space. A mouse event arrives
    /// in window coordinates and has to be put back into timeline coordinates,
    /// which needs this; only `prepaint` knows it, and a `Cell` is how it gets
    /// written down there without notifying from inside a draw.
    canvas: Rc<Cell<Bounds<Pixels>>>,
    /// The working copy has moved away from the saved document and owes a
    /// write. A flag rather than a queue of edits: the unit of writing is the
    /// whole score, so there is only ever one thing outstanding.
    dirty: bool,
    /// A write is in flight. Serialized rather than concurrent: two whole-score
    /// writes in the air at once would land in an order nobody chose.
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

/// A seam preview, decoded once for painting: a clip's body, a browser
/// thumbnail and a carried preset's ghost all hold one and draw it with
/// [`Preview::paint`]. Cheap to clone.
#[derive(Clone)]
enum Preview {
    /// A heatmap, baked for the GPU — see [`bake`]. Two frames under one
    /// identity — frame 0 at [`BODY_ALPHA`], frame 1 opaque — so selecting
    /// a clip picks a frame instead of a second image.
    Heatmap(Arc<RenderImage>),
    /// An aim clip's pan and tilt curves — see [`paint_aim`].
    Aim(Arc<AimCurves>),
}

impl Preview {
    /// A seam row, baked under `identity` when one is given, else under a
    /// fresh one. `None` for a heatmap that is not `width * height` of RGBA:
    /// a seam drift this painter cannot draw, and skipping it leaves the flat
    /// fill, which is already what a missing preview means.
    fn decode(row: AnnotationPreview, identity: Option<ImageId>) -> Option<Self> {
        if let Some(curves) = row.aim {
            return Some(Self::Aim(Arc::new(curves)));
        }
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
        Some(Self::Heatmap(Arc::new(image)))
    }

    /// Stretch the heatmap over `body`, or stroke the curves across it, as
    /// the resting body (`selected` false) or the opaque one. `corners`
    /// rounds the picture to a frame it sits in.
    ///
    /// Answers whether it painted, so the caller can put the flat fill down
    /// when there is nothing to draw — a body too narrow to read (under 8px
    /// the heatmap is noise), or too short for the curves.
    ///
    /// One `paint_image` of the whole body, whatever the zoom: the picture
    /// was baked at [`CELL_TEXELS`] a cell when it arrived, and the GPU does
    /// the stretching from there. A clip that runs off the canvas is masked
    /// by the lane band around this call, not trimmed here.
    fn paint(
        &self,
        body: Bounds<Pixels>,
        selected: bool,
        corners: Corners<Pixels>,
        window: &mut Window,
    ) -> bool {
        if f32::from(body.size.width) < 8. || f32::from(body.size.height) <= 0. {
            return false;
        }
        match self {
            Self::Aim(curves) => paint_aim(body, curves, selected, corners, window),
            Self::Heatmap(image) => {
                window
                    .paint_image(
                        body,
                        body,
                        corners,
                        Arc::clone(image),
                        usize::from(selected),
                        false,
                    )
                    .ok();
                true
            }
        }
    }
}

/// A clip's [`Preview`] in the timeline's map.
struct Installed {
    preview: Preview,
    /// Whether the atlas has been told about a heatmap's bytes. The
    /// identity is the clip's for as long as it has a preview: a re-render
    /// refreshes one atlas tile instead of abandoning a trail of them —
    /// `STAGE_IMAGE_ID`'s trick in `visualizer.rs`, one per clip. False for
    /// every fresh bake, so the painter refreshes the tile under the kept
    /// identity before drawing it.
    published: bool,
}

/// What a cut or a copy took, in its two shapes.
///
/// The clips are whole rows so a paste can mint real clips from them. They
/// keep the times they were copied at, and a paste moves them by the musical
/// distance from `origin` to the cursor — see [`shift_time`].
struct Clipboard {
    /// `row` relative to the topmost copied clip, and the clip itself.
    items: Vec<(usize, Clip)>,
    /// The region's start, or the cursor's when whole clips were copied.
    origin: f64,
    /// Where the whole clipboard ends, which is what the cursor spans after a
    /// paste and how far a duplicate moves.
    end: f64,
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
/// `insert` distinguishes the two modes — dropping a clip *onto* the
/// lane under the pointer, or opening a *new* lane at the boundary the pointer
/// is within a quarter-lane of, shifting everything at or above it up.
#[derive(Clone, Copy)]
struct InsertMenu {
    start: f64,
    end: f64,
    row: usize,
    insert: bool,
    /// The pattern highlighted by either keyboard or pointer navigation.
    active: usize,
}

/// A row of the insertion picker: a shipped preset, or a blank start with
/// only an output node.
#[derive(Clone, Copy)]
enum InsertChoice {
    Preset(&'static luma_patterns::ClipPreset),
    Blank(luma_patterns::clip_graph::Kind),
}

impl InsertChoice {
    /// The blank starts, after the presets.
    const BLANKS: [luma_patterns::clip_graph::Kind; 3] = [
        luma_patterns::clip_graph::Kind::Color,
        luma_patterns::clip_graph::Kind::Aim,
        luma_patterns::clip_graph::Kind::Strobe,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Preset(preset) => &preset.name,
            Self::Blank(kind) => kind.label(),
        }
    }
    /// What tells rows apart. Preset names are unique across kinds.
    fn id(&self) -> String {
        match self {
            Self::Preset(preset) => preset.name.clone(),
            Self::Blank(kind) => format!("blank-{}", kind.name()),
        }
    }
    /// What the picker shows beside the name: the output kind, or "Blank".
    fn origin(&self) -> &'static str {
        match self {
            Self::Preset(preset) => preset.output_kind().label(),
            Self::Blank(_) => "Blank",
        }
    }
    fn graph(&self) -> luma_patterns::ClipGraph {
        match self {
            Self::Preset(preset) => preset.graph.clone(),
            Self::Blank(kind) => luma_patterns::ClipGraph::new([(
                format!("{}1", kind.name()),
                luma_patterns::clip_graph::Node::new(*kind),
            )]),
        }
    }
    /// A clip of this choice from `start` for `duration` beats. A blank
    /// start is named after its output, as the checker wants a name.
    fn clip(&self, start: f64, duration: f64) -> luma_patterns::Clip {
        match self {
            Self::Preset(preset) => preset.clip(start, duration),
            Self::Blank(kind) => luma_patterns::Clip {
                name: kind.label().into(),
                start,
                duration,
                seed: 0,
                selection_seed: None,
                selection: luma_patterns::Selection::all(),
                z_index: 0,
                blend_mode: BlendMode::Replace,
                graph: self.graph(),
            },
        }
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
/// Long enough to cover a momentum flick, which keeps delivering after the
/// fingers lift. One number serves the modified wheel and the trackpad pinch
/// that arrives as ctrl-wheel.
const ANCHOR_IDLE: Duration = Duration::from_millis(120);

/// How often a scrub is allowed to move the transport. `SEEK_THROTTLE_MS`.
const SEEK_THROTTLE: Duration = Duration::from_millis(32);

/// The snap capture radius, in screen pixels, for a cursor or an insertion —
/// `snapToGrid`'s `15`.
const SNAP_CAPTURE: f32 = 15.;
/// The tighter radius inside a clip drag.
const SNAP_CAPTURE_DRAG: f32 = 12.;

/// The shortest a clip may be left by a resize.
///
/// Not `MIN_ANNOTATION_DURATION` (0.05 s): a resize is held to 0.1 s, and the
/// smaller floor is for splits, pastes and insertions.
const MIN_RESIZE: f64 = 0.1;

/// `MIN_ANNOTATION_DURATION`: the shortest a clip a *command* produces may be.
/// A split half, a paste remnant or a region-cleared tail below this is
/// dropped rather than kept — deliberately smaller than [`MIN_RESIZE`], which
/// is what a hand at the edge of a clip is held to.
const MIN_CLIP: f64 = 0.05;

/// How close to a lane *boundary* a right-click has to be, in lanes, to mean
/// "open a new layer here" rather than "drop it on this lane".
const INSERT_BOUNDARY: f32 = 0.25;

/// One bar, for a track with no beat grid to ask: a default beat of 0.5 s
/// times four beats to the bar.
const DEFAULT_BAR: f64 = 2.;

/// How long a clip a right-click inserts at `after` should be.
///
/// The next downbeat if there is one — so an inserted clip lands on the bar
/// line the eye can see — and the mean bar otherwise.
/// Where `bars` whole bars after `start` end, bar by bar along the grid.
fn bars_after(beats: Option<&BeatGrid>, start: f64, bars: usize) -> f64 {
    (0..bars).fold(start, |end, _| end + bar_length(beats, end))
}

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
/// what turns the loop key into a toggle. A 1 ms tolerance.
const LOOP_EPSILON: f64 = 0.001;

/// `snapToGrid`: quantise `time` to the beat subdivision the zoom asks for, but
/// only when the quantised point is within `capture` screen pixels of it.
///
/// The capture radius is a parameter because one gesture has two radii: 15 px
/// for the selection cursor and 12 px inside a clip drag.
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

/// `time` moved by the musical distance from `from` to `to`.
///
/// The distance is counted in beats, not seconds, because beats on a detected
/// grid differ in length. A clip moved by a fixed number of seconds lands a
/// fraction of a millisecond off the grid, and then overlaps its neighbour.
/// Without a grid there are no beats to count, so seconds are all there is.
fn shift_time(clock: Option<&luma_patterns::BeatTimeline>, time: f64, from: f64, to: f64) -> f64 {
    // The beat round trip is not bit exact, and a clip that did not move
    // must keep its authored beat.
    if from == to {
        return time;
    }
    let musical = clock.and_then(|clock| {
        let beat =
            clock.beat_at(time).ok()? + clock.beat_at(to).ok()? - clock.beat_at(from).ok()?;
        clock.seconds_at(beat).ok()
    });
    musical.unwrap_or(time + (to - from))
}

/// The quantised point, before the capture test, or `None` when there is no
/// grid to quantise against.
///
/// It snaps to the grid lines the eye can see: the finest [`Grid`] rung that
/// has at least [`MIN_SNAP_SPACING`] of room.
fn beat_snap(beats: Option<&BeatGrid>, time: f64, zoom: f32) -> Option<f64> {
    let grid = beats.filter(|grid| !grid.beats.is_empty())?;
    let (beat, bar) = mean_lengths(grid);
    let divisions = match Grid::fit(beat * zoom, bar * zoom, MIN_SNAP_SPACING).0 {
        Grid::Beat(divisions) => f64::from(divisions),
        Grid::Bars(n) if !grid.downbeats.is_empty() => {
            return Some(bar_snap(&grid.downbeats, time, n as usize));
        }
        Grid::Bars(_) => 1.,
    };
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
    let step = ((time - prev) / length * divisions)
        .round()
        .clamp(0., divisions);
    Some((prev + step / divisions * length).clamp(prev, next))
}

/// The nearest of every `n`-th downbeat, counted from bar 1.
fn bar_snap(downbeats: &[f32], time: f64, n: usize) -> f64 {
    let index = downbeats
        .partition_point(|bar| f64::from(*bar) <= time)
        .saturating_sub(1);
    let before = index / n * n;
    let at = |index: usize| f64::from(downbeats[index]);
    match downbeats.get(before + n) {
        Some(after) if f64::from(*after) - time < time - at(before) => f64::from(*after),
        _ => at(before),
    }
}

/// Where the eye is: a horizontal zoom in pixels per second, and a scroll in
/// pixels.
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
    /// the floor where it is without anybody recomputing a scroll.
    lift: f32,
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
    /// waveform below it clears the selection instead. That is the one
    /// surprise in the pointer map.
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
        /// Alt left copies of the held clips where they stood.
        cloned: bool,
    },
    /// Dragging one part of a form clip's alpha line — see [`fades`]. Every
    /// move is computed from the press, like a clip drag.
    Alpha {
        clip: SharedString,
        /// The part taken hold of, and the alpha when the pointer took hold.
        grab: fades::Grab,
        origin: Point<Pixels>,
        /// The line's travel from alpha 0 to 1, in pixels.
        travel: f32,
    },
    /// Panning: the lanes follow the pointer on both axes, as a bare wheel
    /// moves them. The middle button pans, and so does the left one with the
    /// platform key and Alt held (Cmd+Option on a Mac, Ctrl+Alt elsewhere).
    /// `last` is where the previous move left the pointer; `button` is the
    /// one whose release ends the pan.
    Pan {
        last: Point<Pixels>,
        button: MouseButton,
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
    /// Its fades when it has any, so a resize can keep their lengths.
    fades: Option<fades::Fades>,
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
    /// Horizontal zoom limits in logical pixels per second. A long song can
    /// go below `MIN_ZOOM`, to fit it whole: see [`Editor::min_zoom`].
    const MIN_ZOOM: f32 = 25.;
    const MAX_ZOOM: f32 = 5_000.;
    /// The opening zoom.
    const DEFAULT_ZOOM: f32 = 50.;
    /// `ZOOM_SENSITIVITY`: the exponential rate a modified wheel notch scales
    /// by.
    const ZOOM_PER_PIXEL: f32 = 0.002;
    /// The rate a trackpad pinch scales by, which arrives as a ctrl-wheel.
    /// Five times [`Self::ZOOM_PER_PIXEL`], because a pinch's deltas are a
    /// fifth the size of a wheel's.
    const ZOOM_PER_PIXEL_PINCH: f32 = 0.01;
    /// `MIN_ZOOM_Y` / `MAX_ZOOM_Y`, and `ZOOM_Y_SENSITIVITY` for the alt-wheel
    /// that walks between them.
    const MIN_ZOOM_Y: f32 = 0.5;
    const MAX_ZOOM_Y: f32 = 1.5;
    const ZOOM_Y_PER_PIXEL: f32 = 0.003;

    /// The time under a point `offset` pixels from the canvas's left edge.
    fn time_at(self, offset: f32) -> f64 {
        (f64::from(offset) + f64::from(self.scroll)) / f64::from(self.zoom)
    }

    /// The time range a canvas `width` pixels wide shows.
    fn visible(&self, width: f32) -> (f64, f64) {
        (self.time_at(0.), self.time_at(width))
    }

    /// Where a time lands in logical pixels. Preserve subpixel motion until paint.
    fn x_of(self, time: f64) -> f32 {
        (time * f64::from(self.zoom) - f64::from(self.scroll)) as f32
    }
}

impl Editor {
    /// The tab's chip: the track, and which of its scores. Two scores of one
    /// track are two tabs, so the track alone would not tell them apart.
    pub(crate) fn title(&self) -> String {
        format!("{} · #{}", self.track_name, self.score.ordinal)
    }

    /// What an export of the open score takes from the timeline: its name,
    /// its length in seconds and its audio file. `None` before the track's
    /// length is known.
    pub(crate) fn export_source(&self) -> Option<(String, f32, std::path::PathBuf)> {
        let duration = self.transport.duration;
        (duration > 0.).then(|| (self.track_name.clone(), duration, self.audio_path.clone()))
    }

    /// Whether an edit is allowed to land at all: a score this host owns, and
    /// no other reason to refuse.
    fn writable(&self) -> bool {
        !self.score.read_only
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
        // Above the lanes the gesture means nothing.
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

    /// The range on screen, from the canvas the last frame painted.
    fn visible(&self) -> (f64, f64) {
        self.view.visible(f32::from(self.canvas.get().size.width))
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
            .find(|clip| clip.row == row && time >= clip.start && time < clip.end)
    }

    /// The furthest the view may scroll: the content's width less the
    /// canvas's. A browser scroll container applies this bound for free; here
    /// it is the one place scroll is written, so it applies it once.
    /// The lowest horizontal zoom: the whole song across the canvas, or
    /// [`View::MIN_ZOOM`] when that is further in.
    fn min_zoom(&self) -> f32 {
        let width = f32::from(self.canvas.get().size.width);
        let duration = self.transport.duration;
        if duration > 0. && width > 0. {
            View::MIN_ZOOM.min(width / duration)
        } else {
            View::MIN_ZOOM
        }
    }

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
        // A drag with no sideways motion keeps its time: a clip lifted to
        // another lane must not also jump to a grid line.
        let snap = |time: f64| {
            if delta == 0. {
                time
            } else {
                snap(beats, time, zoom, SNAP_CAPTURE_DRAG)
            }
        };
        let clock = beats.and_then(|grid| grid.timeline().ok());
        let clock = clock.as_ref();

        // Where the pressed clip's own edge was and where it went, as a
        // musical distance the rest can take. `None` where the pressed clip's
        // guard refused the move, which refuses it for the whole group rather
        // than letting the others slide without it.
        let shift = match drag {
            Drag::Move => Some((anchor.start, snap(anchor.start + delta).max(0.))),
            Drag::Resize(Edge::Start) => {
                let start = snap(anchor.start + delta);
                (start < anchor.end - MIN_RESIZE).then_some((anchor.start, start))
            }
            Drag::Resize(Edge::End) => {
                let end = snap(anchor.end + delta);
                (end > anchor.start + MIN_RESIZE).then_some((anchor.end, end))
            }
        };
        let Some((from, to)) = shift else { return };
        let shift = |time: f64| shift_time(clock, time, from, to);

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
                    clip.start = shift(was.start).max(0.);
                    // The length stays the same number of beats, not seconds.
                    clip.end = shift_time(clock, was.end, was.start, clip.start);
                    // Applied for real rather than as a paint offset: the
                    // whole lane change is a function of the row the press
                    // captured, so recomputing it every move from that is
                    // idempotent and there is no second, visual-only
                    // representation to keep in step with this one.
                    clip.z = row_to_z(layers, was.row as i32 - 1 + rows);
                }
                Drag::Resize(Edge::Start) => {
                    let moved = shift(was.start).max(0.);
                    if moved < was.end - MIN_RESIZE {
                        clip.start = moved;
                    }
                }
                Drag::Resize(Edge::End) => {
                    let moved = shift(was.end).min(duration);
                    if moved > was.start + MIN_RESIZE {
                        clip.end = moved;
                    }
                }
            }
            if let (Drag::Resize(_), Some(faded)) = (drag, was.fades) {
                fades::refit(clip, faded, was.end - was.start);
            }
        }
        self.replace_clips(clips);
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

    /// Let the clips a drag held cut away what they cover in their layers —
    /// see [`lanes::settle`] — except the `kept` ones.
    fn settle(&mut self, held: &[Initial], kept: &[SharedString]) {
        let placed: Vec<SharedString> = held.iter().map(|clip| clip.id.clone()).collect();
        if let Some(clips) = lanes::settle(&self.clips, &placed, kept) {
            self.replace_clips(clips);
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

    /// Advance wheel zoom while keeping its latched time under the pointer.
    fn tick_zoom(&mut self, now: std::time::Instant) -> bool {
        let Some(motion) = &mut self.zoom_motion else {
            return false;
        };
        let (zoom, settled) = motion.advance(now);
        self.view.zoom = zoom;
        if let Some(anchor) = self.anchor {
            self.set_scroll(anchor.time as f32 * zoom - anchor.offset);
        }
        if settled {
            self.zoom_motion = None;
        }
        true
    }

    /// Centre the view on the playhead, if the eye is following it.
    fn follow_playhead(&mut self) {
        if !self.follow {
            return;
        }
        let width = f32::from(self.canvas.get().size.width);
        self.set_scroll(self.transport.position * self.view.zoom - width / 2.);
    }

    /// Adopt one clip's re-rendered preview — `preview_annotation`'s answer.
    fn install_preview(&mut self, row: AnnotationPreview) {
        let mut previews = self.previews.borrow_mut();
        let id: SharedString = row.annotation_id.clone().into();
        let identity = match previews.remove(&id) {
            Some(Installed {
                preview: Preview::Heatmap(image),
                ..
            }) => Some(image.id),
            _ => None,
        };
        if let Some(preview) = Preview::decode(row, identity) {
            previews.insert(
                id,
                Installed {
                    preview,
                    published: false,
                },
            );
        }
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
                let rows = self.cursor.unwrap().rows();
                let clips = clear_region(&self.clips, span, |clip| {
                    (rows.0..=rows.1).contains(&clip.row)
                });
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
        let (first, last) = cursor.rows();
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        let mut halves = Vec::new();
        for clip in &mut clips {
            if !(first..=last).contains(&clip.row) || at <= clip.start || at >= clip.end {
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

    /// `moveAnnotationsVertical`: shift the selection one lane up or down,
    /// into that lane's layer. What it lands on there is cut away — see
    /// [`lanes::settle`].
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
        let held = |clip: &Clip| self.selected.contains(&clip.id);
        if down
            && self
                .clips
                .iter()
                .any(|clip| held(clip) && clip.row == layers.len())
        {
            return;
        }
        let step = if down { 1 } else { -1 };
        let mut clips: Vec<Clip> = self.clips.iter().cloned().collect();
        for clip in clips.iter_mut().filter(|clip| held(clip)) {
            clip.z = row_to_z(&layers, clip.row as i32 - 1 + step);
        }
        let clips = lanes::settle(&clips, &self.selected, &[]).unwrap_or(clips);
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
        let items: Vec<(usize, Clip)> = match cursor.span() {
            Some((from, to)) => {
                let (top, bottom) = cursor.rows();
                self.clips
                    .iter()
                    .filter(|clip| {
                        (top..=bottom).contains(&clip.row) && clip.start < to && clip.end > from
                    })
                    .filter_map(|clip| {
                        let (start, end) = (clip.start.max(from), clip.end.min(to));
                        (end - start >= MIN_CLIP)
                            .then(|| (clip.row.saturating_sub(top), clip.copy(start, end, clip.z)))
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
                    .map(|clip| (clip.row - top, clip.copy(clip.start, clip.end, clip.z)))
                    .collect()
            }
        };
        if items.is_empty() {
            return;
        }
        let (origin, end) = match cursor.span() {
            Some(span) => span,
            None => (
                cursor.start,
                items
                    .iter()
                    .map(|(_, clip)| clip.end)
                    .fold(cursor.start, f64::max),
            ),
        };
        self.clipboard = Some(Clipboard { items, origin, end });
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
        let at = cursor.span().map_or(cursor.start, |(from, _)| from);
        let clock = self.beats.as_deref().and_then(|grid| grid.timeline().ok());
        let shift = |time: f64| shift_time(clock.as_ref(), time, board.origin, at);
        let end = shift(board.end);
        let duration = f64::from(self.transport.duration).max(0.);
        let layers = z_ladder(&self.clips);
        let top = cursor.rows().0.max(1);

        let minted: Vec<Clip> = board
            .items
            .iter()
            .filter_map(|(row, clip)| {
                let (start, end) = (shift(clip.start), shift(clip.end));
                (end <= duration)
                    .then(|| clip.copy(start, end, row_to_z(&layers, (top + row) as i32 - 1)))
            })
            .collect();
        if minted.is_empty() {
            return;
        }
        let bottom = top + board.items.iter().map(|(row, _)| *row).max().unwrap_or(0);
        let mut clips = clear_region(&self.clips, (at, end), |clip| {
            (top..=bottom).contains(&clip.row)
        });
        self.selected = minted.iter().map(|clip| clip.id.clone()).collect();
        clips.extend(minted);
        self.replace_clips(clips);
        self.cursor = Some(Cursor {
            row: top,
            row_end: None,
            start: at,
            end: Some(end),
        });
    }

    /// `duplicate`: copy, move the cursor to the end of what was copied, and
    /// paste — which lands a copy immediately after the original.
    fn duplicate(&mut self) {
        self.copy();
        let Some(board) = self.clipboard.as_ref() else {
            return;
        };
        let Some(cursor) = self.cursor else { return };
        let clock = self.beats.as_deref().and_then(|grid| grid.timeline().ok());
        let end = cursor
            .span()
            .map_or(cursor.start, |(_, to)| to)
            .max(shift_time(
                clock.as_ref(),
                board.end,
                board.origin,
                cursor.start,
            ));
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
    /// originals and the copies stay put. On release the originals cut the
    /// copies where they still cover them.
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

    /// Walk the insertion menu's active row. Clamped at both ends: a menu that
    /// wrapped would commit the wrong pattern to a hand
    /// that held the key a beat too long.
    fn step_menu(&mut self, down: bool) {
        let last = self.insertion_choices().len().saturating_sub(1);
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

    /// The presets the picker lists for the current query, in menu order,
    /// then the blank starts. A query matches a row's name or its output
    /// kind.
    fn insertion_choices(&self) -> Vec<InsertChoice> {
        let query = self.menu_query.to_lowercase();
        luma_patterns::presets()
            .clips
            .iter()
            .map(InsertChoice::Preset)
            .chain(InsertChoice::BLANKS.map(InsertChoice::Blank))
            .filter(|choice| {
                choice.name().to_lowercase().contains(&query)
                    || choice.origin().to_lowercase().contains(&query)
            })
            .collect()
    }

    /// Where a clip `bars` long would go for a gesture at `at`, a window
    /// position: the snapped time under it, and either the lane under it or
    /// a new lane at the boundary it is within a quarter-lane of. `None` off
    /// the lanes, on a read-only score, or with no room before the end.
    fn insertion_at(&self, at: Point<Pixels>, bars: usize) -> Option<InsertMenu> {
        if !self.writable() {
            return None;
        }
        let canvas = self.canvas.get();
        let time = self.view.time_at(f32::from(at.x - canvas.origin.x));
        let y = f32::from(at.y - canvas.origin.y);
        let layout = self.layout();
        if y < TRACK_AREA_Y {
            return None;
        }
        let offset = ((y - layout.start) / layout.lane).max(0.);
        // Inside a swept selection, the clip fills its time span on the lane
        // under the pointer.
        if let Some((start, end)) = self.cursor.and_then(|cursor| {
            let (start, end) = cursor.span()?;
            let (top, bottom) = cursor.rows();
            let row = offset.floor() as usize;
            (time >= start && time <= end && (top..=bottom).contains(&row))
                .then_some((start.max(0.), end.min(f64::from(self.transport.duration))))
        }) {
            return (end - start >= MIN_CLIP).then_some(InsertMenu {
                start,
                end,
                row: offset.floor() as usize,
                insert: false,
                active: 0,
            });
        }
        let beats = self.beats.as_deref();
        let start = snap(beats, time, self.view.zoom, SNAP_CAPTURE).max(0.);
        let end = bars_after(beats, start, bars).min(f64::from(self.transport.duration));
        if end - start < MIN_CLIP {
            return None;
        }
        let layers = z_ladder(&self.clips).len();
        let boundary = offset.round();
        let insert = (offset - boundary).abs() < INSERT_BOUNDARY
            && (1. ..=layers as f32).contains(&boundary);
        Some(InsertMenu {
            start,
            end,
            row: if insert { boundary } else { offset.floor() } as usize,
            insert,
            active: 0,
        })
    }

    fn menu_choice(&self) -> Option<(InsertMenu, InsertChoice)> {
        let menu = self.menu?;
        Some((menu, *self.insertion_choices().get(menu.active)?))
    }
}

/// Lighting priority for each visible row, from top to bottom.
fn z_ladder(clips: &[Clip]) -> Vec<i64> {
    let mut layers = vec![0; clips.iter().map(|clip| clip.row).max().unwrap_or(0)];
    for clip in clips {
        if clip.row > 0 {
            layers[clip.row - 1] = clip.z;
        }
    }
    layers
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
/// cleared out of the clips `hit` picks.
///
/// One function rather than a plan-then-apply pair, because no caller inspects
/// the plan. What survives is the
/// interesting part: a clip the region *partly* covers is trimmed or split
/// rather than deleted, and a remnant shorter than [`MIN_CLIP`] is dropped
/// instead of being left as a sliver nothing can grab.
fn clear_region(clips: &[Clip], span: (f64, f64), hit: impl Fn(&Clip) -> bool) -> Vec<Clip> {
    let (from, to) = span;
    let mut out = Vec::with_capacity(clips.len());
    for clip in clips {
        let touched = hit(clip) && clip.start < to && clip.end > from;
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

// -- navigation, gestures and writes ------------------------------------------
//
// These hang off `Luma` because opening a track is five `Library` calls plus a
// screen transition, and `Luma` owns both.

impl Luma {
    /// Open the newest score `track_id` has in the selected venue — the
    /// first row of the listing, which is the order the sidebar shows.
    pub(crate) fn open_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let track_id = track_id.to_string();
        let listing = self.library.scores_across_venues(&track_id);
        cx.spawn(async move |this, cx| {
            let Ok(summaries) = listing.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let user = this.library.user_id();
                let newest = crate::tracks::scores::rows(&summaries, user.as_deref())
                    .iter()
                    .find(|row| row.venue_id.as_ref() == venue_id)
                    .map(crate::tracks::scores::open_row);
                if let Some(score) = newest {
                    this.open_score(&track_id, score, None, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open `score` of `track_id` as a tab in the selected venue, or bring its
    /// tab to the front: the tab's identity is its score. `thread` is the chat
    /// a restored tab had open.
    ///
    /// The four reads are started together and awaited in order: they do not
    /// depend on each other, and each is already its own task on the Tokio
    /// runtime, so sequencing the `await`s costs nothing and keeps the
    /// assignment in one place.
    pub(crate) fn open_score(
        &mut self,
        track_id: &str,
        score: Score,
        thread: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let Some(track) = browser.find(track_id) else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let target = Target::Score {
            venue: venue_id.clone(),
            track: track.id.clone(),
            score: score.id.clone(),
        };
        if self.workspace.body(&target).is_some() {
            self.workspace.select(&target);
            self.workspace_hidden = false;
            cx.notify();
            return;
        }

        let waveform = self.library.track_waveform(track_id);
        let beats = self.library.track_beats(track_id);
        let validation = self.library.track_beat_validation(track_id);
        let contents = self.library.score_contents(&score.id, track_id);

        let menu_search =
            cx.new(|cx| luma_ui::text_input::TextInput::search("Search patterns…", cx));
        let search_target = target.clone();
        let menu_subscription = cx.subscribe(&menu_search, move |this, field, event, cx| {
            if event == &luma_ui::text_input::Event::Edited {
                let query = field.read(cx).text().to_string();
                this.edit_track_tab(&search_target, cx, |editor| {
                    if editor.menu_query == query {
                        return;
                    }
                    editor.menu_query = query;
                    if let Some(menu) = editor.menu.as_mut() {
                        menu.active = 0;
                    }
                    editor.menu_scroll.scroll_to_item(0);
                });
            }
        });
        let chat = self.tab_chat(&target, thread, cx);
        let state = Box::new(Editor {
            track_id: track.id.clone(),
            track_name: track_title(&track),
            audio_path: track.file_path.clone().into(),
            venue_id: venue_id.clone(),
            score,
            chat,
            waveform: None,
            gpu_waveform: Rc::new(RefCell::new(waveform::Resource::default())),
            timeline_waveform: waveform::Strip::default(),
            overview_waveform: waveform::Strip::default(),
            minimap_drag: None,
            beats: None,
            beat_verdict: BeatValidationVerdict::Unreviewed,
            beat_reason: None,
            beat_reason_menu: Default::default(),
            beat_validation_pending: false,
            beat_validation_error: None,
            clips: Vec::new().into(),
            graph_score: None,
            previews: Rc::new(RefCell::new(HashMap::new())),
            preview_inflight: HashSet::new(),
            preview_errors: HashMap::new(),
            preview_queued: HashSet::new(),
            history: History::default(),
            clipboard: None,
            loop_region: None,
            menu: None,
            menu_query: String::new(),
            menu_search,
            _menu_subscription: menu_subscription,
            menu_scroll: ScrollHandle::new(),
            selected: Vec::new(),
            pressed_clip: None,
            cursor: None,
            follow: false,
            view: View {
                zoom: View::DEFAULT_ZOOM,
                scroll: 0.,
                zoom_y: 1.,
                lift: 0.,
            },
            transport: Transport::default(),
            playback_surface: None,
            gesture: None,
            alpha_hover: None,
            canvas: Rc::new(Cell::new(Bounds::default())),
            anchor: None,
            zoom_motion: None,
            seek_pending: None,
            seek_at: None,
            dirty: false,
            saving: false,
            writes: 0,
            error: None,
            loaded: false,
            composited: None,
            compositing: false,
            sheet: sheet::State::default(),
        });
        self.open_tab(target.clone(), move || Body::TrackEditor(state), cx);

        cx.spawn(async move |this, cx| {
            let waveform = waveform.await;
            let beats = beats.await;
            let validation = validation.await;
            let contents = contents.await;

            this.update(cx, |this, cx| {
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
                    match validation {
                        Ok(grid) => {
                            if let Some(validation) = grid.filter(|validation| {
                                Some(&validation.grid) == editor.beats.as_deref()
                            }) {
                                editor.beat_verdict = validation.verdict;
                                editor.beat_reason = validation.reason;
                            }
                        }
                        Err(error) => editor.beat_validation_error = Some(error.to_string()),
                    }
                });
                this.install_score(target, contents, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Put a score's stored contents on its tab's timeline, with their
    /// previews and the stage's scene.
    fn install_score(
        &mut self,
        target: Target,
        contents: Result<crate::library::ScoreContents, LibraryError>,
        cx: &mut Context<Self>,
    ) {
        let mut previews = Vec::new();
        let mut split = false;
        self.edit_track_tab(&target, cx, |editor| match contents {
            Ok(contents) => match editor.install_contents(contents) {
                Ok(()) => {
                    previews = editor.clips.iter().map(|clip| clip.id.clone()).collect();
                    split = editor.dirty;
                }
                Err(error) => editor.error = Some(error),
            },
            Err(error) => editor.error = Some(error.to_string()),
        });
        for id in previews {
            self.refresh_clip_preview_for(target.clone(), id, cx);
        }
        self.refresh_working_scene_for(&target, cx);
        if split {
            self.commit_graph_score_for(target, cx);
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
        if let Some(Body::TrackEditor(editor)) = self.parked.body_mut(&mut self.workspace, target) {
            edit(editor);
            cx.notify();
        }
    }

    fn edit_waveform_tab(
        &mut self,
        target: &Target,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Editor),
    ) {
        if let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) {
            edit(editor);
            // Playback already schedules the next display frame. Publishing a
            // texture must not insert an extra, off-cadence animation step.
            if editor.transport.playing && editor.timeline_waveform.frame.is_some() {
                return;
            }
            if let Some(surface) = editor.playback_surface.clone() {
                surface.update(cx, |_, cx| cx.notify());
            } else {
                cx.notify();
            }
        }
    }

    fn save_beat_validation(
        &mut self,
        verdict: BeatValidationVerdict,
        reason: Option<BeatValidationReason>,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        if editor.beat_validation_pending {
            return;
        }
        let Some(grid) = editor.beats.as_deref() else {
            return;
        };
        let target = editor.target();
        let request =
            self.library
                .set_track_beat_validation(&editor.track_id, grid, verdict, reason);
        self.edit_track_tab(&target, cx, |editor| {
            editor.beat_reason_menu.close();
            editor.beat_validation_pending = true;
            editor.beat_validation_error = None;
        });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                this.edit_track_tab(&target, cx, |editor| {
                    editor.beat_validation_pending = false;
                    match result {
                        Ok(()) => {
                            editor.beat_verdict = verdict;
                            editor.beat_reason = reason;
                        }
                        Err(error) => editor.beat_validation_error = Some(error.to_string()),
                    }
                });
            })
            .ok();
        })
        .detach();
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
        let pending = self.library.set_loop_region(
            state.transport.session.unwrap_or(0),
            region.map(|(from, to)| (from as f32, to as f32)),
        );
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

    fn add_pattern(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            if !editor.writable() {
                return;
            }
            let time = editor
                .cursor
                .map_or(f64::from(editor.transport.position), |cursor| cursor.start);
            let beats = editor.beats.as_deref();
            let start = snap(beats, time, editor.view.zoom, SNAP_CAPTURE).max(0.);
            let end = editor
                .cursor
                .and_then(Cursor::span)
                .map_or_else(|| start + bar_length(beats, start), |(_, end)| end)
                .min(f64::from(editor.transport.duration));
            if end - start < MIN_CLIP {
                return;
            }
            editor.menu = Some(InsertMenu {
                start,
                end,
                row: editor.cursor.map_or(1, |cursor| cursor.row),
                insert: false,
                active: 0,
            });
            editor.menu_scroll.scroll_to_item(0);
        });
        picker::open(self, cx);
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
            editor.menu = editor.insertion_at(at, 1);
            editor.menu_scroll.scroll_to_item(0);
        });
        picker::open(self, cx);
    }

    /// A double-click on a clip selects it, which brings up its inputs.
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
        let selected = state
            .pressed_clip
            .as_ref()
            .and_then(|(id, _)| state.clips.iter().find(|clip| clip.id == *id));
        let Some(clip) = selected.or_else(|| {
            let row = state.layout().row_at(f32::from(at.y - canvas.origin.y))?;
            state
                .clips
                .iter()
                .find(|clip| clip.row == row && time >= clip.start && time < clip.end)
        }) else {
            return;
        };
        let id = clip.id.clone();
        self.with_track_editor(cx, |editor| editor.selected = vec![id]);
    }

    /// `ArrowUp` / `ArrowDown` in the insertion menu. A no-op with no menu
    /// open, which is what makes the bare arrows safe to bind at all: they stay
    /// unbound everywhere else in the editor.
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
        self.insert_pattern(menu, pattern, luma_patterns::Selection::all(), cx);
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
            Some(Body::TrackEditor(state)) => {
                if state.beat_reason_menu.is_open() {
                    state.beat_reason_menu.close();
                    true
                } else {
                    state.sheet.dismiss_menu()
                }
            }
            _ => false,
        }
    }

    /// Clear the clip selection, if there is one — which is what brings the
    /// preset browser back into the inspector. Reported so `Escape` knows the
    /// key was spent.
    pub(crate) fn clear_clip_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let mut cleared = false;
        self.with_track_editor(cx, |editor| {
            cleared = !editor.selected.is_empty();
            if cleared {
                editor.deselect();
            }
        });
        if cleared {
            cx.notify();
        }
        cleared
    }

    /// Place the chosen preset as a new clip on `selection`.
    fn insert_pattern(
        &mut self,
        menu: InsertMenu,
        choice: InsertChoice,
        selection: luma_patterns::Selection,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.overlay.as_open(),
            Some(crate::shell::Overlay::InsertPattern(_))
        ) {
            self.close_overlay(cx);
        }
        self.track_command(
            |editor| {
                if let Err(error) = editor.insert_preset(menu, choice, selection) {
                    editor.error = Some(error);
                }
            },
            cx,
        );
    }

    /// A press on the canvas: take the playhead, a clip, or a sweep of empty
    /// lane.
    ///
    /// The vertical dispatch goes band by band. The ruler scrubs.
    /// Everything between it and the first lane — the waveform, and the empty
    /// insertion lane under it — clears the selection, which is the behavior
    /// that reads as surprising and is the one a person relies on to get back
    /// to nothing selected. Below the last lane does the same.
    fn timeline_press(&mut self, at: Point<Pixels>, keys: &Modifiers, cx: &mut Context<Self>) {
        let mut seek = None;
        let (shift, alt) = (keys.shift, keys.alt);
        self.with_track_editor(cx, |editor| {
            editor.zoom_motion = None;
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let time = editor.view.time_at(offset);
            let y = f32::from(at.y - canvas.origin.y);
            // A press anywhere dismisses an open insertion menu.
            editor.menu = None;

            if y < HEADER_HEIGHT {
                editor.gesture = Some(Gesture::Scrub);
                editor.transport.clock.reset();
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

            if editor.press_alpha(at) {
                return;
            }

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
            editor.pressed_clip = Some((id.clone(), at));
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
            // One clip is its own region. Several are not: a range around the
            // last one pressed would make copy, cut and delete act on it
            // alone, so the cursor becomes a point at the selection's
            // top-left corner and those commands take the whole selection.
            let held: Vec<&Clip> = editor
                .clips
                .iter()
                .filter(|clip| editor.selected.contains(&clip.id))
                .collect();
            editor.cursor = Some(if held.len() > 1 {
                Cursor {
                    row: held.iter().map(|clip| clip.row).min().unwrap_or(row),
                    row_end: None,
                    start: held
                        .iter()
                        .map(|clip| clip.start)
                        .fold(f64::INFINITY, f64::min),
                    end: None,
                }
            } else {
                Cursor {
                    row,
                    row_end: None,
                    start,
                    end: Some(end),
                }
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
            let cloned = alt && drag == Drag::Move;
            if cloned {
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
                    fades: fades::faded(clip),
                })
                .collect();
            editor.gesture = Some(Gesture::Clips {
                pressed: id,
                drag,
                origin: at,
                initial,
                layers: z_ladder(&editor.clips).into(),
                moved: false,
                cloned,
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
        match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(state)) if state.gesture.is_some() => {}
            // Idle, only the cursor over an alpha line can change, and only
            // a change redraws.
            Some(Body::TrackEditor(state)) => {
                if state.hover_alpha(at) {
                    cx.notify();
                }
                return;
            }
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
            match &mut gesture {
                Gesture::Clips { moved, .. } => *moved = true,
                Gesture::Pan { last, .. } => {
                    let delta = at - *last;
                    *last = at;
                    editor.anchor = None;
                    editor.zoom_motion = None;
                    editor.set_scroll(editor.view.scroll - f32::from(delta.x));
                    editor.set_lift(editor.view.lift + f32::from(delta.y));
                }
                _ => {}
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
                    // A sweep brought back to zero width holds no clips, so
                    // it drops what its wider moments had selected.
                    match cursor.span() {
                        Some(span) => editor.select_within(cursor.rows(), span),
                        None => editor.selected.clear(),
                    }
                }
                &Gesture::Clips { origin, .. } => {
                    let delta = f64::from(f32::from(at.x - origin.x) / editor.view.zoom);
                    let rows = (f32::from(at.y - origin.y) / editor.layout().lane).round() as i32;
                    editor.drag_clips(&gesture, delta, rows);
                    editor.sync_cursor();
                }
                Gesture::Alpha { .. } => editor.drag_alpha(&gesture, at),
                Gesture::Pan { .. } => {}
            }
            editor.gesture = Some(gesture);
        });
        if let Some(seconds) = seek {
            self.scrub_seek(seconds, cx);
        }
    }

    /// A press that pans, unless another gesture already holds the canvas.
    fn timeline_pan(&mut self, at: Point<Pixels>, button: MouseButton, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            if editor.gesture.is_none() {
                editor.gesture = Some(Gesture::Pan { last: at, button });
            }
        });
    }

    /// A release. An edge that actually moved is written back; a press that
    /// only selected is not, because nothing changed.
    fn timeline_release(&mut self, button: MouseButton, cx: &mut Context<Self>) {
        // Each button ends only its own gesture: a pan ends on the button
        // that started it, and everything else on the left one.
        match self.workspace.active_body() {
            Some(Body::TrackEditor(state))
                if state.gesture.as_ref().is_some_and(|gesture| match gesture {
                    Gesture::Pan { button: held, .. } => *held == button,
                    _ => button == MouseButton::Left,
                }) => {}
            _ => return,
        }
        let mut save = false;
        let mut flush = None;
        let mut touched = Vec::new();
        self.with_track_editor(cx, |editor| {
            match editor.gesture.take() {
                Some(Gesture::Clips {
                    drag,
                    initial,
                    moved,
                    cloned,
                    ..
                }) => {
                    // A crossfade lives on the overlap of two clips in one
                    // layer, so the clips it crossed are not cut.
                    if drag == Drag::Move && moved {
                        touched = editor.crossfade(&initial);
                    }
                    // A press that only selected places nothing, unless Alt
                    // left copies under the held clips.
                    if moved || cloned {
                        editor.settle(&initial, &touched);
                    }
                    editor.abandon_checkpoint();
                    save = editor.dirty;
                }
                Some(Gesture::Alpha { clip, .. }) => {
                    editor.abandon_checkpoint();
                    touched.push(clip);
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
        for id in touched {
            self.refresh_clip_preview(id, cx);
        }
        if save {
            self.commit_clips(cx);
        }
    }

    /// A wheel notch over the canvas.
    ///
    /// A bare wheel scrolls both axes, because this canvas is its own scroll
    /// container. A modified wheel zooms, and
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
        let reduced_motion = cx.reduce_motion();
        self.with_track_editor(cx, |editor| {
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let rate = match wheel {
                Wheel::Scroll => {
                    editor.anchor = None;
                    editor.zoom_motion = None;
                    let scroll = editor.view.scroll - delta.x;
                    editor.set_scroll(scroll);
                    editor.set_lift(editor.view.lift + delta.y);
                    return;
                }
                Wheel::Lanes => {
                    editor.anchor = None;
                    editor.zoom_motion = None;
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
            editor.tick_zoom(now);
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
            if reduced_motion {
                editor.zoom_motion = None;
                editor.view.zoom = (editor.view.zoom * (delta.y * rate).exp())
                    .clamp(editor.min_zoom(), View::MAX_ZOOM);
                editor.set_scroll(anchor.time as f32 * editor.view.zoom - anchor.offset);
            } else {
                let floor = editor.min_zoom();
                editor
                    .zoom_motion
                    .get_or_insert_with(|| zoom_motion::Zoom::new(editor.view.zoom, floor, now))
                    .push(delta.y * rate);
            }
            editor.anchor = Some(Anchor { at: now, ..anchor });
        });
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

    /// Move the transport, optimistically: the playhead is already where the
    /// pointer put it, and the seek is what makes the audio agree.
    fn seek(&mut self, seconds: f32, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        let Some(session) = editor.transport.session else {
            return;
        };
        if !editor.transport.ready {
            return;
        }
        let pending = self.library.seek(session, seconds);
        cx.background_spawn(async move {
            pending.await.ok();
        })
        .detach();
    }

    /// Publish the working copy: one whole-score save, whatever the gesture
    /// or the command changed.
    ///
    /// **The only write this screen makes.** A gesture that moves five clips,
    /// splits three and deletes one is one write, so it cannot half land and
    /// the score never passes through a state the editor's own rules forbid.
    ///
    /// One write is in flight at a time, and anything the user did meanwhile
    /// is still on [`Editor::dirty`] and goes out on its return.
    fn commit_clips(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        self.commit_graph_score_for(target, cx);
    }

    /// Run `edit` against the track editor, if that is still what is showing.
    /// A load or a write that lands after the user navigated away is a no-op.
    /// Re-render one clip's heatmap from the working copy's row.
    ///
    /// Coalesced per clip: an edit
    /// landing while this clip's render is in flight queues one trailing
    /// re-issue instead of a backlog, and because the render reads the clip's
    /// *current* state when it is issued, that one trailing render covers
    /// everything the burst did.
    fn refresh_clip_preview(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        self.refresh_clip_preview_for(target, id, cx);
    }

    fn refresh_clip_preview_for(
        &mut self,
        target: Target,
        id: SharedString,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::TrackEditor(state)) = self.workspace.body_mut(&target) else {
            return;
        };
        if state.preview_inflight.contains(&id) {
            state.preview_queued.insert(id);
            return;
        }
        // A clip deleted since the edit that asked has no body to preview.
        if !state.clips.iter().any(|clip| clip.id == id) {
            return;
        }
        let score_id = state.score.id.clone();
        let candidate = match state.graph_candidate() {
            Ok(score) => score,
            Err(error) => {
                state.preview_errors.insert(id, error);
                return;
            }
        };
        let pending = self.library.preview_score_clip(&score_id, &id, &candidate);
        state.preview_inflight.insert(id.clone());
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut again = false;
                this.edit_track_tab(&target, cx, |editor| {
                    if !editor.clips.iter().any(|clip| clip.id == id) {
                        editor.preview_inflight.remove(&id);
                        editor.preview_queued.remove(&id);
                        return;
                    }
                    editor.preview_inflight.remove(&id);
                    again = editor.preview_queued.remove(&id);
                    match result {
                        Ok(row) => {
                            editor.preview_errors.remove(&id);
                            editor.install_preview(row);
                        }
                        Err(error) => {
                            editor.previews.borrow_mut().remove(&id);
                            editor
                                .preview_errors
                                .insert(id.clone(), format!("Preview: {error}"));
                        }
                    }
                });
                if again {
                    this.refresh_clip_preview_for(target, id, cx);
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

/// How often the playhead is re-read while playing. 30 Hz — twice the rate the
/// desktop app's broadcaster emits at, because a poll's phase is arbitrary and
/// halving the period halves the worst-case lag.
const POLL: Duration = Duration::from_millis(33);

/// A track's title, or its file name when it has no title.
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
// The layout at `zoomY == 1`, which is the only zoom the
// ruler and the waveform have. Vertical zoom scales the lanes and nothing
// else, so these are constants and [`Layout`] is what varies.

/// The ruler strip: `HEADER_HEIGHT`.
const HEADER_HEIGHT: f32 = 32.;
/// `WAVEFORM_HEIGHT`. Fixed even under vertical zoom — it is a
/// navigation surface, not part of the annotation workspace.
const WAVEFORM_HEIGHT: f32 = 80.;
/// `TRACK_HEIGHT`: one lane.
const LANE_HEIGHT: f32 = 80.;
/// `layout.trackAreaY`: where the scrubbing surface ends and the lanes begin.
const TRACK_AREA_Y: f32 = HEADER_HEIGHT + WAVEFORM_HEIGHT;
/// `ANNOTATION_HEADER_H`: the opaque strip at the top of a clip.
const CLIP_HEADER: f32 = 18.;
/// The grab width of a clip's edge, in screen pixels.
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
/// Lanes expand to fill spare canvas height, so all visible editing space
/// responds to selection and insertion, including scores with only one layer.
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
        let fitted = (height - TRACK_AREA_Y).max(0.) / rows.max(1) as f32;
        // Sparse scores use the whole editing area at the default zoom.
        // Explicit vertical zoom remains meaningful for larger arrangements.
        let lane = if rows <= 2 && view.zoom_y == 1. {
            fitted.max(1.)
        } else {
            (LANE_HEIGHT * view.zoom_y).round()
        };
        let natural = TRACK_AREA_Y + rows as f32 * lane;
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
        if y < TRACK_AREA_Y {
            return None;
        }
        let row = ((y - self.start).max(0.) / self.lane) as usize;
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
    let score = editor.score.id.clone();
    let candidate = match editor.graph_candidate() {
        Ok(score) => score,
        Err(error) => {
            editor.error = Some(error);
            return;
        }
    };
    let sent = editor.clips.clone();
    let target = editor.target();
    editor.compositing = true;
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| {
            this.library.composite_score_document(&score, &candidate)
        }) else {
            return;
        };
        let result = pending.await;
        this.update(cx, |this, cx| {
            this.edit_track_tab(&target, cx, |editor| {
                editor.compositing = false;
                // Remember this attempt, including a failure, so a bad input
                // produces one useful error instead of a retry every frame.
                editor.composited = Some(sent);
                if let Err(error) = result {
                    editor.error = Some(format!("Playback: {error}"));
                }
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
                && a.start == b.start
                && a.end == b.end
                && a.z == b.z
                && a.blend == b.blend
                && a.core == b.core
        })
}

/// The inspector occupies the editing area, even with the rig hidden: the
/// selected clip's controls, or the preset browser. `fill` is split view's
/// placement and `wide` the widened column — see [`sheet::panel`].
pub(crate) fn inspector(
    state: &mut Editor,
    app: &Entity<Luma>,
    fill: bool,
    wide: bool,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> AnyElement {
    sheet::sync(state, window, cx);
    sheet::panel(state, app, fill, wide)
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
    let surface = state
        .playback_surface
        .get_or_insert_with(|| cx.new(|_| playback_surface::Surface::new(app.downgrade())))
        .clone();
    if state
        .beat_reason_menu
        .tick_close(luma_ui::motion::reduced_motion(cx))
    {
        window.request_animation_frame();
    }

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
        // show. A *write* that came back refused — an overlap the seam
        // forbids, a document it would not validate — leaves a perfectly
        // good timeline on screen, and
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
                .child(surface)
                .into_any_element(),
        })
}

/// Keep the working score and its transport live while the timeline is hidden.
pub(crate) fn fullscreen_transport(
    state: &mut Editor,
    app: &Entity<Luma>,
    cx: &mut Context<Luma>,
) -> AnyElement {
    sync_composite(state, cx);
    let transport = app.clone();
    let position = state
        .transport
        .clock
        .position(std::time::Instant::now())
        .unwrap_or(f64::from(state.transport.position))
        .min(f64::from(state.transport.duration)) as f32;
    let label = if state.transport.playing {
        "Pause"
    } else {
        "Play"
    };
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            luma_ui::button(
                label,
                (state.waveform.is_some()
                    && state.transport.ready
                    && state.transport.session.is_some())
                .into(),
            )
            .id("fullscreen-transport")
            .on_click(move |_, _, cx| transport.update(cx, |this, cx| this.toggle_playback(cx)))
            .agent_node(Role::Button, label),
        )
        .child(
            div()
                .text_size(px(12.))
                .pr(px(8.))
                .child(format!(
                    "{} / {}",
                    clock(position),
                    clock(state.transport.duration)
                ))
                .agent_node(
                    Role::Text,
                    format!("{} / {}", clock(position), clock(state.transport.duration)),
                ),
        )
        .into_any_element()
}

/// The way back, what is open, the transport, and whether a write is in the air.
fn toolbar(state: &Editor, app: &Entity<Luma>) -> Div {
    let transport = app.clone();
    let insert = app.clone();
    let export = app.clone();
    let playing = state.transport.playing;
    div()
        .flex()
        .flex_wrap()
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
            luma_ui::button(
                if playing { "Pause" } else { "Play" },
                (state.transport.ready && state.transport.session.is_some()).into(),
            )
            .id("transport")
            // One button, two labels — the label *is* the state, so a
            // script reads what the transport is doing from the same place
            // a person does.
            .on_click(move |_, _, cx| transport.update(cx, |this, cx| this.toggle_playback(cx)))
            .agent_node(Role::Button, if playing { "Pause" } else { "Play" }),
        )
        .child(
            luma_ui::button(
                "Add pattern",
                if state.writable() {
                    Enabled::Yes
                } else {
                    Enabled::No
                },
            )
            .id("add-score-pattern")
            .on_click(move |_, _, cx| insert.update(cx, |app, cx| app.add_pattern(cx)))
            .agent_node(Role::Button, "Add pattern"),
        )
        .child(
            luma_ui::button("Export show", state.export_source().is_some().into())
                .id("export-show")
                .on_click(move |_, _, cx| export.update(cx, |app, cx| app.open_show_export(cx)))
                .agent_node(Role::Button, "Export show"),
        )
        .child(luma_ui::caption(format!(
            "{} / {}",
            clock(state.transport.position),
            clock(state.transport.duration)
        )))
        .child(luma_ui::caption(format!("{} clips", state.clips.len())))
        .when_some(state.beats.as_deref(), |el, grid| {
            el.child(luma_ui::caption(format!("{:.1} BPM", grid.bpm)))
        })
        .when(
            state
                .beats
                .as_ref()
                .is_some_and(|grid| !grid.beats.is_empty() && !grid.downbeats.is_empty()),
            |el| {
                let enabled = if state.beat_validation_pending {
                    Enabled::No
                } else {
                    Enabled::Yes
                };
                let votes = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().text_size(px(12.)).child("Beat grid"))
                    .children(
                        [
                            (
                                BeatValidationVerdict::Correct,
                                luma_ui::icons::IconName::ThumbsUp,
                                if state.beat_verdict == BeatValidationVerdict::Correct {
                                    "Beat grid approved"
                                } else {
                                    "Beat grid correct"
                                },
                            ),
                            (
                                BeatValidationVerdict::Incorrect,
                                luma_ui::icons::IconName::ThumbsDown,
                                if state.beat_verdict == BeatValidationVerdict::Incorrect {
                                    "Beat grid flagged"
                                } else {
                                    "Needs correction"
                                },
                            ),
                        ]
                        .into_iter()
                        .map(|(verdict, icon, label)| {
                            let selected = state.beat_verdict == verdict;
                            let next = if selected {
                                BeatValidationVerdict::Unreviewed
                            } else {
                                verdict
                            };
                            let app = app.clone();
                            luma_ui::button("", enabled)
                                .w(px(28.))
                                .px(px(0.))
                                .id(label)
                                .child(gpui_component::Icon::new(icon).size(px(14.)))
                                .when(selected, |el| el.text_color(ladder::primary()))
                                .on_click(move |_, _, cx| {
                                    app.update(cx, |this, cx| {
                                        this.save_beat_validation(next, None, cx)
                                    })
                                })
                                .agent_node(Role::Button, label)
                        }),
                    );
                el.child(votes.when(
                    state.beat_verdict == BeatValidationVerdict::Incorrect,
                    |el| {
                        let value = match state.beat_reason {
                            None => "Reason (optional)",
                            Some(BeatValidationReason::Tempo) => "Wrong tempo",
                            Some(BeatValidationReason::Offset) => "Offset",
                            Some(BeatValidationReason::Drift) => "Drift",
                            Some(BeatValidationReason::BarPhase) => "Bar phase",
                        };
                        let toggle = app.clone();
                        let pick = app.clone();
                        el.child(luma_ui::arg::select::luma_arg_select(
                            "beat-grid-reason",
                            value,
                            &[
                                "Reason (optional)",
                                "Wrong tempo",
                                "Offset",
                                "Drift",
                                "Bar phase",
                            ],
                            state.beat_reason_menu,
                            move |_, cx| {
                                toggle.update(cx, |this, cx| {
                                    this.with_track_editor(cx, |editor| {
                                        if !editor.beat_validation_pending {
                                            editor.beat_reason_menu.toggle();
                                        }
                                    });
                                })
                            },
                            move |index, _, cx| {
                                pick.update(cx, |this, cx| {
                                    let reason = [
                                        None,
                                        Some(BeatValidationReason::Tempo),
                                        Some(BeatValidationReason::Offset),
                                        Some(BeatValidationReason::Drift),
                                        Some(BeatValidationReason::BarPhase),
                                    ][index];
                                    this.save_beat_validation(
                                        BeatValidationVerdict::Incorrect,
                                        reason,
                                        cx,
                                    );
                                })
                            },
                        ))
                    },
                ))
            },
        )
        .when_some(state.beat_validation_error.clone(), |el, message| {
            el.child(
                luma_ui::float::label(message.clone())
                    .text_color(ladder::danger())
                    .agent_node(Role::Text, message),
            )
        })
        .when(!state.selected.is_empty(), |el| {
            el.child(luma_ui::caption(format!(
                "{} selected",
                state.selected.len()
            )))
        })
        // The cursor's own readout: a point reads as one time, a range as the
        // span it covers. It is the only text that says where an edit would
        // land.
        .when_some(state.cursor, |el, cursor| {
            el.child(luma_ui::caption(match cursor.span() {
                Some((from, to)) => format!("Cursor {from:.2}-{to:.2}"),
                None => format!("Cursor {:.2}", cursor.start),
            }))
        })
        .when_some(state.loop_region, |el, (from, to)| {
            el.child(luma_ui::caption(format!("Loop {from:.2}-{to:.2}")))
        })
        .when(state.follow, |el| el.child(luma_ui::caption("Follow")))
        .child(div().flex_1())
        // Which score is on the timeline, by the handle the sidebar names it
        // by.
        .child(luma_ui::caption(format!("Score #{}", state.score.ordinal)))
        // A refused write, over the timeline it was refused for.
        .when_some(
            state
                .error
                .clone()
                .or_else(|| state.gpu_waveform.borrow().error.clone())
                .or_else(|| state.timeline_waveform.error.clone())
                .or_else(|| state.overview_waveform.error.clone())
                .or_else(|| state.preview_errors.values().next().cloned())
                .filter(|_| state.waveform.is_some()),
            |el, message| {
                el.child(
                    luma_ui::float::label(message.clone())
                        .text_color(ladder::danger())
                        .agent_node(Role::Text, message),
                )
            },
        )
        .when(state.score.read_only, |el| {
            el.child(luma_ui::caption("Read only"))
        })
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
    // Present a completed physical-pixel waveform and its camera together.
    // This adds one pipeline frame of latency, without resampling the texture
    // or letting the grid run ahead of the waveform during focus playback.
    let (view, playhead) = state
        .timeline_waveform
        .frame
        .as_ref()
        .and_then(|frame| frame.camera)
        .filter(|(view, _)| {
            state.follow
                && state.transport.playing
                && view.zoom == state.view.zoom
                && !waveform::updates_frozen()
        })
        .map(|(view, position)| {
            // At either scroll limit the waveform is stationary and reusable;
            // its original capture time must not freeze the moving playhead.
            let position = if view.scroll == state.view.scroll {
                state.transport.position
            } else {
                position
            };
            (view, position)
        })
        .unwrap_or((state.view, state.transport.position));
    let scene = Scene {
        clips: Rc::clone(&state.clips),
        previews: Rc::clone(&state.previews),
        waveform: state.timeline_waveform.frame.clone(),
        beats: state.beats.clone(),
        view,
        playhead,
        trace_active: state.transport.playing && state.follow,
        selected: state.selected.clone(),
        cursor: state.cursor,
        loop_region: state.loop_region,
        menu: state.menu,
        ghost: state.drop_ghost().map(Rc::new),
    };
    let registered = scene.clone();
    // Over an alpha line, the cursor says what a drag does. A drag keeps it
    // wherever the pointer goes.
    let cursor = match &state.gesture {
        Some(Gesture::Alpha { grab, .. }) => Some((grab.part.cursor(true), true)),
        Some(Gesture::Pan { .. }) => Some((CursorStyle::ClosedHand, true)),
        Some(_) => None,
        None => state.alpha_hover.map(|part| (part.cursor(false), false)),
    };
    let canvas_bounds = Rc::clone(&state.canvas);
    let app = app.clone();
    let resized = app.clone();
    let dropped = app.clone();
    let carried = app.clone();

    div()
        .flex_1()
        .overflow_hidden()
        // A preset carried from the browser shows where it would land while
        // it is over the timeline, and lands there when it is let go.
        .on_drag_move(move |event: &DragMoveEvent<sheet::PresetDrag>, _, cx| {
            let at = event.event.position;
            let over = event.bounds.contains(&at).then_some(at);
            let drag = *event.drag(cx);
            carried.update(cx, |this, cx| this.carry_preset(&drag, over, cx));
        })
        .on_drop(move |drag: &sheet::PresetDrag, window, cx| {
            let at = window.mouse_position();
            dropped.update(cx, |this, cx| this.drop_preset(drag, at, cx));
        })
        .child(
            canvas(
                move |bounds, window, cx| {
                    // Where the canvas ended up is what turns a window-space mouse
                    // position back into a time, and only prepaint knows it. A
                    // press can arrive before the next paint but never before the
                    // next prepaint, so this is also the only place it is safe to
                    // write.
                    //
                    canvas_bounds.set(bounds);
                    waveform::prepaint(&resized, false, bounds, window, cx);
                    register(&registered, bounds, window, cx);
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                move |bounds, hitbox, window, cx| {
                    paint(bounds, &scene, window, cx);
                    match cursor {
                        Some((style, true)) => window.set_window_cursor_style(style),
                        Some((style, false)) => window.set_cursor_style(style, &hitbox),
                        None => {}
                    }
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
    previews: Rc<RefCell<HashMap<SharedString, Installed>>>,
    waveform: Option<Rc<waveform::Painted>>,
    beats: Option<Rc<BeatGrid>>,
    view: View,
    playhead: f32,
    trace_active: bool,
    selected: Vec<SharedString>,
    cursor: Option<Cursor>,
    loop_region: Option<(f64, f64)>,
    menu: Option<InsertMenu>,
    /// A preset carried over the timeline from the browser.
    ghost: Option<Rc<sheet::DropGhost>>,
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
        clip_bounds(self.view, self.layout(canvas), canvas, clip)
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

/// One clip's box in window space, for a canvas at `canvas`.
fn clip_bounds(view: View, layout: Layout, canvas: Bounds<Pixels>, clip: &Clip) -> Bounds<Pixels> {
    let x = view.x_of(clip.start);
    let width = ((clip.end - clip.start) as f32 * view.zoom).floor().max(4.);
    Bounds {
        origin: point(
            canvas.origin.x + px(x),
            canvas.origin.y + px(layout.top(clip.row) + 1.),
        ),
        size: size(px(width), px(layout.lane - 2.)),
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
                fades::register(box_, clip, window, cx);
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
            // A carried preset is evidence of where a drop would land.
            if let Some(ghost) = &scene.ghost {
                agent_paint_node(
                    Role::Card,
                    format!("{} drop preview", ghost.clip.label),
                    ghost_box(canvas, layout, scene, ghost),
                    window,
                    cx,
                );
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
        if phase == DispatchPhase::Capture {
            let opens_pressed = pressed.update(cx, |this, cx| {
                let Some(Body::TrackEditor(editor)) = this.workspace.active_body_mut() else {
                    return false;
                };
                let pans = event.modifiers.secondary() && event.modifiers.alt;
                if event.button != MouseButton::Left || event.click_count != 2 || pans {
                    editor.pressed_clip = None;
                    return false;
                }
                let same_press = editor.pressed_clip.as_ref().is_some_and(|(_, point)| {
                    (point.x - event.position.x).abs() <= px(5.)
                        && (point.y - event.position.y).abs() <= px(5.)
                });
                if same_press {
                    this.timeline_open_pattern(event.position, cx);
                }
                same_press
            });
            if opens_pressed {
                cx.stop_propagation();
            }
            return;
        }
        if phase != DispatchPhase::Bubble || !inside.is_hovered(window) {
            return;
        }
        let at = event.position;
        // The gestures that share one press, told apart by button, keys and
        // count, the way the platform tells them apart: the right button
        // offers an insertion, the middle button (or the left with the
        // platform key and Alt) pans, a second left click opens the clip's
        // pattern, and a first left click is the pointer contract in
        // `timeline_press`.
        match (event.button, event.click_count) {
            (MouseButton::Right, _) => {
                pressed.update(cx, |this, cx| {
                    this.timeline_insert_menu(at, cx);
                    if let Some(Body::TrackEditor(editor)) = this.workspace.active_body() {
                        if editor.menu.is_some() {
                            editor.menu_search.focus_handle(cx).focus(window, cx);
                        }
                    }
                });
            }
            (MouseButton::Middle, _) => {
                pressed.update(cx, |this, cx| {
                    this.timeline_pan(at, MouseButton::Middle, cx)
                });
            }
            (MouseButton::Left, _) if event.modifiers.secondary() && event.modifiers.alt => {
                pressed.update(cx, |this, cx| this.timeline_pan(at, MouseButton::Left, cx));
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
        if phase == DispatchPhase::Bubble
            && matches!(event.button, MouseButton::Left | MouseButton::Middle)
        {
            let button = event.button;
            released.update(cx, |this, cx| this.timeline_release(button, cx));
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
        // A modified wheel zooms and a bare one scrolls. Two modifiers, two
        // rates: the platform key is a wheel and control is a trackpad pinch,
        // which sends a fifth the distance for the same gesture.
        //
        // The sign is already right without a negation: gpui reports a wheel
        // in the direction the *content* moves.
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

/// Paint the timeline in order: ground, ruler,
/// waveform, lanes and clips, playhead.
///
/// The beat grid goes down *before* the waveform and the clips, so its lines
/// run under both — which is what makes
/// a clip's translucent body show the beats through it.
fn paint(bounds: Bounds<Pixels>, scene: &Scene, window: &mut Window, cx: &mut App) {
    let started = std::time::Instant::now();
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
        paint_waveform(bounds, scene, window);
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
                if let Some(ghost) = &scene.ghost {
                    paint_ghost(bounds, layout, scene, ghost, window, cx);
                }
            },
        );
        if let Some(region) = scene.loop_region {
            paint_loop(bounds, region, scene.view, window);
        }
        paint_playhead(bounds, scene, start, end, window);
    });
    trace::frame(
        scene.trace_active,
        started,
        scene.view.scroll,
        scene.view.zoom,
        scene.playhead,
        window.is_window_active(),
    );
}

/// The box a canvas 2D stroke of `width` centred on `x + 0.5` actually covers.
/// Every timeline coordinate is written that way; this is the one place the
/// spelling is converted.
fn hairline(canvas: Bounds<Pixels>, x: f32, top: f32, bottom: f32, width: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            canvas.origin.x + px(x + 0.5 - width / 2.),
            canvas.origin.y + px(top),
        ),
        size: size(px(width), px(bottom - top)),
    }
}

/// The same color at a different alpha.
fn fade(color: Rgba, alpha: f32) -> Hsla {
    let mut color: Hsla = color.into();
    color.a = alpha;
    color
}

/// One rung of the grid ladder, from finest to coarsest.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Grid {
    /// `n` lines to the beat: 4, 2 or 1.
    Beat(u32),
    /// A line every `n` bars: 1, 2, 4 and on.
    Bars(u32),
}

impl Grid {
    /// The ladder, finest first. Each rung has twice the spacing of the one
    /// before it, except beat to bar, which is the time signature.
    const LADDER: [Self; 14] = [
        Self::Beat(4),
        Self::Beat(2),
        Self::Beat(1),
        Self::Bars(1),
        Self::Bars(2),
        Self::Bars(4),
        Self::Bars(8),
        Self::Bars(16),
        Self::Bars(32),
        Self::Bars(64),
        Self::Bars(128),
        Self::Bars(256),
        Self::Bars(512),
        Self::Bars(1024),
    ];

    /// The pixels between two lines of this rung.
    fn spacing(self, beat: f32, bar: f32) -> f32 {
        match self {
            Self::Beat(n) => beat / n as f32,
            Self::Bars(n) => bar * n as f32,
        }
    }

    /// The finest rung whose lines are at least `minimum` pixels apart, and
    /// that spacing.
    fn fit(beat: f32, bar: f32, minimum: f32) -> (Self, f32) {
        Self::LADDER
            .iter()
            .map(|grid| (*grid, grid.spacing(beat, bar)))
            .find(|(_, spacing)| *spacing >= minimum)
            .unwrap_or((Self::Bars(1024), Self::Bars(1024).spacing(beat, bar)))
    }

    /// The rung the grid shows at `zoom`, and how far its lines have faded
    /// in: 0 when they have just enough room, 1 at twice that.
    fn at(beats: &BeatGrid, zoom: f32) -> (Self, f32) {
        let (beat, bar) = mean_lengths(beats);
        let (grid, spacing) = Self::fit(beat * zoom, bar * zoom, MIN_GRID_SPACING);
        let fade = ((spacing - MIN_GRID_SPACING) / MIN_GRID_SPACING).clamp(0., 1.);
        // Smoothstep, so a rung eases in and out rather than ramping.
        (grid, fade * fade * (3. - 2. * fade))
    }
}

/// The smallest gap between two drawn grid lines, in pixels. A rung fades in
/// from here and is fully drawn at twice this.
const MIN_GRID_SPACING: f32 = 12.;

/// The smallest gap a snap target needs: a rung snaps once it is half faded
/// in, so a cursor never catches on a line the eye cannot see.
const MIN_SNAP_SPACING: f32 = 18.;

/// The mean beat and bar, in seconds.
///
/// The bar comes from the gap between the first two downbeats, and falls back
/// to the mean beat times the time signature with a single downbeat.
fn mean_lengths(beats: &BeatGrid) -> (f32, f32) {
    let beat = if beats.beats.len() > 1 {
        (beats.beats[beats.beats.len() - 1] - beats.beats[0]) / (beats.beats.len() - 1) as f32
    } else {
        0.5
    };
    let bar = if beats.downbeats.len() > 1 {
        beats.downbeats[1] - beats.downbeats[0]
    } else {
        beat * if beats.beats_per_bar == 0 {
            4.
        } else {
            beats.beats_per_bar as f32
        }
    };
    (beat, bar)
}

/// The grid behind the timeline, which follows the zoom without a single
/// jump.
///
/// Bars are shaded even and odd, in blocks that run from one bar number to
/// the next, counted from bar 1. Lines are drawn at the finest rung with room,
/// and that rung fades in as the zoom gives it more room. Bar lines are the
/// strongest, beats weaker and beat subdivisions the faintest.
///
/// When the bar numbers halve or double, nothing snaps: the bars that gain or
/// lose a number fade their number, their line weight and the shading edge
/// they carry, over one doubling of the zoom. See [`Labels`].
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
    let (grid, fade_in) = Grid::at(beats, view.zoom);
    let (beat_length, bar_length) = mean_lengths(beats);
    let labels = Labels::at(bar_length * view.zoom);
    let downbeat_count = beats.downbeats.len();

    // Even and odd blocks, bar by bar, merged into runs of one shade so that
    // two quads never meet mid-block. A bar past the last downbeat ends where
    // the mean bar says it would.
    let bar_at = |index: usize| -> f64 {
        match beats.downbeats.get(index) {
            Some(time) => f64::from(*time),
            None => {
                f64::from(beats.downbeats[downbeat_count - 1])
                    + f64::from(bar_length) * (index + 1 - downbeat_count) as f64
            }
        }
    };
    let first = beats
        .downbeats
        .partition_point(|bar| f64::from(*bar) <= start)
        .saturating_sub(1);
    let mut run: Option<(f64, f32)> = None;
    let shade = |left: f64, right: f64, value: f32, window: &mut Window| {
        if value <= 0. {
            return;
        }
        let (left, right) = (view.x_of(left), view.x_of(right));
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    canvas.origin.x + px(left),
                    canvas.origin.y + px(HEADER_HEIGHT),
                ),
                size: size(px(right - left), px(height - HEADER_HEIGHT)),
            },
            fade(ladder::stripe(), value),
        ));
    };
    for index in first..downbeat_count {
        let left = bar_at(index);
        if left > end {
            break;
        }
        let value = labels.shade(index);
        match run {
            Some((_, shaded)) if shaded == value => {}
            Some((from, shaded)) => {
                shade(from, left, shaded, window);
                run = Some((left, value));
            }
            None => run = Some((left, value)),
        }
    }
    if let Some((from, shaded)) = run {
        let last = beats
            .downbeats
            .partition_point(|bar| f64::from(*bar) <= end)
            .max(first + 1);
        shade(from, bar_at(last), shaded, window);
    }

    // Millisecond-rounded, which is what de-duplicates a downbeat against the
    // beat it sits on. Exact float equality would draw both, a hair apart.
    let downbeats: std::collections::HashSet<i64> = beats
        .downbeats
        .iter()
        .map(|time| (f64::from(*time) * 1000.).round() as i64)
        .collect();

    if let Grid::Beat(divisions) = grid {
        // The ruler's beat ticks fade in between 80 and 120 px/s.
        let tick = ((view.zoom - 80.) / 40.).clamp(0., 1.);
        for (index, beat) in beats.beats.iter().enumerate() {
            let beat = f64::from(*beat);
            let next = beats
                .beats
                .get(index + 1)
                .map_or(beat + f64::from(beat_length), |next| f64::from(*next));
            if next < start {
                continue;
            }
            if beat > end {
                break;
            }
            // Subdivisions: the odd ones belong to the finest rung alone.
            for part in 1..divisions {
                let finest = divisions == 2 || part % 2 == 1;
                let alpha = 0.12 * if finest { fade_in } else { 1. };
                let x = view.x_of(beat + (next - beat) * f64::from(part) / f64::from(divisions));
                window.paint_quad(fill(
                    hairline(canvas, x, HEADER_HEIGHT, height, 1.),
                    fade(ladder::primary(), alpha),
                ));
            }
            if beat < start || downbeats.contains(&((beat * 1000.).round() as i64)) {
                continue;
            }
            let alpha = 0.25 * if divisions == 1 { fade_in } else { 1. };
            let x = view.x_of(beat);
            window.paint_quad(fill(
                hairline(canvas, x, HEADER_HEIGHT, height, 1.),
                fade(ladder::primary(), alpha),
            ));
            if tick > 0. {
                window.paint_quad(fill(
                    hairline(canvas, x, HEADER_HEIGHT - 5., HEADER_HEIGHT, 1.),
                    fade(ladder::primary(), alpha * tick),
                ));
            }
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
        let major = labels.weight(index);
        // At a bar rung, only every `n`-th bar has a line, and the odd ones of
        // those belong to the finest rung alone.
        let presence = match grid {
            Grid::Beat(_) => 1.,
            Grid::Bars(n) if index % n as usize != 0 => 0.,
            Grid::Bars(n) if index % (2 * n as usize) != 0 => fade_in,
            Grid::Bars(_) => 1.,
        };
        let alpha = lerp(0.35 * presence, 0.6, major);
        if alpha <= 0. {
            continue;
        }
        let x = view.x_of(downbeat);
        window.paint_quad(fill(
            hairline(
                canvas,
                x,
                HEADER_HEIGHT - lerp(8., 12., major),
                height,
                lerp(1., 2., major),
            ),
            fade(ladder::primary(), alpha),
        ));
        if major > 0. {
            let mut color = ladder::foreground();
            color.a *= major;
            label(
                canvas,
                x + 4.,
                HEADER_HEIGHT - 10.,
                &(index + 1).to_string().into(),
                color,
                window,
                cx,
            );
        }
    }
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

/// Where the bar numbers are, as a smooth function of the zoom.
///
/// `getBarLabelStep` put a number on every `step`-th bar, `step` the power of
/// two that keeps numbers about [`LABEL_SPACING`] apart, and the shading
/// alternates at the same step. Here `level` is `log2` of the exact step
/// that spacing asks for. At a whole level, the numbered bars are the
/// multiples of `2^level`. Between two levels, the multiples of `2^level`
/// that are not multiples of `2^(level + 1)` are on their way out, and
/// `blend` is how far.
struct Labels {
    level: u32,
    blend: f32,
}

/// The gap bar numbers keep, in pixels.
const LABEL_SPACING: f32 = 80.;

impl Labels {
    fn at(bar: f32) -> Self {
        let exact = (LABEL_SPACING / bar.max(f32::EPSILON)).log2().max(0.);
        let level = exact.floor();
        let blend = exact - level;
        Self {
            level: (level as u32).min(30),
            // Smoothstep, so each change eases in and out.
            blend: blend * blend * (3. - 2. * blend),
        }
    }

    /// How much bar `index` (0 is bar 1) is a numbered bar: 1 for a full
    /// number and a heavy line, 0 for a plain bar.
    fn weight(&self, index: usize) -> f32 {
        let rank = if index == 0 {
            u32::MAX
        } else {
            index.trailing_zeros()
        };
        if rank > self.level {
            1.
        } else if rank == self.level {
            1. - self.blend
        } else {
            0.
        }
    }

    /// How shaded bar `index` is, from 0 to 1: the odd blocks of the step
    /// being left, blended into the odd blocks of the step being taken.
    fn shade(&self, index: usize) -> f32 {
        let odd = |level: u32| ((index >> level.min(63)) & 1) as f32;
        lerp(odd(self.level), odd(self.level + 1), self.blend)
    }
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

/// Draw the main strip using the same pixel sampler as the overview.
fn paint_waveform(canvas: Bounds<Pixels>, scene: &Scene, window: &mut Window) {
    waveform::paint(
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(HEADER_HEIGHT)),
            size: size(canvas.size.width, px(WAVEFORM_HEIGHT)),
        },
        scene.waveform.as_deref(),
        Some(scene.view),
        window,
    );
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

    // The navigation surface above lane 0 and the floor below the last one are
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

/// Where a carried preset would sit: in its lane, or across the boundary
/// where it would open a new one.
fn ghost_box(
    canvas: Bounds<Pixels>,
    layout: Layout,
    scene: &Scene,
    ghost: &sheet::DropGhost,
) -> Bounds<Pixels> {
    let mut box_ = scene.clip_box(canvas, &ghost.clip);
    if ghost.insert {
        box_.origin.y -= px(layout.lane / 2.);
    }
    box_
}

/// A carried preset, drawn as the clip it would make, with its strip.
fn paint_ghost(
    canvas: Bounds<Pixels>,
    layout: Layout,
    scene: &Scene,
    ghost: &sheet::DropGhost,
    window: &mut Window,
    cx: &mut App,
) {
    if ghost.insert {
        let menu = InsertMenu {
            start: ghost.clip.start,
            end: ghost.clip.end,
            row: ghost.clip.row,
            insert: true,
            active: 0,
        };
        paint_insertion(canvas, layout, menu, scene.view, window);
    }
    let previews = RefCell::new(
        ghost
            .strip
            .clone()
            .map(|preview| {
                (
                    ghost.clip.id.clone(),
                    Installed {
                        preview,
                        published: true,
                    },
                )
            })
            .into_iter()
            .collect(),
    );
    paint_clip(
        ghost_box(canvas, layout, scene, ghost),
        &ghost.clip,
        &previews,
        true,
        window,
        cx,
    );
}

/// One clip: an opaque header plate over a translucent body — the heatmap
/// where one has landed, the pattern colour where none has — inside a border,
/// with grab handles at both ends once it is selected.
fn paint_clip(
    box_: Bounds<Pixels>,
    clip: &Clip,
    previews: &RefCell<HashMap<SharedString, Installed>>,
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
    fades::paint(box_, clip, selected, window);
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
            // Three grip dots down the middle of each plate. Each is a 2px
            // square; a round dot would look the same at this size.
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
    // The clip clips its own label: a name too long for the header is cut off
    // at the edge. The name is the verb, bright; the summary after it is the
    // detail, dimmer.
    let text = Bounds {
        origin: point(box_.origin.x + px(8.), box_.origin.y),
        size: size(box_.size.width - px(16.), px(CLIP_HEADER)),
    };
    window.with_content_mask(Some(ContentMask { bounds: text }), |window| {
        let at = point(
            box_.origin.x + px(9.),
            box_.origin.y + px(12. - LABEL_SIZE * paint::ASCENT),
        );
        let color = ink(clip.color);
        let name = paint::shape(&clip.label, LABEL_SIZE, FontWeight::NORMAL, color, window);
        let width = name.width;
        name.paint(
            at,
            px(LABEL_SIZE * paint::LINE_HEIGHT),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
        if !clip.summary.is_empty() && clip.summary != clip.label {
            let detail: SharedString = format!(" · {}", clip.summary).into();
            paint::line(
                point(at.x + width, at.y),
                &detail,
                LABEL_SIZE,
                FontWeight::NORMAL,
                fade(color, 0.55).into(),
                window,
                cx,
            );
        }
    });
}

/// The translucency of an unselected clip's body. It lets the beat grid,
/// painted first, read through both the flat fill and the heatmap; selection
/// goes opaque.
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

/// A clip's [`Preview`] over its body — see [`Preview::paint`] — after
/// telling the atlas about a fresh bake under the clip's kept identity.
fn paint_preview(
    body: Bounds<Pixels>,
    clip: &Clip,
    previews: &RefCell<HashMap<SharedString, Installed>>,
    selected: bool,
    window: &mut Window,
) -> bool {
    let mut previews = previews.borrow_mut();
    let Some(installed) = previews.get_mut(&clip.id) else {
        return false;
    };
    if let (Preview::Heatmap(image), false) = (&installed.preview, installed.published) {
        // A re-render lands under the clip's kept atlas identity, and the
        // atlas answers paints from its cache, so it has to be told: same-size
        // tiles are refreshed in place, resized ones evicted for the paint
        // to re-insert.
        window.update_image(image).ok();
        installed.published = true;
    }
    installed
        .preview
        .paint(body, selected, Corners::default(), window)
}

/// The least body height that holds both of an aim clip's bands.
const AIM_BODY: f32 = 12.;

/// Stroke an aim clip's curves across its body: pan in the top band, tilt in
/// the bottom, each in its own trace colour on the dark ground. Both bands
/// are 0–1 from the seam, bottom to top, so zoom only changes where the
/// points land. Answers false for a body too short for two bands.
///
/// A curve keeps about one point per 2px of body: a zoomed-out clip strokes
/// a few dozen points, and a zoomed-in one every sample the seam sent.
fn paint_aim(
    body: Bounds<Pixels>,
    curves: &AimCurves,
    selected: bool,
    corners: Corners<Pixels>,
    window: &mut Window,
) -> bool {
    if f32::from(body.size.height) < AIM_BODY {
        return false;
    }
    let alpha = if selected { 1. } else { BODY_ALPHA };
    window.paint_quad(fill(body, fade(ladder::background(), alpha)).corner_radii(corners));
    let half = body.size.height / 2.;
    // The line between the bands.
    window.paint_quad(fill(
        Bounds {
            origin: point(body.origin.x, body.origin.y + half),
            size: size(body.size.width, px(1.)),
        },
        fade(ladder::foreground(), 0.12),
    ));
    let width = f32::from(body.size.width);
    for (top, band, color) in [
        (body.origin.y, &curves.pan, ladder::aim_pan()),
        (body.origin.y + half, &curves.tilt, ladder::aim_tilt()),
    ] {
        // 3px of air above and below each band, for the stroke's width.
        let (top, height) = (top + px(3.), half - px(6.));
        let mut path = PathBuilder::stroke(px(1.5));
        for curve in band.iter().filter(|curve| curve.len() > 1) {
            let last = curve.len() - 1;
            let stride = ((last as f32 / (width / 2.).max(1.)).ceil() as usize).max(1);
            let at = |i: usize| {
                point(
                    body.origin.x + body.size.width * (i as f32 / last as f32),
                    top + height * (1. - curve[i].clamp(0., 1.)),
                )
            };
            path.move_to(at(0));
            for i in (stride..last).step_by(stride).chain([last]) {
                path.line_to(at(i));
            }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, fade(color, alpha));
        }
    }
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
/// which is what the pointer gets: the mark is 6px and the grab is 8px.
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

/// The loop band's yellow, `rgb(234 179 8)`. It is not on the grey ladder because nothing else on this canvas needs
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

/// One line of canvas text, placed by its baseline.
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

#[cfg(test)]
mod tests {
    use super::{bar_snap, shift_time, Cursor, Grid, Labels, MIN_GRID_SPACING};

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

    #[test]
    fn bar_numbers_and_shading_change_without_a_jump() {
        // Bars from 200 px down to 2 px, in small steps: no bar's number,
        // line weight or shade may move more than a little between two.
        let mut previous: Option<Labels> = None;
        let mut bar = 200_f32;
        while bar > 2. {
            let labels = Labels::at(bar);
            if let Some(previous) = &previous {
                for index in 0..256 {
                    assert!((labels.weight(index) - previous.weight(index)).abs() < 0.05);
                    assert!((labels.shade(index) - previous.shade(index)).abs() < 0.05);
                }
            }
            previous = Some(labels);
            bar *= 0.995;
        }
        // At 40 px a bar, every 2nd bar is numbered and blocks are 2 bars.
        let labels = Labels::at(40.);
        assert_eq!(
            [labels.weight(0), labels.weight(1), labels.weight(2)],
            [1., 0., 1.]
        );
        assert_eq!(
            (0..4).map(|index| labels.shade(index)).collect::<Vec<_>>(),
            [0., 0., 1., 1.]
        );
    }

    #[test]
    fn the_grid_climbs_the_ladder_as_the_zoom_drops() {
        // A 0.5 s beat in 4/4, in pixels at each zoom.
        let fit = |zoom: f32| Grid::fit(0.5 * zoom, 2. * zoom, MIN_GRID_SPACING).0;
        assert_eq!(fit(200.), Grid::Beat(4));
        assert_eq!(fit(50.), Grid::Beat(2));
        assert_eq!(fit(25.), Grid::Beat(1));
        assert_eq!(fit(10.), Grid::Bars(1));
        assert_eq!(fit(2.), Grid::Bars(4));
    }

    #[test]
    fn a_bar_snap_counts_its_blocks_from_bar_one() {
        let bars: Vec<f32> = (0..9).map(|bar| 1. + 2. * bar as f32).collect();
        assert_eq!(bar_snap(&bars, 4.9, 1), 5.);
        // Every 4th bar is 1 s and 9 s: 4.9 is nearer 1, 5.1 nearer 9.
        assert_eq!(bar_snap(&bars, 4.9, 4), 1.);
        assert_eq!(bar_snap(&bars, 5.1, 4), 9.);
        // Before bar 1 and past the last block.
        assert_eq!(bar_snap(&bars, 0., 4), 1.);
        assert_eq!(bar_snap(&bars, 30., 4), 17.);
    }

    /// Beats that alternate between 0.4285 s and 0.4286 s, like a detected
    /// grid stored to 0.1 ms.
    fn uneven() -> luma_patterns::BeatTimeline {
        let seconds = (0..40)
            .scan(0., |at, i| {
                let beat = *at;
                *at += if i % 2 == 0 { 0.4285 } else { 0.4286 };
                Some(beat)
            })
            .collect();
        luma_patterns::BeatTimeline::new(seconds, 0.).unwrap()
    }

    #[test]
    fn a_shift_moves_whole_beats_on_an_uneven_grid() {
        let clock = uneven();
        let at = |beat: f64| clock.seconds_at(beat).unwrap();
        // A one-beat clip moved from beat 0 to beat 1 must still end on a beat.
        let end = shift_time(Some(&clock), at(1.), at(0.), at(1.));
        assert!((clock.beat_at(end).unwrap() - 2.).abs() < 1e-9);
        // The seconds arithmetic this replaced lands 0.1 ms off the grid.
        assert!(((at(1.) + at(1.) - at(0.)) - at(2.)).abs() > 5e-5);
    }

    #[test]
    fn a_shift_of_nothing_keeps_the_exact_time() {
        let clock = uneven();
        let time = clock.seconds_at(2.5).unwrap() + 1e-13;
        assert_eq!(shift_time(Some(&clock), time, 3., 3.), time);
    }
}
