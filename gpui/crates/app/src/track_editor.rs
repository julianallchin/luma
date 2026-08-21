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
//! ([`Fine`]), and paints that. In between, where the stored envelope has
//! exactly its bucket a pixel, the two agree exactly.
//!
//! # What v1 is
//!
//! Open, look, listen, and move a clip's edges. Selecting a clip and dragging
//! either edge persists through `update_track_score`; moving a whole clip
//! between lanes, creating and deleting clips, the pattern inspector, the
//! agent sidebar, stem views and the minimap do not exist here yet. Vertical
//! zoom is fixed at 1 — the web's `zoomY` only scales lanes, and there is no
//! vertical scroll here for it to interact with.
//!
//! A clip is named in the automation tree by its *pattern*, so two clips of
//! one pattern are two nodes with one label. Their edge handles are separate
//! nodes (`"<pattern> start"` / `"<pattern> end"`), which is what a script
//! drags — the clip's own centre is nowhere near either edge.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::node::{agent_paint_node, Instrument, Role};
use luma_ui::{ladder, paint};

use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::node_graph::BeatGrid;
use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::waveforms::TrackWaveform;
use luma_lib::services::track_edits::TrackClip;

use crate::{Luma, Screen};

// -- state --------------------------------------------------------------------

/// The screen's whole state: the track it is showing, everything the seam
/// returned for it, where the eye is, and where the transport is.
pub struct Editor {
    track_id: String,
    track_name: String,
    /// The score whose clips are on the timeline, and whether this host may
    /// write to it. `None` until the lookup lands, and still `None` for a
    /// track this venue has never annotated — there is no score to edit then,
    /// and the web app shows the same empty lanes.
    score: Option<Score>,
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
    /// The clips, with their lanes resolved. Rebuilt whenever the clips change
    /// and never during a draw: a lane is a function of *every* clip's
    /// `zIndex`, so working it out per clip per frame would be quadratic in
    /// the one thing that grows.
    clips: Rc<[Clip]>,
    selected: Option<SharedString>,
    view: View,
    transport: Transport,
    gesture: Option<Gesture>,
    /// Where the canvas last painted, in window space. A mouse event arrives
    /// in window coordinates and has to be put back into timeline coordinates,
    /// which needs this; only `prepaint` knows it, and a `Cell` is how it gets
    /// written down there without notifying from inside a draw.
    canvas: Rc<Cell<Bounds<Pixels>>>,
    /// The edit waiting to be written, if any. One clip's bounds — the only
    /// edit this screen makes — replaced rather than queued, because a second
    /// drag of the same edge supersedes the first.
    pending: Option<Edit>,
    /// A write is in flight. Serialized rather than concurrent: the authored
    /// score resolves a partial update against whatever is current, and two in
    /// flight would resolve in whichever order the runtime happened to pick.
    saving: bool,
    error: Option<String>,
    /// Whether the screen's first load has finished. Written in one assignment
    /// with the data, so "still loading" and "nothing here" cannot be confused.
    loaded: bool,
}

/// The score being edited, and whether this host owns it.
struct Score {
    id: String,
    /// Somebody else's score: visible, not writable. The web browser computes
    /// the same flag from `score.uid !== currentUserId`.
    read_only: bool,
}

/// One clip, with everything a draw needs already resolved.
struct Clip {
    id: SharedString,
    /// The pattern's name, or the same `Pattern <id>` fallback the web label
    /// falls back to when the catalogue does not know it.
    label: SharedString,
    color: Rgba,
    start: f64,
    end: f64,
    /// Which lane it sits in, counting down from the empty insertion lane at
    /// row 0. Derived from every clip's `zIndex` together — see [`lanes`].
    row: usize,
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
    min: Vec<f32>,
    max: Vec<f32>,
    /// How loud each bucket was over the whole of it, as opposed to how far it
    /// reached. Drawn as the solid core inside the peak outline: at a bucket
    /// per pixel the peaks alone are a spiky hull that says nothing about
    /// where the energy is.
    rms: Vec<f32>,
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
            per_second: self.max.len() as f64 / (self.cut.end - self.cut.start),
            count: self.max.len(),
        }
    }
}

/// How much of a viewport is measured either side of it. A pan of up to half a
/// screen is free; the cost is that every window measures twice the audio it
/// shows, which is microseconds of a scan over a buffer already in memory.
const FINE_MARGIN: f64 = 0.5;

/// One clip's new bounds, on their way to the seam.
struct Edit {
    clip: SharedString,
    start: f64,
    end: f64,
}

/// Where the eye is: a horizontal zoom in pixels per second, and a scroll in
/// pixels. The same two numbers the web timeline's scroll container holds.
#[derive(Clone, Copy)]
struct View {
    zoom: f32,
    scroll: f32,
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

/// What the pointer is doing between a press and a release.
enum Gesture {
    /// Dragging the playhead over the ruler or the waveform.
    Scrub,
    /// Dragging the empty timeline sideways. `last` is the previous pointer
    /// position, so the pan follows the pointer exactly regardless of zoom.
    Pan { last: Pixels },
    /// Dragging one end of a clip. `grab` is how far into the clip, in
    /// seconds, the pointer took hold of that edge — so the edge moves by the
    /// drag's displacement rather than jumping to centre itself on the
    /// pointer. `moved` distinguishes a drag from a press that only selected.
    Edge {
        clip: SharedString,
        edge: Edge,
        grab: f64,
        moved: bool,
    },
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
    /// `ZOOM_SENSITIVITY`: the exponential rate a wheel notch scales by.
    const ZOOM_PER_PIXEL: f32 = 0.002;

