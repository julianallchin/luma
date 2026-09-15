# Track editor — interaction contract

**Status:** built in `gpui/crates/app/src/track_editor.rs` and
`gpui/crates/app/src/track_editor/`. This contract states the exact numbers and
rules the timeline follows. Section numbers are cited from tests. Keep them
stable. §8 lists where the native editor differs.

---

## 0. Coordinate system and geometry

```
MIN_ZOOM = 25            MAX_ZOOM = 500          (pixels per second)
ZOOM_SENSITIVITY = 0.002
MIN_ZOOM_Y = 0.5         MAX_ZOOM_Y = 1.5        ZOOM_Y_SENSITIVITY = 0.003
HEADER_HEIGHT = 32       WAVEFORM_HEIGHT = 80    TRACK_HEIGHT = 80
ANNOTATION_LANE_HEIGHT = 80   MINIMAP_HEIGHT = 48
MIN_ANNOTATION_DURATION = 0.05  (seconds)
ANNOTATION_HEADER_H = 18
```

Layout:

- `trackHeight = round(TRACK_HEIGHT * zoomY)`; `waveformHeight` and `headerHeight`
  never scale with `zoomY` — the waveform is a fixed navigation surface.
- `trackAreaY = headerHeight + waveformHeight` = 112 at zoomY 1.
- `trackStartY = trackAreaY + trackHeight` — row 0 (between `trackAreaY` and
  `trackStartY`) is the **empty insertion lane** above the topmost layer. Occupied
  rows are 1..N. There is deliberately no empty row below the lowest layer.
- `computeBottomAnchoredLayout(zoomY, layerCount, viewportHeight)`:
  `rowCount = max(1, layerCount + 1)`,
  `naturalHeight = trackStartY + rowCount * trackHeight`,
  `totalHeight = max(viewportHeight, naturalHeight)`, and `trackStartY` is pushed
  down by `totalHeight - naturalHeight` — i.e. **lanes are bottom-anchored**: z=0
  is pinned to the bottom of the viewport and new layers grow upward.

Two Y frames are in play:

- **world Y** includes the vertical scroll. Lane hit tests use it.
- **screen Y** excludes it. The ruler hit test uses it, because the ruler does
  not scroll.

Row from a world Y: `laneIdx = floor((y - trackStartY) / trackHeight)`.
Time from an X: `time = (x + scrollX) / zoom`. Both axes scroll.

---

## 1. Snapping — the exact quantization

Two snap functions, with **different capture thresholds**, both zoom-progressive.

Beat-relative snap: find `index` = last beat with
`beats[index+1] <= time`; `prevBeat = beats[index]`, `nextBeat = beats[index+1]`
(if undefined → return `prevBeat`). `beatLength = nextBeat - prevBeat`, falling back
to the average beat duration if non-positive; average beat duration is
`(beats[last] - beats[0]) / (beats.length - 1)`, defaulting to `0.5s` when there are
fewer than 2 beats. Then
`offset = (time - prevBeat) / beatLength`, `k = clamp(round(offset * D), 0, D)`,
`snapped = clamp(prevBeat + (k/D) * beatLength, prevBeat, nextBeat)`, where D is:

```
zoom >= 200  -> D = 4
zoom >= 100  -> D = 2
otherwise    -> D = 4
```

There are only two distinct snap behaviours.

Capture threshold — snap only applies if the snapped point is within N screen pixels:

- the snap used for the **selection cursor**, **cursor-drag range**, and
  **right-click insertion**: `abs(snapped - time) * zoom < 15`.
- the snap used for clip move/resize:
  `abs(snapped - time) * zoom < 12`.

If `beatGrid` is absent or has no beats, snapping is the identity.

**There is no modifier that bypasses snapping.** Alt during a move is duplicate-drag,
not snap-off. Scrubbing the playhead never snaps.

---

## 2. Playhead scrub

