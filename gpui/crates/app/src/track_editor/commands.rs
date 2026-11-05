//! The editing commands — delete, split, lane moves, copy, cut, paste,
//! duplicate — as rewrites of the working copy, and the undo they share.

use super::*;

/// What a cut or a copy took, in its two shapes.
///
/// The clips are whole rows so a paste can mint real clips from them. They
/// keep the times they were copied at, and a paste moves them by the musical
/// distance from `origin` to the cursor — see [`shift_time`].
pub(super) struct Clipboard {
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
pub(super) struct Snapshot {
    clips: Rc<[Clip]>,
    selected: Vec<SharedString>,
    cursor: Option<Cursor>,
}

/// `MIN_ANNOTATION_DURATION`: the shortest a clip a *command* produces may be.
/// A split half, a paste remnant or a region-cleared tail below this is
/// dropped rather than kept — deliberately smaller than [`MIN_RESIZE`], which
/// is what a hand at the edge of a clip is held to.
pub(super) const MIN_CLIP: f64 = 0.05;

/// How close two loop bounds have to be to count as the same loop, which is
/// what turns the loop key into a toggle. A 1 ms tolerance.
const LOOP_EPSILON: f64 = 0.001;

/// Lighting priority for each visible row, from top to bottom.
pub(super) fn z_ladder(clips: &[Clip]) -> Vec<i64> {
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
pub(super) fn row_to_z(layers: &[i64], row: i32) -> i64 {
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
pub(super) fn clear_region(
    clips: &[Clip],
    span: (f64, f64),
    hit: impl Fn(&Clip) -> bool,
) -> Vec<Clip> {
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

impl Editor {
    /// Where the timeline is now, as something an undo could return to.
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            clips: Rc::clone(&self.clips),
            selected: self.selected.clone(),
            cursor: self.cursor,
        }
    }

    /// Mark the point an undo comes back to, before running an edit.
    pub(super) fn checkpoint(&mut self) {
        let now = self.snapshot();
        self.history.record(now);
    }

    /// Forget the last checkpoint when the edit it was taken for changed
    /// nothing.
    ///
    /// [`Self::replace_clips`] is the only thing that swaps the list and it
    /// always swaps in a fresh allocation, so pointer identity answers exactly
    /// the question "did anything run".
    pub(super) fn abandon_checkpoint(&mut self) {
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
    pub(super) fn sync_cursor(&mut self) {
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
    pub(super) fn clone_in_place(&mut self, ids: &[SharedString]) {
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
}

impl Luma {
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
    pub(super) fn track_command(
        &mut self,
        command: impl FnOnce(&mut Editor),
        cx: &mut Context<Self>,
    ) {
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

    /// Publish the working copy: one save of its changes, whatever the gesture
    /// or the command changed.
    ///
    /// **The only write this screen makes.** A gesture that moves five clips,
    /// splits three and deletes one is one write, so it cannot half land and
    /// the score never passes through a state the editor's own rules forbid.
    ///
    /// One write is in flight at a time, and anything the user did meanwhile
    /// is still on [`Editor::dirty`] and goes out on its return.
    pub(super) fn commit_clips(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        self.commit_graph_score_for(target, cx);
    }
}