    /// The time under a point `offset` pixels from the canvas's left edge.
    fn time_at(self, offset: f32) -> f64 {
        f64::from((offset + self.scroll) / self.zoom)
    }

    /// Where a time lands, in pixels from the canvas's left edge. Floored,
    /// because every coordinate in `timeline-drawing.ts` is.
    fn x_of(self, time: f64) -> f32 {
        (time as f32 * self.zoom - self.scroll).floor()
    }

    /// Scale about `offset`, keeping whatever is under it exactly where it is.
    fn zoom_about(&mut self, offset: f32, factor: f32) {
        let held = self.time_at(offset);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.scroll = (held as f32 * self.zoom - offset).max(0.);
    }
}

impl Editor {
    /// The track being edited. The window title is the only reader outside
    /// this module.
    pub(crate) fn track_name(&self) -> &str {
        &self.track_name
    }

    /// Whether an edit is allowed to land at all: a score this host owns, and
    /// no other reason to refuse.
    fn writable(&self) -> bool {
        self.score.as_ref().is_some_and(|score| !score.read_only)
    }

    /// The range on screen, from the canvas the last frame painted.
    fn visible(&self) -> (f64, f64) {
        let width = f32::from(self.canvas.get().size.width);
        (self.view.time_at(0.), self.view.time_at(width))
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

    /// The clip under `time` in `row`, if any.
    fn clip_at(&self, time: f64, row: usize) -> Option<&Clip> {
        self.clips
            .iter()
            .find(|clip| clip.row == row && time >= clip.start && time <= clip.end)
    }

    /// Move one edge of one clip to `time`, keeping the clip at least
    /// `MIN_CLIP` long and inside the track.
    fn drag_edge(&mut self, clip_id: &str, edge: Edge, time: f64) {
        let duration = f64::from(self.transport.duration).max(0.);
        let mut clips: Vec<Clip> = Vec::with_capacity(self.clips.len());
        for clip in self.clips.iter() {
            let (mut start, mut end) = (clip.start, clip.end);
            if clip.id == clip_id {
                match edge {
                    Edge::Start => start = time.clamp(0., end - MIN_CLIP),
                    Edge::End => end = time.clamp(start + MIN_CLIP, duration.max(start + MIN_CLIP)),
                }
            }
            clips.push(Clip {
                id: clip.id.clone(),
                label: clip.label.clone(),
                color: clip.color,
                start,
                end,
                row: clip.row,
            });
        }
        self.clips = clips.into();
    }

    /// One clip's current bounds, as an edit to write back.
    fn edit_for(&self, clip_id: &str) -> Option<Edit> {
        let clip = self.clips.iter().find(|clip| clip.id == clip_id)?;
        Some(Edit {
            clip: clip.id.clone(),
            start: clip.start,
            end: clip.end,
        })
    }
}

/// `MIN_ANNOTATION_DURATION` from `utils/timeline-constants.ts`.
const MIN_CLIP: f64 = 0.05;

/// Resolve every clip's lane, exactly as the web timeline's `rowMap` does:
/// the distinct `zIndex` values sorted ascending, then inverted so the highest
/// z is the *highest* lane on screen, and row 0 left empty as the insertion
/// lane above it.
fn lanes(clips: &[TrackClip]) -> HashMap<&str, usize> {
    let mut z: Vec<i64> = clips.iter().map(|clip| clip.z_index).collect();
    z.sort_unstable();
    z.dedup();
    let max_row = z.len().saturating_sub(1);
    clips
        .iter()
        .map(|clip| {
            let index = z.iter().position(|value| *value == clip.z_index);
            let row = index.map_or(max_row + 1, |index| max_row - index + 1);
            (clip.id.as_str(), row)
        })
        .collect()
}

/// Resolve the clips a load or a write returned into what the canvas draws.
fn resolve(clips: &[TrackClip], patterns: &HashMap<String, String>) -> Rc<[Clip]> {
    let rows = lanes(clips);
    clips
        .iter()
        .map(|clip| Clip {
            id: clip.id.clone().into(),
            label: patterns
                .get(&clip.pattern_id)
                .cloned()
                .unwrap_or_else(|| format!("Pattern {}", clip.pattern_id))
                .into(),
            color: ladder::pattern(&clip.pattern_id),
            start: clip.start_time,
            end: clip.end_time,
            row: rows.get(clip.id.as_str()).copied().unwrap_or(1),
        })
        .collect()
}

// -- navigation, gestures and writes ------------------------------------------
//
// These hang off `Luma` because opening a track is five `Library` calls plus a
// screen transition, and `Luma` owns both.

impl Luma {
    /// Navigate to a track's timeline, from the browser row that named it.
    ///
    /// The five reads are started together and awaited in order: they do not
    /// depend on each other, and each is already its own task on the Tokio
    /// runtime, so sequencing the `await`s costs nothing and keeps the
    /// assignment in one place. The clips are the exception — they are keyed
    /// by a score id that only the first read knows.
    pub(crate) fn open_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Screen::Tracks(browser) = &self.screen else {
            return;
        };
        let Some(track) = browser.find(track_id) else {
            return;
        };
        let venue_id = browser.venue_id().to_string();