Trigger: left mousedown with **screen** Y `< headerHeight`
(the 32px ruler strip only — *not* the waveform).

- `time = clamp(x / zoom, 0, durationSeconds)`. The playhead moves at once.
- The scrub re-anchors the playhead clock and throttles the host seek to one
  per **32 ms**. The latest pending value is kept.
- Move: every pointer move re-scrubs with the same clamp.
- Release commits the position and sends any pending seek.
- **No drag threshold** — a bare click in the header seeks immediately on mousedown.
- **No snapping.**
- **While playing**: playback is not paused. The playhead clock continues from
  the scrubbed position, so audio follows the scrub.

Playhead clock while playing: each display frame extrapolates
`position + elapsed` from the last host reading. It re-anchors only on a large
disagreement (seek, loop wrap, resume). Small reading errors are ignored,
because they show as pixel jitter at high zoom.

---

## 3. Clip (annotation) interactions

### 3.1 Hit test

Mousedown: world Y must be in `[trackStartY, totalHeight)`.
Within `laneIdx`, a clip is hit only when

```
laneIdx === rowMap[ann.id]
clickTime in [ann.startTime, ann.endTime]
y < trackStartY + laneIdx*trackHeight + 1 + ANNOTATION_HEADER_H     // top 18px only
```

i.e. **only the clip's 18px header bar is grabbable**; the body (heatmap) is inert and
a press there behaves as an empty-lane press (§3.6). `find` returns the first match in
array order when clips overlap in time within a lane.

### 3.2 Selection semantics

| state | modifier | result |
| --- | --- | --- |
| clip not selected | none | selection becomes exactly `[id]` |
| clip not selected | shift | append to selection |
| clip already selected | none | selection unchanged (keeps the multi-selection so a group drag works) |
| clip already selected | shift | remove from selection |

Always, regardless of branch, the selection cursor is set to the clicked clip's
extent: `{ trackRow: laneIdx, trackRowEnd: null, startTime: ann.startTime,
endTime: ann.endTime }`.

If `readOnly`, the handler returns here — selection works, dragging does not.

### 3.3 Drag type and handles

`handleSize = 8` **world pixels**, compared against the clip's pixel extent:

```
x - startTime*zoom < 8   -> resize-left
endTime*zoom - x < 8     -> resize-right
otherwise                -> move
```

(For a clip narrower than 16px both tests can pass; left wins.)

Which clips move: if the pressed clip was already selected, **all** currently selected
clips; otherwise only the pressed one. Initial
`{startTime, endTime, zIndex, row}` of each is captured.

Before any movement, an undo snapshot is taken and compositing pauses until
release.

**Alt+drag on a move** mints copies at the current positions, then the *originals* are dragged away — so the
copy stays put and the dragged one is the original.

### 3.4 Move

- Horizontal: `deltaTime = (ev.clientX - startX) / zoom`;
  `newStart = max(0, snapToGrid(clickedInitial.startTime + deltaTime))` with the
  **12px** threshold; `snappedDelta = newStart - clickedInitial.startTime` is applied
  to *every* dragged clip (each also clamped at `>= 0`), preserving durations. Snap is
  computed from the **pressed** clip only — the group keeps its relative spacing.
- Vertical: `requestedRowDelta = round(dy / trackHeight)`, then
  `rowDelta = min(requestedRowDelta, bottomRow - lowestSelectedRow)` where
  `bottomRow = numberOfDistinctZ`. Downward motion is
  clamped at the floor; **upward motion is unclamped** — dragging above row 1 mints new
  z values above the current top.
- Vertical movement is **visual only** during the drag; the z-index change is applied on
  release.
- There is **no axis lock** and **no modifier for one**: horizontal and vertical apply
  simultaneously and independently.
- Each move changes local state only. Nothing is written to the backend.
- The selection cursor is re-derived from live positions each move, offset by `rowDelta`.

Release:
`newRow = initial.row + rowDelta - 1` (initial rows are 1-based), mapped back to a
z-index by `rowToZ`:

