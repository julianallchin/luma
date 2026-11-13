//! The pointer contract: press, drag, release and pan on the canvas, the
//! gestures they start, and the scrub that moves the transport.

use super::*;

/// How often a scrub is allowed to move the transport. `SEEK_THROTTLE_MS`.
const SEEK_THROTTLE: Duration = Duration::from_millis(32);

/// The shortest a clip may be left by a resize.
///
/// Not `MIN_ANNOTATION_DURATION` (0.05 s): a resize is held to 0.1 s, and the
/// smaller floor is for splits, pastes and insertions.
const MIN_RESIZE: f64 = 0.1;

/// The epsilon a marquee's containment test allows, so a clip whose edge was
/// snapped to the same beat as the sweep's still counts as inside.
const CONTAINED_EPSILON: f64 = 0.001;

/// What the pointer is doing between a press and a release.
pub(super) enum Gesture {
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
pub(super) enum Drag {
    Move,
    Resize(Edge),
}

/// Where one dragged clip was when the pointer took hold. Every move is
/// computed from the press rather than from the last frame, so a drag out and
/// back lands exactly where it started.
pub(super) struct Initial {
    pub(super) id: SharedString,
    pub(super) start: f64,
    pub(super) end: f64,
    /// The lane it was in, 1-based as [`lanes`] resolves them — which is what
    /// [`row_to_z`] expects.
    row: usize,
    /// Its fades when it has any, so a resize can keep their lengths.
    fades: Option<fades::Fades>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Edge {
    Start,
    End,
}

impl Edge {
    pub(super) fn suffix(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
}

/// Register this frame's pointer handlers.
///
/// Press and scroll are scoped to the canvas's hitbox; move and release are
/// not. A drag that wanders off the canvas must keep tracking, and must end
/// when the button comes up wherever that happens — see the same note in
/// `graph.rs`, which this mirrors.
pub(super) fn listen(app: &Entity<Luma>, hitbox: &Hitbox, window: &mut Window) {
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
        // opens a clip's menu or offers an insertion, the middle button (or the left with the
        // platform key and Alt) pans, a second left click opens the clip's
        // pattern, and a first left click is the pointer contract in
        // `timeline_press`.
        match (event.button, event.click_count) {
            (MouseButton::Right, _) => {
                pressed.update(cx, |this, cx| {
                    if !this.open_clip_menu(at, cx) {
                        this.timeline_insert_menu(at, cx);
                    }
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

    // macOS hands a trackpad pinch over as its own event rather than as the
    // ctrl-wheel other platforms send, so without this the timeline only ever
    // heard the pinch on Linux and Windows. `delta` is the magnification:
    // fingers apart is positive and means zoom in, which is already the sign
    // `timeline_zoom` wants; taking the log turns the `1 + delta` factor into
    // the additive step the wheel uses.
    let pinched = app.clone();
    let under = hitbox.clone();
    window.on_mouse_event(move |event: &PinchEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !under.should_handle_scroll(window) {
            return;
        }
        let (at, amount) = (event.position, event.delta.ln_1p());
        pinched.update(cx, |this, cx| this.timeline_zoom(at, amount, cx));
    });
}

impl Editor {
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

    /// The clip under a window position, anywhere in its lane row: body as
    /// well as header bar. What a gesture that is not a drag hits.
    pub(super) fn clip_under(&self, at: Point<Pixels>) -> Option<&Clip> {
        let canvas = self.canvas.get();
        let time = self.view.time_at(f32::from(at.x - canvas.origin.x));
        let row = self.layout().row_at(f32::from(at.y - canvas.origin.y))?;
        self.clips
            .iter()
            .find(|clip| clip.row == row && time >= clip.start && time < clip.end)
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

    /// Let the clips a drag held cut away what they cover in their layers —
    /// see [`lanes::settle`] — except the `kept` ones.
    fn settle(&mut self, held: &[Initial], kept: &[SharedString]) {
        let placed: Vec<SharedString> = held.iter().map(|clip| clip.id.clone()).collect();
        if let Some(clips) = lanes::settle(&self.clips, &placed, kept) {
            self.replace_clips(clips);
        }
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
}

impl Luma {
    /// A double-click on a clip selects it, which brings up its inputs: in
    /// Program the inspector beside the timeline shows them already, and in
    /// Direct the clip's graph takes the timeline's place.
    ///
    /// The hit test is the whole lane row, not the header band: this is the
    /// one clip gesture that is not a drag, so there is nothing for the inert
    /// body to leave room for.
    fn timeline_open_pattern(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body() else {
            return;
        };
        let selected = state
            .pressed_clip
            .as_ref()
            .and_then(|(id, _)| state.clips.iter().find(|clip| clip.id == *id));
        let Some(clip) = selected.or_else(|| state.clip_under(at)) else {
            return;
        };
        let id = clip.id.clone();
        self.with_track_editor(cx, |editor| editor.selected = vec![id]);
        self.open_clip_graph(cx);
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
        self.refresh_clip_previews(touched, cx);
        if save {
            self.commit_clips(cx);
        }
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
}