        let waveform = self.library.track_waveform(track_id);
        let beats = self.library.track_beats(track_id);
        let scores = self.library.scores_for_track(track_id, &venue_id);
        let patterns = self.library.patterns();
        let audio = self.library.load_audio(track_id);

        // Take the browser whole rather than reloading it on the way back:
        // its filters and its search are the user's, and re-running the query
        // would throw them away. Settings does the same for the screen it
        // covers.
        let browser = std::mem::replace(
            &mut self.screen,
            Screen::Welcome {
                venues: Vec::new(),
                error: None,
            },
        );
        self.screen = Screen::TrackEditor {
            state: Box::new(Editor {
                track_id: track.id.clone(),
                track_name: track_title(&track),
                score: None,
                waveform: None,
                fine: None,
                fine_pending: None,
                beats: None,
                clips: Vec::new().into(),
                selected: None,
                view: View {
                    zoom: View::DEFAULT_ZOOM,
                    scroll: 0.,
                },
                transport: Transport::default(),
                gesture: None,
                canvas: Rc::new(Cell::new(Bounds::default())),
                pending: None,
                saving: false,
                error: None,
                loaded: false,
            }),
            browser: Box::new(browser),
        };
        cx.notify();

        let library = cx.entity();
        cx.spawn(async move |this, cx| {
            let waveform = waveform.await;
            let beats = beats.await;
            let patterns = patterns.await;
            let scores = scores.await;
            let audio = audio.await;

            let names: HashMap<String, String> = patterns
                .unwrap_or_default()
                .into_iter()
                .map(|pattern| (pattern.id, pattern.name))
                .collect();

            // The one dependent read: a clip list is keyed by a score id that
            // only the score lookup knows.
            let clips = match scores.as_ref().ok().and_then(|scores| scores.first()) {
                Some(score) => Some(
                    library
                        .read_with(cx, |this, _| this.library.track_scores(&score.id))
                        .await,
                ),
                None => None,
            };

            this.update(cx, |this, cx| {
                let user = this.library.user_id().map(str::to_string);
                this.with_track_editor(cx, |editor| {
                    editor.loaded = true;
                    match waveform {
                        Ok(waveform) => {
                            editor.transport.duration = waveform.duration_seconds as f32;
                            editor.waveform = Some(Rc::new(waveform));
                        }
                        Err(message) => editor.error = Some(message),
                    }
                    editor.beats = beats.ok().flatten().map(Rc::new);
                    if let Err(message) = audio {
                        editor.error = Some(message);
                    }
                    match scores {
                        Ok(scores) => {
                            editor.score = scores.first().map(|score| Score {
                                id: score.id.clone(),
                                read_only: score.uid.is_some() && score.uid != user,
                            });
                        }
                        Err(message) => editor.error = Some(message),
                    }
                    match clips {
                        Some(Ok(clips)) => {
                            let clips: Vec<TrackClip> =
                                clips.iter().map(TrackClip::from).collect::<Vec<_>>();
                            editor.clips = resolve(&clips, &names);
                        }
                        Some(Err(message)) => editor.error = Some(message),
                        None => {}
                    }
                });
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

    /// Leave the editor for the browser it was opened from, silencing the
    /// transport on the way out — audio that kept playing over another screen
    /// would be a second, invisible thing running.
    pub(crate) fn close_track_editor(&mut self, cx: &mut Context<Self>) {
        let Screen::TrackEditor { browser, .. } = &mut self.screen else {
            return;
        };
        let pause = self.library.pause();
        self.screen = *std::mem::replace(
            browser,
            Box::new(Screen::Welcome {
                venues: Vec::new(),
                error: None,
            }),
        );
        cx.notify();
        cx.background_spawn(async move {
            pause.await.ok();
        })
        .detach();
    }

    /// Start or stop playback, and read the transport back either way — the
    /// audio host is the authority on whether it is playing, so the button's
    /// own state is never assumed.
    pub(crate) fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        let Screen::TrackEditor { state, .. } = &self.screen else {
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
                if let Err(message) = step.await {
                    failed = Some(message);
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
        let Screen::TrackEditor { state, .. } = &mut self.screen else {
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
        snapshot: Result<HostAudioSnapshot, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Screen::TrackEditor { state, .. } = &mut self.screen else {
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
        cx.notify();
        snapshot.is_playing
    }

    /// A press on the canvas: take the playhead, an edge, or the timeline.
    fn timeline_press(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let mut seek = None;
        self.with_track_editor(cx, |editor| {
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let time = editor.view.time_at(offset);
            let y = f32::from(at.y - canvas.origin.y);

            // The header and the waveform are one scrubbing surface, as they
            // are on the web: everything above the lanes moves the playhead.
            if y < TRACK_AREA_Y {
                editor.gesture = Some(Gesture::Scrub);
                editor.transport.position = time.max(0.) as f32;
                seek = Some(editor.transport.position);
                return;
            }

            let Some(row) = row_at(y) else {
                editor.gesture = Some(Gesture::Pan { last: at.x });
                return;
            };
            let Some(clip) = editor.clip_at(time, row) else {
                editor.selected = None;
                editor.gesture = Some(Gesture::Pan { last: at.x });
                return;
            };
            let (id, start, end) = (clip.id.clone(), clip.start, clip.end);
            editor.selected = Some(id.clone());
            // Within a handle's width of either end, in *pixels* — a handle is
            // a fixed size on screen, so what it covers in seconds is a
            // function of the zoom.
            let handle = f64::from(HANDLE / editor.view.zoom);
            let edge = if time - start < handle {
                Edge::Start
            } else if end - time < handle {
                Edge::End
            } else {
                return;
            };
            if !editor.writable() {
                return;
            }
            editor.gesture = Some(Gesture::Edge {
                clip: id,
                edge,
                grab: time - if edge == Edge::Start { start } else { end },
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
        match &self.screen {
            Screen::TrackEditor { state, .. } if state.gesture.is_some() => {}
            _ => return,
        }
        let mut seek = None;
        let mut dragged = None;
        let mut panned = false;
        self.with_track_editor(cx, |editor| {
            let canvas = editor.canvas.get();
            let offset = f32::from(at.x - canvas.origin.x);
            let time = editor.view.time_at(offset);
            match &mut editor.gesture {
                Some(Gesture::Scrub) => {
                    editor.transport.position =
                        time.clamp(0., f64::from(editor.transport.duration)) as f32;
                    seek = Some(editor.transport.position);
                }
                Some(Gesture::Pan { last }) => {
                    let delta = f32::from(at.x - *last);
                    *last = at.x;
                    editor.view.scroll = (editor.view.scroll - delta).max(0.);
                    panned = true;
                }
                Some(Gesture::Edge {
                    clip,
                    edge,
                    grab,
                    moved,
                }) => {
                    *moved = true;
                    dragged = Some((clip.clone(), *edge, time - *grab));
                }
                None => {}
            }
        });
        if let Some((clip, edge, time)) = dragged {
            self.with_track_editor(cx, |editor| editor.drag_edge(&clip, edge, time));
        }
        if panned {
            self.ensure_fine_waveform(cx);
        }
        if let Some(seconds) = seek {
            self.seek(seconds, cx);
        }
    }

    /// A release. An edge that actually moved is written back; a press that
    /// only selected is not, because nothing changed.
    fn timeline_release(&mut self, cx: &mut Context<Self>) {
        match &self.screen {
            Screen::TrackEditor { state, .. } if state.gesture.is_some() => {}
            _ => return,
        }
        let mut save = false;
        self.with_track_editor(cx, |editor| {
            if let Some(Gesture::Edge {
                clip, moved: true, ..
            }) = editor.gesture.take()
            {
                editor.pending = editor.edit_for(&clip);
                save = editor.pending.is_some();
            }
        });
        if save {
            self.save_clip(cx);
        }
    }

    fn timeline_zoom(&mut self, at: Point<Pixels>, delta: Point<f32>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let offset = f32::from(at.x - editor.canvas.get().origin.x);
            if delta.y != 0. {
                // Exponential in the scroll distance, so a fast flick and a
                // slow one over the same distance land in the same place.
                editor
                    .view
                    .zoom_about(offset, (delta.y * View::ZOOM_PER_PIXEL).exp());
            }
            if delta.x != 0. {
                editor.view.scroll = (editor.view.scroll - delta.x).max(0.);
            }
        });
        self.ensure_fine_waveform(cx);
    }

    /// Measure the range on screen, if the stored envelope has run out of
    /// buckets for it and this measurement is not already in hand or in flight.
    ///
    /// Called from wherever the view can move — the two gestures that move it,
    /// and the prepaint that first tells the canvas how wide it is. The old
    /// measurement is kept until the new one lands, so a scrub or a pan draws
    /// the coarse envelope at worst and never a blank bed.
    fn ensure_fine_waveform(&mut self, cx: &mut Context<Self>) {
        let Screen::TrackEditor { state, .. } = &mut self.screen else {
            return;
        };
        let Some(want) = state.fine_window() else {
            // Back under the stored envelope's own resolution, where measuring
            // would only reproduce it.
            state.fine = None;
            return;
        };
        if state.fine_pending.is_some() || state.drawn_buckets().is_some() {
            return;
        }
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
                                buckets: measured.max.len(),
                                zoom: want.zoom,
                            },
                            duration,
                            min: measured.min,
                            max: measured.max,
                            rms: measured.rms,
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

    /// Write the pending clip bounds back.
    ///
    /// One write is in flight at a time; an edit made while one is running
    /// replaces [`Editor::pending`] and is flushed on its return. The result
    /// carries the whole authoritative clip list, so the screen adopts that
    /// rather than trusting what it drew — except while the pointer is still
    /// on a clip, where adopting it would undo a drag the user can see.
    fn save_clip(&mut self, cx: &mut Context<Self>) {
        let Screen::TrackEditor { state, .. } = &mut self.screen else {
            return;
        };
        if state.saving {
            return;
        }
        let (Some(score), Some(edit)) = (&state.score, state.pending.take()) else {
            return;
        };
        state.saving = true;
        state.error = None;
        // Minted per *edit*, not per attempt: this id is what lets the seam
        // replay a durable outcome rather than guess from a later snapshot.
        let operation = uuid::Uuid::new_v4().to_string();
        let pending = self.library.move_clip(
            &score.id,
            &state.track_id,
            &edit.clip,
            &operation,
            edit.start,
            edit.end,
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut flush = false;
                this.with_track_editor(cx, |editor| {
                    editor.saving = false;
                    match result {
                        Ok(saved) => {
                            if editor.pending.is_none() && editor.gesture.is_none() {
                                let labels = editor
                                    .clips
                                    .iter()
                                    .map(|clip| (clip.id.to_string(), clip.label.to_string()))
                                    .collect();
                                editor.clips = adopt(&saved.clips, &editor.clips, &labels);
                            }
                        }
                        // Includes the conflict case, which the seam does not
                        // type on the wire. There is nothing to merge in a
                        // bounds-only editor, so the honest recovery is to say
                        // so and let the next reopen re-read the truth.
                        Err(message) => editor.error = Some(message),
                    }
                    flush = editor.pending.is_some();
                });
                if flush {
                    this.save_clip(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Run `edit` against the track editor, if that is still what is showing.
    /// A load or a write that lands after the user navigated away is a no-op.
    fn with_track_editor(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Editor)) {
        if let Screen::TrackEditor { state, .. } = &mut self.screen {
            edit(state);
            cx.notify();
        }
    }
}

/// One step of a transport change. Boxed because a play is two commands and a
/// pause is one, and the two arms of that choice are different opaque future
/// types that only a `dyn` can hold in one list.
type Transition = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>;

/// How often the playhead is re-read while playing. 30 Hz — twice the rate the
/// desktop app's broadcaster emits at, because a poll's phase is arbitrary and
/// halving the period halves the worst-case lag.
const POLL: Duration = Duration::from_millis(33);

/// Adopt the seam's authoritative clip list, keeping the pattern names already
/// looked up. The names are the one thing the write does not return, and
/// re-reading `list_patterns` to relabel a clip that only moved would be a
/// round trip for a string this screen already has.
fn adopt(clips: &[TrackClip], current: &[Clip], labels: &HashMap<String, String>) -> Rc<[Clip]> {
    let mut names: HashMap<String, String> = HashMap::new();
    for clip in clips {
        // Keyed by pattern, so a clip the write *created* still gets the name
        // its pattern already had on another clip.
        if let Some(existing) = current.iter().find(|c| c.id == clip.id) {
            names.insert(clip.pattern_id.clone(), existing.label.to_string());
        } else if let Some(label) = labels.get(&clip.id) {
            names.insert(clip.pattern_id.clone(), label.clone());
        }
    }
    resolve(clips, &names)
}

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
// `utils/timeline-constants.ts`, at `zoomY == 1`. Vertical zoom only scales
// the lanes, and there is no vertical scroll here for it to anchor against.

/// The ruler strip: `HEADER_HEIGHT`.
const HEADER_HEIGHT: f32 = 32.;
/// `WAVEFORM_HEIGHT`. Fixed even under vertical zoom on the web too — it is a
/// navigation surface, not part of the annotation workspace.
const WAVEFORM_HEIGHT: f32 = 80.;
/// `TRACK_HEIGHT`: one lane.
const LANE_HEIGHT: f32 = 80.;
/// `layout.trackAreaY`: where the scrubbing surface ends and the lanes begin.
const TRACK_AREA_Y: f32 = HEADER_HEIGHT + WAVEFORM_HEIGHT;
/// `layout.trackStartY`: the first lane that can hold a clip. Row 0 is the
/// empty insertion lane above the topmost layer.
const TRACK_START_Y: f32 = TRACK_AREA_Y + LANE_HEIGHT;
/// `ANNOTATION_HEADER_H`: the opaque strip at the top of a clip.
const CLIP_HEADER: f32 = 18.;
/// The grab width of a clip's edge, in screen pixels. `handleSize` in
/// `components/timeline.tsx`.
const HANDLE: f32 = 8.;
/// The ruler's and the clip labels' type size.
const LABEL_SIZE: f32 = 10.;

/// Which lane a point `y` pixels down the canvas falls in, or `None` above the
/// lanes.
fn row_at(y: f32) -> Option<usize> {
    if y < TRACK_START_Y {
        return None;
    }
    Some(((y - TRACK_START_Y) / LANE_HEIGHT) as usize)
}

// -- rendering ----------------------------------------------------------------

/// Render the screen: a toolbar strip over the canvas.
///
/// The same split as the graph editor, and for the same reason — a panel is a
/// stack of boxes and gpui lays boxes out well, while a timeline is a
/// coordinate system nothing gpui lays out could express without a box per
/// clip per beat.
pub fn track_editor(state: &Editor, app: &Entity<Luma>) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        .child(match (&state.error, state.loaded) {
            (Some(message), _) => plate(message.clone(), ladder::danger().into()),
            (None, false) => plate(
                "Loading track…".to_string(),
                ladder::muted_foreground().into(),
            ),
            (None, true) => canvas_element(state, app).into_any_element(),
        })
}

/// The way back, what is open, the transport, and whether a write is in the
/// air.
fn toolbar(state: &Editor, app: &Entity<Luma>) -> Div {
    let back = app.clone();
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
            luma_ui::luma_button("Back", false)
                .id("back")
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.back(cx)))
                .agent_node(Role::Button, "Back"),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.track_name.clone())
                .agent_node(Role::Text, state.track_name.clone()),
        )
        .child(
            luma_ui::luma_button(if playing { "Pause" } else { "Play" }, false)
                .id("transport")
                // One button, two labels — the label *is* the state, so a
                // script reads what the transport is doing from the same place
                // a person does.
                .on_click(move |_, _, cx| transport.update(cx, |this, cx| this.toggle_playback(cx)))
                .agent_node(Role::Button, if playing { "Pause" } else { "Play" }),
        )
        .child(silkscreen(format!(
            "{} / {}",
            clock(state.transport.position),
            clock(state.transport.duration)
        )))
        .child(silkscreen(format!("{} CLIPS", state.clips.len())))
        // What the waveform is drawn from, and only while that is the measured
        // window: past the stored envelope's resolution the panel says how many
        // buckets the canvas actually has, the way an instrument reads out its
        // own range rather than leaving you to guess it.
        .when_some(state.drawn_buckets(), |el, buckets| {
            el.child(silkscreen(format!("FINE {buckets}")))
        })
        .child(div().flex_1())
        .when(state.score.is_none() && state.loaded, |el| {
            el.child(silkscreen("NO SCORE".to_string()))
        })
        .when(
            state.writable() && (state.saving || state.pending.is_some()),
            |el| el.child(silkscreen("SAVING".to_string())),
        )
        .when(
            state.score.as_ref().is_some_and(|score| score.read_only),
            |el| el.child(silkscreen("READ ONLY".to_string())),
        )
}

/// `M:SS`, the same clock the browser's TIME column reads in.
fn clock(seconds: f32) -> String {
    let total = if seconds.is_finite() {
        seconds.max(0.)
    } else {
        0.
    };
    format!("{}:{:02}", (total / 60.) as u64, (total % 60.) as u64)
}

/// 9px uppercase silkscreen, the panel's one label style.
fn silkscreen(label: String) -> impl IntoElement {
    div()
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(ladder::muted_foreground())
        .child(label.clone())
        .agent_node(Role::Text, label)
}

fn plate(message: String, color: Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .text_color(color)
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
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
        waveform: state.waveform.clone(),
        fine: state.fine.clone(),
        beats: state.beats.clone(),
        view: state.view,
        playhead: state.transport.position,
        selected: state.selected.clone(),
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
                // not notify.
                if canvas_bounds.replace(bounds).size.width != bounds.size.width {
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
    waveform: Option<Rc<TrackWaveform>>,
    fine: Option<Rc<Fine>>,
    beats: Option<Rc<BeatGrid>>,
    view: View,
    playhead: f32,
    selected: Option<SharedString>,
}

impl Scene {
    /// One clip's box in window space.
    fn clip_box(&self, canvas: Bounds<Pixels>, clip: &Clip) -> Bounds<Pixels> {
        let x = self.view.x_of(clip.start);
        let width = ((clip.end - clip.start) as f32 * self.view.zoom)
            .floor()
            .max(4.);
        Bounds {
            origin: point(
                canvas.origin.x + px(x),
                canvas.origin.y + px(TRACK_START_Y + clip.row as f32 * LANE_HEIGHT + 1.),
            ),
            size: size(px(width), px(LANE_HEIGHT - 2.)),
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
fn register(scene: &Scene, canvas: Bounds<Pixels>, window: &Window, cx: &mut App) {
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
    for clip in scene.clips.iter() {
        let box_ = scene.clip_box(canvas, clip);
        agent_paint_node(Role::Card, clip.label.clone(), box_, window, cx);
        for edge in [Edge::Start, Edge::End] {
            let x = match edge {
                Edge::Start => box_.origin.x,
                Edge::End => box_.origin.x + box_.size.width - px(HANDLE),
            };
            agent_paint_node(
                Role::Slider,
                format!("{} {}", clip.label, edge.suffix()),
                Bounds {
                    origin: point(x, box_.origin.y),
                    size: size(px(HANDLE), box_.size.height),
                },
                window,
                cx,
            );
        }
    }
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
        if phase != DispatchPhase::Bubble
            || event.button != MouseButton::Left
            || !inside.is_hovered(window)
        {
            return;
        }
        let at = event.position;
        pressed.update(cx, |this, cx| this.timeline_press(at, cx));
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
        if phase != DispatchPhase::Bubble || !over.is_hovered(window) {
            return;
        }
        let wheel = event.delta.pixel_delta(window.line_height());
        // The web timeline zooms on a modified wheel and scrolls on a bare
        // one, because a bare wheel there is the scroll container's. Same
        // split here, with the vertical axis standing in for the modifier's
        // absence on a trackpad.
        let delta = if event.modifiers.secondary() || event.modifiers.control {
            point(0., f32::from(wheel.y))
        } else {
            point(f32::from(wheel.x) + f32::from(wheel.y), 0.)
        };
        let at = event.position;
        zoomed.update(cx, |this, cx| this.timeline_zoom(at, delta, cx));
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
        ladder::gutter(),
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
        paint_lanes(bounds, scene, window);
        for clip in scene.clips.iter() {
            if clip.end < start || clip.start > end {
                continue;
            }
            paint_clip(
                scene.clip_box(bounds, clip),
                clip,
                scene.selected.as_ref().is_some_and(|id| id == &clip.id),
                window,
                cx,
            );
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
                ladder::muted()
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
        ladder::muted(),
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(
                canvas.origin.x,
                canvas.origin.y + px(top + WAVEFORM_HEIGHT - 0.5),
            ),
            size: size(canvas.size.width, px(1.)),
        },
        WAVEFORM_FLOOR,
    ));

    let Some(waveform) = &scene.waveform else {
        return;
    };
    let duration = waveform.duration_seconds;
    let centre = top + WAVEFORM_HEIGHT / 2.;
    let half = (WAVEFORM_HEIGHT - 8.) / 2.;
    let view = scene.view;

    let mut bar = |column: &Column, top: f32, height: f32, color: Rgba| {
        if height <= 0. || column.x < -1. || column.x > width + 1. {
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

    let Some(stored) = stored_grid(waveform, duration) else {
        return;
    };

    // A measured window is the only source with a bucket per pixel, and it is
    // asked for only where the stored envelope has stopped having one. Until
    // one arrives — and while a pan is outrunning the one in hand — the stored
    // envelope is still a waveform, so the bed is never blank.
    if let Some(fine) = scene
        .fine
        .as_ref()
        .filter(|fine| fine.covers(start, end, view.zoom))
    {
        let grid = fine.grid();
        // Colour is the one thing a measured window does not carry: hue is
        // spectral content, which moves slowly, so it comes from the stored
        // per-bucket colours underneath at whatever resolution those have.
        let colors = waveform
            .colors
            .as_ref()
            .filter(|colors| colors.len() == stored.count * 3);
        for column in columns(grid, grid.range(start, end), view) {
            let peak = |values: &[f32], fold: fn(f32, f32) -> f32| {
                values[column.buckets.clone()]
                    .iter()
                    .fold(0., |extreme, value| fold(extreme, *value))
            };
            let color = match colors {
                Some(colors) => {
                    let bucket = stored.bucket_at(grid.time_of(column.buckets.start));
                    Rgba {
                        r: f32::from(colors[bucket * 3]) / 255.,
                        g: f32::from(colors[bucket * 3 + 1]) / 255.,
                        b: f32::from(colors[bucket * 3 + 2]) / 255.,
                        a: 1.,
                    }
                }
                None => ladder::accent(),
            };
            // Peaks as a dimmed hull, RMS as the solid core inside it: at a
            // bucket per pixel the peaks alone are a spiky outline that says
            // where the audio reached but not where its energy is.
            let (low, high) = (peak(&fine.min, f32::min), peak(&fine.max, f32::max));
            let top = (centre - high * half).floor();
            let bottom = (centre - low * half).floor();
            bar(
                &column,
                top,
                (bottom - top).max(1.),
                Rgba { a: 0.45, ..color },
            );
            let body = (peak(&fine.rms, f32::max) * half).floor();
            bar(&column, centre - body, body * 2., color);
        }
        return;
    }

    if let Some(bands) = &waveform.bands {
        for column in columns(stored, stored.range(start, end), view) {
            // `floor` is monotone, so the tallest bar's floored height is the
            // floored peak — this is the same number the per-bucket walk drew,
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
        return;
    }

    let Some(samples) = &waveform.full_samples else {
        return;
    };
    // The per-bucket colors are the legacy path; without them the whole
    // envelope is one hue, as `--chart-4` is on the web.
    let colors = waveform
        .colors
        .as_ref()
        .filter(|colors| colors.len() == stored.count * 3);
    for column in columns(stored, stored.range(start, end), view) {
        // The min/max envelope of the run, which is the union the overlapping
        // per-bucket rects covered. The *colour* is the one thing that cannot
        // fold, so the column takes the loudest bucket's — the bucket that
        // reached furthest, and so the one that coloured most of the column.
        let (mut min, mut max) = (0., 0.);
        let mut loudest = (column.buckets.start, f32::MIN);
        for index in column.buckets.clone() {
            let (low, high) = (samples[index * 2], samples[index * 2 + 1]);
            min = f32::min(min, low);
            max = f32::max(max, high);
            if high - low > loudest.1 {
                loudest = (index, high - low);
            }
        }
        let y_top = centre - max * half;
        let y_bottom = centre - min * half;
        let color = match colors {
            Some(colors) => {
                let channel = loudest.0 * 3;
                Rgba {
                    r: f32::from(colors[channel]) / 255.,
                    g: f32::from(colors[channel + 1]) / 255.,
                    b: f32::from(colors[channel + 2]) / 255.,
                    a: 1.,
                }
            }
            None => ladder::accent(),
        };
        bar(&column, y_top, (y_bottom - y_top).max(1.), color);
    }
}

/// One drawn column of the waveform, and the buckets that land in it.
struct Column {
    /// The column's left edge, from [`View::x_of`] and so a whole pixel.
    x: f32,
    /// The drawn width, `barWidth` in `drawWaveform`.
    span: f32,
    buckets: std::ops::Range<usize>,
}

/// A run of buckets laid on the timeline: `count` of them, evenly spaced, the
/// first starting `origin` seconds in.
///
/// The stored envelope's grid starts at zero and spans the track; a measured
/// window's starts wherever it was cut and spans only itself. Everything that
/// walks buckets — which range is visible, where one lands, how many share a
/// column — is the same arithmetic over these three numbers either way.
#[derive(Clone, Copy)]
struct Grid {
    origin: f64,
    per_second: f64,
    count: usize,
}

impl Grid {
    fn time_of(self, bucket: usize) -> f64 {
        self.origin + bucket as f64 / self.per_second
    }

    /// The bucket covering `time`, clamped to the grid — a time outside it
    /// takes the nearest end rather than nothing.
    fn bucket_at(self, time: f64) -> usize {
        let index = ((time - self.origin) * self.per_second).floor();
        if !index.is_finite() || index < 0. {
            return 0;
        }
        (index as usize).min(self.count.saturating_sub(1))
    }

    /// The half-open bucket range a visible time range covers.
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
/// whole track, however long it is. `None` for a waveform with neither band
/// envelopes nor min/max pairs, which is a waveform with nothing to draw.
fn stored_grid(waveform: &TrackWaveform, duration: f64) -> Option<Grid> {
    let count = match &waveform.bands {
        Some(bands) => bands.low.len(),
        None => waveform.full_samples.as_ref()?.len() / 2,
    };
    if count == 0 || !duration.is_finite() || duration <= 0. {
        return None;
    }
    Some(Grid {
        origin: 0.,
        per_second: count as f64 / duration,
        count,
    })
}

/// Group the visible buckets into the columns they are drawn in.
///
/// Below one pixel per bucket — which is most of the zoom range for the stored
/// envelope, since it is `FULL_WAVEFORM_SIZE` buckets however long the track is
/// — a run of buckets shares one floored `x` and each would paint the *same*
/// one-pixel-wide rect. Only the run's envelope can show, so a column is the
/// unit worth drawing, and the quad count is what a frame costs here: gpui
/// charges a `BoundsTree` insert per quad, and at full zoom-out those inserts
/// dominate this canvas's paint.
///
/// Above one pixel per bucket every bucket gets its own column, and folding is
/// the identity — which is the whole of a measured window, cut at exactly a
/// bucket per pixel.
fn columns(
    grid: Grid,
    buckets: std::ops::Range<usize>,
    view: View,
) -> impl Iterator<Item = Column> {
    let (from, to) = (buckets.start, buckets.end);
    let span = (view.zoom / grid.per_second as f32).max(1.).ceil();
    let x_of = move |bucket: usize| view.x_of(grid.time_of(bucket));
    let mut next = from;
    std::iter::from_fn(move || {
        let start = next;
        if start >= to {
            return None;
        }
        let x = x_of(start);
        next += 1;
        while next < to && x_of(next) == x {
            next += 1;
        }
        Some(Column {
            x,
            span,
            buckets: start..next,
        })
    })
}

/// The line under the waveform. Literal on the web side too — it is darker
/// than anything on the ladder, because it is the seam between two planes
/// rather than a plane of its own.
const WAVEFORM_FLOOR: Rgba = Rgba {
    r: 0x17 as f32 / 255.,
    g: 0x17 as f32 / 255.,
    b: 0x17 as f32 / 255.,
    a: 1.,
};

/// `drawAnnotations`' ground: the empty insertion lane, the alternating lane
/// fills, and the darkened floor below the last one.
fn paint_lanes(canvas: Bounds<Pixels>, scene: &Scene, window: &mut Window) {
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

    strip(
        TRACK_AREA_Y,
        TRACK_START_Y - TRACK_AREA_Y,
        fade(rgb(0x000000), 0.3),
        window,
    );
    let lanes = scene
        .clips
        .iter()
        .map(|clip| clip.row + 1)
        .max()
        .unwrap_or(1)
        .max(1);
    for lane in 0..lanes {
        let top = TRACK_START_Y + lane as f32 * LANE_HEIGHT;
        let alpha = if lane % 2 == 0 { 0.2 } else { 0.15 };
        strip(top, LANE_HEIGHT, fade(ladder::muted(), alpha), window);
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    canvas.origin.x,
                    canvas.origin.y + px(top + LANE_HEIGHT - 0.5),
                ),
                size: size(width, px(1.)),
            },
            ladder::border(),
        ));
    }
    let floor = TRACK_START_Y + lanes as f32 * LANE_HEIGHT;
    strip(
        floor,
        f32::from(canvas.size.height) - floor,
        fade(rgb(0x000000), 0.3),
        window,
    );
}

/// One clip: an opaque header plate over a translucent body, inside a border,
/// with grab handles at both ends once it is selected.
fn paint_clip(
    box_: Bounds<Pixels>,
    clip: &Clip,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let body_alpha = if selected { 1. } else { 0.75 };
    let header = Bounds {
        origin: box_.origin,
        size: size(box_.size.width, px(CLIP_HEADER)),
    };
    window.paint_quad(fill(header, clip.color));
    window.paint_quad(fill(
        Bounds {
            origin: point(box_.origin.x, box_.origin.y + px(CLIP_HEADER)),
            size: size(box_.size.width, box_.size.height - px(CLIP_HEADER)),
        },
        fade(clip.color, body_alpha),
    ));
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

/// The drawn width of a selected clip's grab mark. Narrower than [`HANDLE`],
/// which is what the pointer gets — the web draws 6px and grabs 8px.
const HANDLE_MARK: f32 = 6.;

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