```
row < 0                  -> zRowsDesc[0] + (-row)          // above the top: new z above highest
row < zRowsDesc.length   -> zRowsDesc[row]                 // an existing layer
otherwise                -> lowestZ - (row - maxRow)       // below the bottom
```

Release then re-derives the cursor and saves the moved clips. Compositing
resumes first.

### 3.5 Resize

Left: `newStart = snapToGrid(pressedClip.startTime + deltaTime)` (12px threshold);
proceed only if `newStart < pressedClip.endTime - 0.1`. `startDelta` is applied to every
selected clip, each with `max(0, …)` and a per-clip guard
`newAnnStart < initial.endTime - 0.1`.

Right: `newEnd = snapToGrid(pressedClip.endTime + deltaTime)`; proceed only if
`newEnd > pressedClip.startTime + 0.1`. `endDelta` is applied to every selected clip,
each clamped `min(durationSeconds, …)` with guard `newAnnEnd > initial.startTime + 0.1`.

So the **minimum clip length during a resize is 0.1 s**, not `MIN_ANNOTATION_DURATION`
(0.05 s, which governs splits/paste/insertion instead). Resize is multi-clip: dragging
one selected clip's edge moves the same edge of every selected clip by the same delta.
Release path is shared with move (persist + cursor re-derive).

### 3.6 Empty-lane press → range selection

A press inside the lane area that hits no clip header:

1. `snappedTime = snapToGrid(clickTime)` (15px threshold), cursor set to
   `{trackRow: laneIdx, trackRowEnd: null, startTime: snappedTime, endTime: null}`
   (a point cursor), selection cleared.
2. A window-level drag builds a **rectangular time × row** range:
   `endTime = snapToGrid(moveTime)`,
   `currentRow = clamp(floor((moveY - trackStartY)/trackHeight), 0, rowCount-1)`,
   `trackRowEnd = currentRow !== startRow ? currentRow : null`.
3. Selection becomes every clip **fully contained** in the rectangle:
   `annRow in [minRow, maxRow] && ann.startTime >= rangeStart - 0.001 &&
   ann.endTime <= rangeEnd + 0.001` (1 ms epsilon). Partial overlaps are *not* selected.

Right-to-left drags are allowed; `startTime`/`endTime` are stored unnormalized and every
consumer does its own `min`/`max`.

### 3.7 Press outside the lane area

World Y between `headerHeight` and `trackStartY` (i.e. **over the waveform** or the row-0
insertion lane), or below `totalHeight`: `selectAnnotation(null)` and
`setSelectionCursor(null)`. **Clicking the waveform clears the selection; it does not
scrub.**

### 3.8 Double-click

Ignored while a drag is active. Hit test is the **whole lane row** (no header-band
restriction) plus `clickTime` inside the clip. On hit, the clip's graph opens in
a graph tab. No hit → nothing.

### 3.9 Right-click → pattern insert

Right-click does nothing when `readOnly`. Otherwise it computes the insertion
target:

- `startTime = snapToGrid(x / zoom)` (15px threshold).
- `endTime = startTime + oneBarLength`, where one bar is the **mean** downbeat interval, falling back to `avgBeat * (beatsPerBar || 4)`; then
  overridden by the first downbeat strictly after `startTime` if one exists.
- clamp `startTime >= 0`, `endTime <= durationSeconds`; abort if the span is
  `< MIN_ANNOTATION_DURATION` (0.05).
- Vertical: `floatRow = max(0, y - trackStartY) / trackHeight`,
  `visualRow = floor(floatRow)`, `nearestBoundary = round(floatRow)`. If
  `abs(floatRow - nearestBoundary) < 0.25` and the boundary is in `[1, totalTracks]`,
  it is **insert mode** (a new layer between two existing ones, shifting z upward);
  otherwise **add mode** onto the row under the pointer. Row 0 adds above the top layer.
- Insert mode target z: `zBoundary == 0` → `highestZ + 1` (no shift);
  `zBoundary >= totalTracks` → `lowestZ - 1`; else `zRowsDesc[zBoundary-1]` with a shift.

A ghost preview is painted at that position while the picker is open. The
picker commits on select: insert mode first bumps every `zIndex >= targetZ` by
1, then creates the clip.

Menu keys: `ArrowDown`/`ArrowUp` move the active row
(clamped), `Enter` commits, `Escape` closes. Hovering a row also makes it active and
recolors the ghost. A press outside the picker dismisses it.

---

## 4. Zoom and scroll

| gesture | effect |
| --- | --- |
| bare wheel / two-finger scroll | scroll, both axes |
| platform key + wheel | horizontal zoom at `ZOOM_SENSITIVITY` (0.002) per pixel |
| control + wheel (trackpad pinch) | horizontal zoom at 0.01 per pixel |
| alt + wheel | vertical zoom at `ZOOM_Y_SENSITIVITY` (0.003) per pixel |

Clamps: horizontal `[MIN_ZOOM, MAX_ZOOM] = [25, 500]`; vertical `[0.5, 1.5]`.

**Anchor is always the pointer, never the playhead.**

- Horizontal: the time under the pointer stays under the pointer:
  `scrollX = time * newZoom - pixel`.
- Vertical: anchored on `rowsFromBottom = (scrollHeight - (scrollTop + pixel)) /
  trackHeight` at the pointer's Y, then restored as
  `scrollTop = clamp(scrollHeight - rowsFromBottom*trackHeight - pixel, 0, maxScrollTop)`.
  Alt-wheel above `trackAreaY` is **ignored**.
- With no anchor (window resize, layer-count change, `H`), `scrollTop = maxScrollTop`
  — i.e. **pinned to the bottom**, keeping z=0 on the floor.

No rubber-banding: horizontal scroll is clamped to
`[0, contentWidth - canvasWidth]`.

### Auto-scroll ("follow playhead")

Follow playhead is toggled with `F`. While on, every frame sets
`scrollX = max(0, playhead*zoom - width/2)` when it differs by more than 0.5px.
It **centres** the playhead. It does not page at the edge.

### Minimap (strip above the timeline)

Pointer, all in minimap-local pixels,
`timeToPixel = width / durationMs`, `handleSize = 8`:

- within 8px of the lens's left edge → `resize-left`; right edge → `resize-right`;
  inside the lens → `move`; **outside the lens** → jump: the view centres the clicked
  time immediately, *and* a `move` drag starts from there.
- `move`: `scrollX = (initialStartTime + dx * (durationMs/minimapWidth)) / 1000 * zoom`.
- `resize-right`: `newLensW = max(10, startLensW + dx)`, `newZoom = containerWidth /
  visibleDurationSeconds`, clamped **`[5, 500]`** — note the low clamp is `5`, not
  `MIN_ZOOM` (25); left keeps the left edge fixed and
  right keeps the start time fixed.
- Hover sets the cursor: `ew-resize` on either handle, `grab`
  inside the lens, `pointer` outside.

Minimap paint: outside the lens is dimmed with
`background @ 0.55`, inside is lifted with `foreground @ 0.08`, a `chart-3 @ 0.85`
1px lens border with two 3px `chart-3 @ 0.9` handle bars, a yellow loop band, and a 1px
`chart-3` playhead.

---

## 5. Keyboard — the complete map

The bindings apply in `TrackEditor && !TextInput`. A text field that has focus
takes the keys.

| keys | action |
| --- | --- |
| `Space` | play / pause; no-op when no track is open |
| `Cmd/Ctrl+Z` | undo (per-track undo stack) |
| `Cmd/Ctrl+Shift+Z` | redo |
| `Cmd/Ctrl+E` | split every clip straddling the cursor time, in the cursor's row band |
| `Delete` / `Backspace` | if the cursor has a range → `deleteInRegion()`; else delete the selected clips |
| `Alt+ArrowUp` / `Alt+ArrowDown` | move selected clips one lane up / down |
| `Cmd/Ctrl+C` | copy (region or object mode, §6) |
| `Cmd/Ctrl+X` | cut |
| `Cmd/Ctrl+V` | paste at the cursor |
| `Cmd/Ctrl+D` | duplicate after the cursor |
| `Cmd/Ctrl+L` | set loop from the cursor range; clear if the range equals the current loop (1 ms tolerance) or there is no valid range |
| `F` | toggle follow-playhead |
| `H` | auto-fit vertical zoom: `zoomY = clamp((clientHeight - 32 - 80) / ((layers+1) * 80), 0.5, 1.5)`, then bottom-anchored re-layout |
| `ArrowUp`/`ArrowDown`/`Enter`/`Escape` | only inside the pattern search menu |

There are no bindings for arrow-key nudge, Escape-to-deselect, zoom keys,
transport scrub keys or save. Every chord binds under both the platform key and
control, so both work on every platform.

---

## 6. Command semantics behind the bindings

Every mutating command records an undo step and does nothing when `readOnly`.

**Row ↔ z mapping** used everywhere: distinct z values sorted **descending** are rows
`0..N-1`; the cursor's `trackRow` is 1-based (row 0 is the empty top lane), so
`zIdx = trackRow - 1`.

- **`getRegionInfo`** — returns `null` unless the cursor has an `endTime`.
  Otherwise `[min,max]` of the cursor times × the z set covered by
  `[min(trackRow, trackRowEnd), max(...)]`.
- **`splitAtCursor`** — splits at `selectionCursor.startTime` every clip
  in the affected z set with `startTime < t < endTime`; skips a split where either half
  would be `< MIN_ANNOTATION_DURATION` (0.05 s). The new right halves become the
  selection.
- **`deleteInRegion`** — `resolveOverlaps(annotations, rangeStart,
  rangeEnd, affectedZ, ∅)` then apply; clears both selection and cursor. Partially
  overlapping clips are **clipped**, not deleted whole.
- **`moveAnnotationsVertical`** — up: each selected clip takes the z of the
  row above, or `highestZ + 1` if already at the top. Down: **all-or-nothing** — if any
  selected clip is already at the bottom row the whole command is a no-op, so a multi-lane
  selection cannot collapse against the z=0 floor.
- **`copySelection`** — two modes.
  *Region mode* (cursor has a range): every clip overlapping the range × row band is
  **clipped** to the range and stored with `offsetFromStart` relative to `regionStart`;
  clips shorter than 0.05 s after clipping are dropped; `totalDuration = regionEnd -
  regionStart`. *Object mode* (point cursor): the explicitly selected clips are copied
  whole, offsets relative to the cursor start, `totalDuration` running to the last
  selected clip's end. Requires a cursor — with no cursor, copy is a no-op.
- **`cutSelection`** — copy, then delete: region mode uses the same
  `resolveOverlaps` as `deleteInRegion`; object mode removes the selected ids whole.
- **`paste`** — **top-left anchored**: the highest-z clipboard item lands on
  the cursor's row (`targetRow = max(0, trackRow - 1)`), other items keep their relative
  row offsets, mapped back to z through the combined z list, extending below the floor if
  needed. Paste start is `min(cursor.startTime, cursor.endTime)`; the destination region is
  cleared with `resolveOverlaps` first; items whose end would exceed `durationSeconds` are
  dropped. Afterwards the cursor spans `[pasteStart, pasteStart + totalDuration]` and the
  pasted clips are selected.
- **`duplicate`** — copy, move the cursor to the selection's **end**
  (point cursor) with `trackRow` re-derived from the topmost *selected clip's* z (not the
  stale drag-origin row), then paste. Net effect: a copy immediately after the original.
- **clone in place** — local-only copies, used by Alt+drag.
- **loop region set / clear** — updates the editor and the host loop region.
- **`play`** — **seeks to `playheadPosition` first**, then plays: that is what
  makes Play resume from a scrub made while stopped.
- **playback sync** — adopts the playing state and position from the host
  reading while a track is loaded.

---

## 7. Visual states

Clips:

- box: `x = floor(startTime*zoom - scrollLeft)`,
  `w = max(4, floor((end-start)*zoom))`, `y = trackY + 1`, `h = trackHeight - 2`.
- header strip `ANNOTATION_HEADER_H = 18` painted at **alpha 1** in the pattern color;
  body painted at `alpha = selected ? 1 : 0.75` (either the heatmap bitmap, nearest-neighbor
  and only when `w >= 8`, or the flat color).
- border `foreground @ 0.35`, 1px, plus a 1px header/body divider.
- **selected**: border redrawn `foreground @ 0.9` at 1.5px; two 6px-wide
  `foreground @ 0.9` grab plates at both ends *of the header only*; three 1px grip dots
  per plate, spaced 4px, centered on the header.
- label drawn only when `w > 30`, clipped to `x+8 .. x+w-8`, 10px system font at
  `alpha = selected ? 0.95 : 0.8`, black or white by sRGB luminance of the pattern color.
- lanes: alternating `muted @ 0.2` / `muted @ 0.15` stripes with a 1px `border` rule at
  each lane bottom; the empty row-0 lane and everything below the last lane are
  `rgba(0,0,0,0.3)`.
- dragged clips are painted at `row + rowDelta` — the only preview of a
  pending lane change.

Selection cursor: point cursor = 2px `accent` vertical line spanning
`[minRow, maxRow]` lanes; range cursor = `accent @ 0.15` fill plus a 2px `accent`
rectangle over the same row band.

Loop region: `rgba(234,179,8,0.12)` fill from `headerHeight` down over
everything, with 1px `rgba(234,179,8,0.7)` boundary lines.

Insertion feedback: add mode highlights the target lane
`accent @ 0.1` with an `accent @ 0.4` outline; insert mode draws a 2px `accent` line at
the boundary with a small left-edge arrow.

**Hover** is expressed entirely through the cursor, not through fills, and only inside a clip's header band:

- within 8px of the left edge → a custom bracket-left SVG cursor, right edge → `CURSOR_BRACKET_R`;
- elsewhere in the header → `grab`; anywhere else → `default`.
- during a drag: `grabbing` for a move, the bracket cursors for a resize.
- minimap: `ew-resize` / `grab` / `pointer` as in §4.

There is **no hover highlight on rows or clips** — a hovered clip looks identical to an
unhovered one.

---

## 8. Status in the native editor

Built as stated above:

- Only the 32px ruler scrubs. A press on the waveform clears the selection.
- Scrub seeks are throttled to one per 32 ms.
- Only a clip's header bar is grabbable.
- Selection is a list. Shift-click, marquee range selection, group move,
  group resize and Alt-drag duplicate work. Snapping uses the pressed clip.
- Alt+wheel zooms lanes. The platform key + wheel zooms time. Control + wheel
  is the pinch rate. Scroll is clamped to the content.
- Keys: Space, `F`, `H`, Undo and Redo, Split, Copy, Cut, Paste, Duplicate,
  Loop, Delete/Backspace and Alt+Arrow.
- Right-click opens the pattern insert picker. Double-click opens the clip's
  graph.
- Clip bodies show heatmap previews. The minimap has move, resize and jump
  gestures.

Differences:

- Wheel zoom eases through a spring in log scale (`zoom_motion.rs`).
- The playhead clock re-anchors when it disagrees with the host by more than
  0.1 s (`playback_clock.rs`).
- The minimap is 40px high with 6px handles.
- The time readout is `M:SS`, not bar and beat.

Not built: bracket hover cursors on clip edges, playback-rate buttons and a
resizable timeline panel.
