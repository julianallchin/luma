# Web track editor — complete interaction contract

Extracted from `src/features/track-editor/`. Every behavior below is stated so the
GPUI port can be built without reading the web source. Line references are
`path:line` at the time of extraction.

Source files that carry interaction:

| file | what it owns |
| --- | --- |
| `src/features/track-editor/components/timeline.tsx` | all canvas pointer handling, timeline keyboard map, minimap pointer handling |
| `src/features/track-editor/components/hooks/use-timeline-zoom.ts` | wheel + trackpad-gesture zoom |
| `src/features/track-editor/components/track-editor.tsx` | Space transport binding, panel resize drag |
| `src/features/track-editor/components/pattern-search-menu.tsx` | right-click insert menu keyboard |
| `src/features/track-editor/stores/use-track-editor-store.ts` | every command a key binding invokes |
| `src/features/track-editor/utils/timeline-constants.ts` | geometry + zoom limits |
| `src/features/track-editor/utils/timeline-drawing.ts` | selection / hover / loop visual states |
| `src/features/track-editor/components/timeline-minimap.tsx` | minimap paint (lens, handles, playhead) |

---

## 0. Coordinate system and geometry

`utils/timeline-constants.ts:2-16`:

```
MIN_ZOOM = 25            MAX_ZOOM = 500          (pixels per second)
ZOOM_SENSITIVITY = 0.002
MIN_ZOOM_Y = 0.5         MAX_ZOOM_Y = 1.5        ZOOM_Y_SENSITIVITY = 0.003
HEADER_HEIGHT = 32       WAVEFORM_HEIGHT = 80    TRACK_HEIGHT = 80
ANNOTATION_LANE_HEIGHT = 80   MINIMAP_HEIGHT = 48
MIN_ANNOTATION_DURATION = 0.05  (seconds)
ANNOTATION_HEADER_H = 18        (utils/timeline-drawing.ts:13)
```

Layout (`timeline-constants.ts:30-70`):

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

Two Y frames are in play (`timeline.tsx:1505-1518`):

- **world Y** = `clientY - rect.top + container.scrollTop` — used for lane hit tests.
- **screen Y** = `clientY - rect.top` — used for the header (ruler) hit test, because
  the canvas is `sticky top-0` and the header does not scroll.

Row from a world Y: `laneIdx = floor((y - trackStartY) / trackHeight)`
(`timeline.tsx:1538`). Time from an X: `x = clientX - rect.left + scrollLeft`,
`time = x / zoom` (`timeline.tsx:1506, 1520`).

The container is a real scroll container (`overflow-x-auto overflow-y-auto`,
`overscrollBehavior: none`, `timeline.tsx:2515-2520`) with a zero-height spacer sized
`durationSeconds * zoom` wide and `totalHeight` tall; the canvas is
`sticky left-0 top-0` with `marginTop: -totalHeight`. **Both axes scroll natively.**

---

## 1. Snapping — the exact quantization

Two snap functions, with **different capture thresholds**, both zoom-progressive.

Beat-relative snap (`timeline.tsx:479-593`): find `index` = last beat with
`beats[index+1] <= time`; `prevBeat = beats[index]`, `nextBeat = beats[index+1]`
(if undefined → return `prevBeat`). `beatLength = nextBeat - prevBeat`, falling back
to the average beat duration if non-positive; average beat duration is
`(beats[last] - beats[0]) / (beats.length - 1)`, defaulting to `0.5s` when there are
fewer than 2 beats (`timeline.tsx:458-462`). Then
`offset = (time - prevBeat) / beatLength`, `k = clamp(round(offset * D), 0, D)`,
`snapped = clamp(prevBeat + (k/D) * beatLength, prevBeat, nextBeat)`, where D is:

- `getQuarterBeatSnap` — D = 4 (`timeline.tsx:479`)
- `getEighthBeatSnap` — D = 2 (`timeline.tsx:508`)
- `getSixteenthBeatSnap` — D = 4 (`timeline.tsx:537`)
- `getTripletBeatSnap` — D = 3 (`timeline.tsx:566`) — **never called; dead code.**

Note the *names lie*: "quarter" and "sixteenth" are both D=4 and therefore identical,
"eighth" is D=2. Snap divisions are actually only two distinct behaviors. Port the
numbers, not the names.

Zoom selection (`timeline.tsx:1470-1495` outer, `1679-1694` inner):

```
zoom >= 200  -> sixteenth (D=4)
zoom >= 100  -> eighth    (D=2)
otherwise    -> quarter   (D=4)
```

Capture threshold — snap only applies if the snapped point is within N screen pixels:

- `snapToGrid` used for the **selection cursor**, **cursor-drag range**, and
  **right-click insertion**: `abs(snapped - time) * zoom < 15` (`timeline.tsx:1487`).
- the **drag-local** `snapToGrid` used for clip move/resize:
  `abs(snapped - time) * zoom < 12` (`timeline.tsx:1693`).

If `beatGrid` is absent or has no beats, snapping is the identity.

**There is no modifier that bypasses snapping.** Alt during a move is duplicate-drag,
not snap-off. Scrubbing the playhead never snaps.

---

## 2. Playhead scrub

Trigger (`timeline.tsx:1519-1532`): left mousedown with **screen** Y `< headerHeight`
(the 32px ruler strip only — *not* the waveform).

- `time = clamp(x / zoom, 0, durationSeconds)`, immediately `scrubPlayhead(time)`,
  `playheadDragRef = true`, and a one-shot window `mouseup` ends it.
- `scrubPlayhead` (`timeline.tsx:796-814`): writes the DOM playhead transform directly
  (no React state), re-anchors the extrapolation clock, marks the minimap dirty, and
  throttles the `host_seek` IPC to one per **32 ms** (`SEEK_THROTTLE_MS`), stashing the
  latest value in `pendingSeekRef`.
- Move (`timeline.tsx:2034-2046`): while `playheadDragRef`, every mousemove re-scrubs
  with the same clamp. This handler is on the canvas only, so a scrub that leaves the
  canvas horizontally stops tracking until it returns; the global `mouseup`
  (`timeline.tsx:2023-2032`) still ends it.
- Release: `flushScrub` (`timeline.tsx:817-826`) commits `setPlayheadPosition` and
  fires any pending throttled seek.
- **No drag threshold** — a bare click in the header seeks immediately on mousedown.
- **No snapping.**
- **While playing**: playback is not paused. The rAF loop keeps extrapolating from
  `lastSyncPlayheadRef` (which the scrub just rewrote), so audio follows the scrub
  and the picture never stalls.
- `mouseup` on the canvas (`timeline.tsx:2275-2306`) *also* seeks when the release is
  in the header strip and no drag was active — so a plain header click issues a seek
  on both press and release. Harmless (same value) but it is the observed behavior.

Playhead clock while playing (`timeline.tsx:381-426`, `1096-1210`): the rAF loop
extrapolates `position + elapsed * playbackRate` from the last snapshot, and only
re-anchors to a host snapshot when it disagrees by **> 0.25 s** (seek, loop wrap,
resume). Small snapshot errors are IPC noise and are deliberately ignored — feeding
them in shows as pixel jitter at high zoom.

---

## 3. Clip (annotation) interactions

### 3.1 Hit test

Mousedown (`timeline.tsx:1535-1549`): world Y must be in `[trackStartY, totalHeight)`.
Within `laneIdx`, a clip is hit only when

```
laneIdx === rowMap[ann.id]
clickTime in [ann.startTime, ann.endTime]
y < trackStartY + laneIdx*trackHeight + 1 + ANNOTATION_HEADER_H     // top 18px only
```

i.e. **only the clip's 18px header bar is grabbable**; the body (heatmap) is inert and
a press there behaves as an empty-lane press (§3.6). `find` returns the first match in
array order when clips overlap in time within a lane.

### 3.2 Selection semantics (`timeline.tsx:1551-1582`)

| state | modifier | result |
| --- | --- | --- |
| clip not selected | none | `selectAnnotation(id)` — selection becomes exactly `[id]` |
| clip not selected | shift | append to selection |
| clip already selected | none | selection unchanged (keeps the multi-selection so a group drag works) |
| clip already selected | shift | remove from selection |

Always, regardless of branch, the selection cursor is set to the clicked clip's
extent: `{ trackRow: laneIdx, trackRowEnd: null, startTime: ann.startTime,
endTime: ann.endTime }` (`timeline.tsx:1572-1580`), written to both store and ref.

If `readOnly`, the handler returns here — selection works, dragging does not
(`timeline.tsx:1585`).

### 3.3 Drag type and handles (`timeline.tsx:1587-1593`)

`handleSize = 8` **world pixels**, compared against the clip's pixel extent:

```
x - startTime*zoom < 8   -> resize-left
endTime*zoom - x < 8     -> resize-right
otherwise                -> move
```

(For a clip narrower than 16px both tests can pass; left wins.)

Which clips move: if the pressed clip was already selected, **all** currently selected
clips; otherwise only the pressed one (`timeline.tsx:1598-1602`). Initial
`{startTime, endTime, zIndex, row}` of each is captured.

Before any movement: `captureBeforeDrag()` snapshots for undo (`timeline.tsx:1631`),
and `setIsDraggingAnnotation(true)` suppresses the compositor until release
(`timeline.tsx:1640`).

**Alt+drag on a move** (`timeline.tsx:1634-1637`): `cloneAnnotationsInPlace(ids)` mints
UUID copies at the current positions, then the *originals* are dragged away — so the
copy stays put and the dragged one is the original.

### 3.4 Move (`timeline.tsx:1696-1741`)

- Horizontal: `deltaTime = (ev.clientX - startX) / zoom`;
  `newStart = max(0, snapToGrid(clickedInitial.startTime + deltaTime))` with the
  **12px** threshold; `snappedDelta = newStart - clickedInitial.startTime` is applied
  to *every* dragged clip (each also clamped at `>= 0`), preserving durations. Snap is
  computed from the **pressed** clip only — the group keeps its relative spacing.
- Vertical: `requestedRowDelta = round(dy / trackHeight)`, then
  `rowDelta = min(requestedRowDelta, bottomRow - lowestSelectedRow)` where
  `bottomRow = numberOfDistinctZ` (`timeline.tsx:1710-1719`). Downward motion is
  clamped at the floor; **upward motion is unclamped** — dragging above row 1 mints new
  z values above the current top.
- Vertical movement is **visual only** during the drag (`draggedIdsRef` + `rowDelta`
  offset the paint, `timeline-drawing.ts:341-345`); the z-index change is applied on
  release.
- There is **no axis lock** and **no modifier for one**: horizontal and vertical apply
  simultaneously and independently.
- `updateAnnotationsLocal(updates)` per move → local state only, no IPC.
- The selection cursor is re-derived from live positions each move
  (`syncCursorFromAnnotations`, `timeline.tsx:1643-1672`), offset by `rowDelta`.

Release (`timeline.tsx:1805-1846`):
`newRow = initial.row + rowDelta - 1` (initial rows are 1-based), mapped back to a
z-index by `rowToZ` (`timeline.tsx:1815-1822`):

```
row < 0                  -> zRowsDesc[0] + (-row)          // above the top: new z above highest
row < zRowsDesc.length   -> zRowsDesc[row]                 // an existing layer
otherwise                -> lowestZ - (row - maxRow)       // below the bottom
```

then `updateAnnotationsLocal(zUpdates)`, `syncCursorFromAnnotations()`, and
`persistAnnotations(ids)` writes to the backend. `setIsDraggingAnnotation(false)` is the
first thing done, which re-enables compositing.

### 3.5 Resize (`timeline.tsx:1742-1802`)

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

### 3.6 Empty-lane press → range selection (`timeline.tsx:1853-1942`)

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

### 3.7 Press outside the lane area (`timeline.tsx:1945-1948`)

World Y between `headerHeight` and `trackStartY` (i.e. **over the waveform** or the row-0
insertion lane), or below `totalHeight`: `selectAnnotation(null)` and
`setSelectionCursor(null)`. **Clicking the waveform clears the selection; it does not
scrub.**

### 3.8 Double-click (`timeline.tsx:1972-2020`)

Ignored while a drag is active. Hit test is the **whole lane row** (no header-band
restriction) plus `clickTime` inside the clip. On hit: `navigate("/pattern/<patternId>")`
carrying `{name, from, backLabel: trackName, instanceId: annotation.id}` — i.e. it opens
the pattern editor for that clip. No hit → nothing.

### 3.9 Right-click → pattern insert (`timeline.tsx:2121-2273`)

`onContextMenu` (`timeline.tsx:2217-2233`), no-op when `readOnly`.
`computeInsertionTarget(clientX, clientY)`:

- `startTime = snapToGrid(x / zoom)` (15px threshold).
- `endTime = startTime + oneBarLength`, where one bar is the **mean** downbeat interval
  (`timeline.tsx:465-477`), falling back to `avgBeat * (beatsPerBar || 4)`; then
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

The ghost preview (`dragPreview`) is painted at that position while the menu is open, and
the menu (`PatternSearchMenu`) commits on select: insert mode first bumps every
`zIndex >= targetZ` by 1, then `createAnnotation` (`timeline.tsx:2242-2273`).

Menu keys (`pattern-search-menu.tsx:186-200`): `ArrowDown`/`ArrowUp` move the active row
(clamped), `Enter` commits, `Escape` closes. Hovering a row also makes it active and
recolors the ghost. A full-screen backdrop dismisses on mousedown.

---

## 4. Zoom and scroll

`components/hooks/use-timeline-zoom.ts:76-176`, one non-passive `wheel` listener on the
scroll container.

| gesture | effect |
| --- | --- |
| bare wheel / two-finger scroll | **not intercepted** — native container scroll, both axes (`overscrollBehavior: none`) |
| `metaKey` + wheel | horizontal zoom, `scale = exp(-deltaY * 0.002)` (`ZOOM_SENSITIVITY`) |
| `ctrlKey` + wheel (trackpad pinch on Chromium) | horizontal zoom, `scale = exp(-deltaY * 0.01)` — **hardcoded, 5× the constant** (`use-timeline-zoom.ts:161`) |
| `altKey` + wheel | vertical zoom, `scale = exp(-deltaY * 0.003)` (`ZOOM_Y_SENSITIVITY`) |
| Safari `gesturestart`/`gesturechange`/`gestureend` | horizontal zoom to `startZoom * event.scale`, rAF-coalesced |

Clamps: horizontal `[MIN_ZOOM, MAX_ZOOM] = [25, 500]`; vertical `[0.5, 1.5]`.

**Anchor is always the pointer, never the playhead.**

- Horizontal: `time = (clientX - rect.left + scrollLeft) / zoom` at the *start* of the
  gesture, then `scrollLeft = time * newZoom - pixel` after each step. The anchor is
  **latched** for the gesture and released after an idle timeout: **100 ms** for
  meta-wheel (`use-timeline-zoom.ts:142-147`), **120 ms** for ctrl-wheel
  (`:171-174`). This is what stops a momentum-scroll flick from walking the anchor.
- Vertical: anchored on `rowsFromBottom = (scrollHeight - (scrollTop + pixel)) /
  trackHeight` at the pointer's Y, then restored by `syncVerticalGeometry`
  (`timeline.tsx:1212-1253`) as
  `scrollTop = clamp(scrollHeight - rowsFromBottom*trackHeight - pixel, 0, maxScrollTop)`.
  Alt-wheel above `trackAreaY` is **ignored** (`use-timeline-zoom.ts:87`).
- With no anchor (window resize, layer-count change, `H`), `scrollTop = maxScrollTop`
  — i.e. **pinned to the bottom**, keeping z=0 on the floor.

Zoom writes go straight to `zoomRef` + spacer width, with a forced reflow
(`void spacer.offsetWidth`) before setting `scrollLeft`, so the new scroll is applied
against the new content width in the same frame (`use-timeline-zoom.ts:61-64`). Zoom and
scrollX are persisted to the store on unmount (`timeline.tsx:308-316`) and `zoomY` /
auto-scroll / panel height to `localStorage` (`use-track-editor-store.ts:877-902`).

No momentum, no rubber-banding: `scrollLeft` is only ever clamped by the browser to
`[0, scrollWidth - clientWidth]`, and every hand-written scroll is `max(0, …)`.

### Auto-scroll ("follow playhead")

`autoScroll` (toggled with `F` or the crosshair button, persisted to localStorage):
while on, every draw sets `scrollLeft = max(0, playhead*zoom - width/2)` when it differs
by more than 0.5px — **centering**, not edge-paging (`timeline.tsx:889-899`,
`1162-1173`). While playing with auto-scroll on, the rAF loop takes the full-draw path
every frame so scroll + tile blit + playhead land atomically (`timeline.tsx:1129-1132`).

### Minimap (48px strip above the timeline)

Pointer (`timeline.tsx:1277-1452`), all in minimap-local pixels,
`timeToPixel = width / durationMs`, `handleSize = 8`:

- within 8px of the lens's left edge → `resize-left`; right edge → `resize-right`;
  inside the lens → `move`; **outside the lens** → jump: `scrollLeft` centers the clicked
  time immediately, *and* a `move` drag starts from there.
- `move`: `scrollLeft = (initialStartTime + dx * (durationMs/minimapWidth)) / 1000 * zoom`.
- `resize-right`: `newLensW = max(10, startLensW + dx)`, `newZoom = containerWidth /
  visibleDurationSeconds`, clamped **`[5, 500]`** — note the low clamp is `5`, not
  `MIN_ZOOM` (25) (`timeline.tsx:1368`, `:1387`); left keeps the left edge fixed and
  right keeps the start time fixed.
- Hover (`timeline.tsx:1417-1452`) sets the cursor: `ew-resize` on either handle, `grab`
  inside the lens, `pointer` outside.

Minimap paint (`timeline-minimap.tsx:148-193`): outside the lens is dimmed with
`background @ 0.55`, inside is lifted with `foreground @ 0.08`, a `chart-3 @ 0.85`
1px lens border with two 3px `chart-3 @ 0.9` handle bars, a yellow loop band, and a 1px
`chart-3` playhead.

---

## 5. Keyboard — the complete map

Two `window`-level `keydown` listeners. **There is no focus requirement** beyond "not
typing": both bail when `target.tagName` is `INPUT` or `TEXTAREA` or
`target.isContentEditable` (`timeline.tsx:2311-2319`, `track-editor.tsx:336-341`). The
canvas is never focused and has no tabindex.

| keys | action | source |
| --- | --- | --- |
| `Space` (matched on `e.code`) | play / pause; no-op when no track is open | `track-editor.tsx:333-350` |
| `Cmd/Ctrl+Z` | undo (per-track undo stack) | `timeline.tsx:2324-2330` |
| `Cmd/Ctrl+Shift+Z` | redo | `timeline.tsx:2333-2339` |
| `Cmd/Ctrl+E` | split every clip straddling the cursor time, in the cursor's row band | `timeline.tsx:2342-2346` |
| `Delete` / `Backspace` | if the cursor has a range → `deleteInRegion()`; else delete the selected clips | `timeline.tsx:2349-2357` |
| `Alt+ArrowUp` / `Alt+ArrowDown` | move selected clips one lane up / down | `timeline.tsx:2360-2366` |
| `Cmd/Ctrl+C` | copy (region or object mode, §6) | `timeline.tsx:2369-2373` |
| `Cmd/Ctrl+X` | cut | `timeline.tsx:2376-2380` |
| `Cmd/Ctrl+V` | paste at the cursor | `timeline.tsx:2383-2387` |
| `Cmd/Ctrl+D` | duplicate after the cursor | `timeline.tsx:2390-2394` |
| `Cmd/Ctrl+L` | set loop from the cursor range; clear if the range equals the current loop (1 ms tolerance) or there is no valid range | `timeline.tsx:2399-2427` |
| `F` | toggle follow-playhead | `timeline.tsx:2430-2434` |
| `H` | auto-fit vertical zoom: `zoomY = clamp((clientHeight - 32 - 80) / ((layers+1) * 80), 0.5, 1.5)`, then bottom-anchored re-layout | `timeline.tsx:2437-2452` |
| `ArrowUp`/`ArrowDown`/`Enter`/`Escape` | only inside the pattern search menu | `pattern-search-menu.tsx:186-200` |

Bindings the web editor **does not** have: nudge by arrow keys (bare arrows are unbound),
Escape-to-deselect, zoom keys, transport scrub keys, save. Modifier keys are read as
`metaKey || ctrlKey` throughout — both work on every platform. The published shortcut
sheet is `components/timeline-shortcuts.tsx:10-36` and matches the handlers.

---

## 6. Command semantics behind the bindings

All in `stores/use-track-editor-store.ts`. Every mutating command is wrapped in
`withUndo(label, …)` and short-circuits on `readOnly`.

**Row ↔ z mapping** used everywhere: distinct z values sorted **descending** are rows
`0..N-1`; the cursor's `trackRow` is 1-based (row 0 is the empty top lane), so
`zIdx = trackRow - 1` (`use-track-editor-store.ts:441-447`).

- **`getRegionInfo`** (`:409-460`) — returns `null` unless the cursor has an `endTime`.
  Otherwise `[min,max]` of the cursor times × the z set covered by
  `[min(trackRow, trackRowEnd), max(...)]`.
- **`splitAtCursor`** (`:1430-1546`) — splits at `selectionCursor.startTime` every clip
  in the affected z set with `startTime < t < endTime`; skips a split where either half
  would be `< MIN_ANNOTATION_DURATION` (0.05 s). The new right halves become the
  selection.
- **`deleteInRegion`** (`:1548-1639`) — `resolveOverlaps(annotations, rangeStart,
  rangeEnd, affectedZ, ∅)` then apply; clears both selection and cursor. Partially
  overlapping clips are **clipped**, not deleted whole (see `utils/overlap-resolution.ts`).
- **`moveAnnotationsVertical`** (`:1641-1770`) — up: each selected clip takes the z of the
  row above, or `highestZ + 1` if already at the top. Down: **all-or-nothing** — if any
  selected clip is already at the bottom row the whole command is a no-op, so a multi-lane
  selection cannot collapse against the z=0 floor.
- **`copySelection`** (`:1772-1842`) — two modes.
  *Region mode* (cursor has a range): every clip overlapping the range × row band is
  **clipped** to the range and stored with `offsetFromStart` relative to `regionStart`;
  clips shorter than 0.05 s after clipping are dropped; `totalDuration = regionEnd -
  regionStart`. *Object mode* (point cursor): the explicitly selected clips are copied
  whole, offsets relative to the cursor start, `totalDuration` running to the last
  selected clip's end. Requires a cursor — with no cursor, copy is a no-op.
- **`cutSelection`** (`:1844-1926`) — copy, then delete: region mode uses the same
  `resolveOverlaps` as `deleteInRegion`; object mode removes the selected ids whole.
- **`paste`** (`:1928-2067`) — **top-left anchored**: the highest-z clipboard item lands on
  the cursor's row (`targetRow = max(0, trackRow - 1)`), other items keep their relative
  row offsets, mapped back to z through the combined z list, extending below the floor if
  needed. Paste start is `min(cursor.startTime, cursor.endTime)`; the destination region is
  cleared with `resolveOverlaps` first; items whose end would exceed `durationSeconds` are
  dropped. Afterwards the cursor spans `[pasteStart, pasteStart + totalDuration]` and the
  pasted clips are selected.
- **`duplicate`** (`:2069-2116`) — copy, move the cursor to the selection's **end**
  (point cursor) with `trackRow` re-derived from the topmost *selected clip's* z (not the
  stale drag-origin row), then paste. Net effect: a copy immediately after the original.
- **`cloneAnnotationsInPlace`** (`:2127-2150`) — local-only UUID copies, used by Alt+drag.
- **`setLoopRegion` / `clearLoopRegion`** (`:855-868`) — store + `host_set_loop_region`.
- **`play`** (`:816-822`) — **seeks to `playheadPosition` first**, then plays: that is what
  makes Play resume from a scrub made while stopped. `pause` (`:824`), `seek` (`:830`).
- **`setPlaybackRate`** (`:903-916`) — clamped; the transport UI offers 1× and 0.5×
  (`track-editor.tsx:443-472`).
- **`syncPlaybackState`** (`:836-844`) — adopts `isPlaying` + `currentTime` from the host
  snapshot whenever `isLoaded`.

---

## 7. Visual states

Clips (`utils/timeline-drawing.ts:338-438`):

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
- dragged clips are painted at `row + rowDelta` (`:341-345`) — the only preview of a
  pending lane change.

Selection cursor (`:569-636`): point cursor = 2px `accent` vertical line spanning
`[minRow, maxRow]` lanes; range cursor = `accent @ 0.15` fill plus a 2px `accent`
rectangle over the same row band.

Loop region (`:638-668`): `rgba(234,179,8,0.12)` fill from `headerHeight` down over
everything, with 1px `rgba(234,179,8,0.7)` boundary lines.

Insertion feedback (`:326-335`, `:440-457`): add mode highlights the target lane
`accent @ 0.1` with an `accent @ 0.4` outline; insert mode draws a 2px `accent` line at
the boundary with a small left-edge arrow.

**Hover** is expressed entirely through the cursor, not through fills
(`timeline.tsx:2034-2116`), and only inside a clip's header band:

- within 8px of the left edge → a custom bracket-left SVG cursor
  (`CURSOR_BRACKET_L`, `timeline.tsx:59`), right edge → `CURSOR_BRACKET_R`;
- elsewhere in the header → `grab`; anywhere else → `default`.
- during a drag: `grabbing` for a move, the bracket cursors for a resize.
- minimap: `ew-resize` / `grab` / `pointer` as in §4.

There is **no hover highlight on rows or clips** — a hovered clip looks identical to an
unhovered one.

---

## 8. Things the GPUI port cannot express today — needs new capability

Listed rather than dropped. Each is a seam that has to exist before the behavior can be
ported at all.

1. **Vertical scroll + bottom-anchored lane layout.** The GPUI canvas has no scrollTop
   and a fixed `zoomY = 1`. Alt-wheel vertical zoom, `H` auto-fit, the row-0 insertion
   lane's drop behavior and every `rowsFromBottom` anchor depend on it.
2. **A right-click / context-menu event.** GPUI's `listen` filters to
   `MouseButton::Left`, and the harness API (`gpui/crates/agent/src/api.d.ts`) has no
   right-click. Pattern insertion is unreachable without both.
3. **A double-click event.** Neither the canvas handlers nor the harness have one; the
   "open the pattern" gesture has no expression.
4. **Hover cursors.** GPUI sets no cursor per region, and there is no custom-bitmap
   cursor equivalent to the two Ableton bracket SVGs.
5. **A mutation seam beyond "move one clip".** `Library::move_clip` writes one clip's
   bounds. Create, delete, multi-clip batch update, z-index change, split, and the
   clipboard commands all need seam calls that do not exist.
6. **An undo stack.** `use-undo-store.ts` has no GPUI counterpart; `Cmd+Z` has nothing to
   call.
7. **A clipboard.** Copy/cut/paste/duplicate need the `ClipboardItem` model and
   `resolveOverlaps` ported (`utils/overlap-resolution.ts`).
8. **Loop region.** No `host_set_loop_region` call is wired into the GPUI library seam,
   and the paint has no loop band.
9. **The minimap.** No element, no seam; it is a second canvas with its own gesture set.
10. **Clip body previews.** The heatmap `ImageBitmap` per clip has no GPUI path (would
    need an image element or a texture upload).
11. **Playhead extrapolation between snapshots.** GPUI polls the host at 30 Hz and paints
    the last reported position; the web runs a local clock and re-anchors only on a
    >0.25 s disagreement. Without an equivalent clock the playhead steps rather than
    glides, and no amount of gesture work fixes it.
12. **Harness reach for modifier-held pointer gestures.** `app.drag` takes `ActOptions`
    but the file's `Modifier` list is only documented on `scroll`; shift-click,
    alt-drag and the multi-select contract cannot be verified until modifiers are
    accepted on `click`/`drag`.

### Smells found in the web source while extracting (flag, do not port)

- `getTripletBeatSnap` (`timeline.tsx:566-593`) is defined, listed in two dependency
  arrays, and **never called**.
- `getQuarterBeatSnap` and `getSixteenthBeatSnap` are the **same function** (both D = 4);
  the three-tier zoom ladder is really two tiers.
- Two snap thresholds (15px vs 12px) for the same conceptual gesture, defined in two
  places (`timeline.tsx:1487` and `:1693`) with duplicated bodies.
- `ZOOM_SENSITIVITY = 0.002` is bypassed by a hardcoded `0.01` on the ctrl-wheel path
  (`use-timeline-zoom.ts:161`).
- The minimap resize clamps zoom to `[5, 500]` (`timeline.tsx:1368`, `:1387`) while every
  other path clamps to `[MIN_ZOOM, MAX_ZOOM] = [25, 500]`. The minimap can push the
  timeline to a zoom no other control can reach or leave.
- `src/features/track-editor/components/minimap.tsx` (203 lines) is **unimported dead
  code** — the live minimap is `timeline-minimap.tsx` plus the handlers in `timeline.tsx`.
- A header click seeks twice (mousedown and mouseup, `timeline.tsx:1519` / `:2294`).

---

## 9. Gap table — web semantics vs `gpui/crates/app/src/track_editor.rs`

Legend: **match** = same observable behavior; **divergent** = present but behaves
differently; **missing** = no expression at all.

| # | Behavior | Web semantics | GPUI today | Verdict |
| --- | --- | --- | --- | --- |
| 1 | Scrub region | mousedown with **screen Y < 32** (ruler strip only) starts a scrub (`timeline.tsx:1519`) | `y < TRACK_AREA_Y` = 112 — the whole ruler **and waveform** scrub (`track_editor.rs:723`) | **divergent** (and the doc comment at `:722` asserts the web behaves this way; it does not) |
| 2 | Waveform press | clears selection and cursor (`timeline.tsx:1945-1948`) | scrubs | **divergent** |
| 3 | Scrub clamp | `clamp(x/zoom, 0, durationSeconds)` | press: `time.max(0.)` only, no upper clamp (`:725`); drag: clamped both ends (`:786`) | **divergent** (press can seek past the end) |
| 4 | Seek throttle | 32 ms, flushed on release (`timeline.tsx:795-826`) | every move fires a seek (`:814`, `:921`) | **divergent** |
| 5 | Scrub while playing | keeps playing, local clock re-anchors | keeps playing; `apply_transport` refuses to adopt host position during `Scrub` (`:698`) | **match** |
| 6 | Scrub snapping | none | none | **match** |
| 7 | Playhead motion while playing | rAF-extrapolated at vsync, re-anchored only on >0.25 s error | 30 Hz poll, painted as reported (`:1012`) | **divergent** (visible stepping) |
| 8 | Clip hit region | **top 18px header only** (`timeline.tsx:1547-1548`) | whole lane row (`clip_at`, `:365-369`) | **divergent** |
| 9 | Overlapping-clip tie-break | first in array order | first in array order (`.find`) | **match** |
| 10 | Plain click on unselected clip | selection = `[id]` | `selected = Some(id)` (`:740`) | **match** |
| 11 | Shift-click to add / remove | toggles membership (`timeline.tsx:1556-1568`) | no modifier read; selection is `Option<SharedString>` (`:105`) | **missing** |
| 12 | Multi-select at all | array of ids | single `Option` | **missing** |
| 13 | Selection cursor set on clip click | cursor spans the clicked clip (`:1572`) | no cursor concept | **missing** |
| 14 | Read-only | selectable, not draggable (`:1585`) | `writable()` gates the edge gesture (`:752`) | **match** |
| 15 | Edge handle width | 8 world px from either end, only after a header hit | `HANDLE / zoom` seconds from either end, after a whole-lane hit (`:744-750`) | **divergent** (region differs; width matches) |
| 16 | Handle draw width | 6px plates, 3 grip dots each (`timeline-drawing.ts:404-421`) | 6px plates (`HANDLE_MARK`), **no grip dots** (`:2007-2020`) | **divergent** (minor) |
| 17 | Resize snapping | snap to grid, 12px capture | none (`drag_edge`, `:373`) | **missing** |
| 18 | Resize multi-clip | same edge of **every selected clip** moves by the same delta | only the pressed clip | **divergent** |
| 19 | Resize min length | 0.1 s guard on both sides | `MIN_CLIP = 0.05` (`:408`) | **divergent** |
| 20 | Resize right clamp | `min(durationSeconds, …)` | `end.clamp(start+MIN, max(duration, start+MIN))` — can exceed duration when `start+MIN > duration` (`:381`) | **divergent** (edge case) |
| 21 | Resize grab offset | edge follows `deltaTime` from the press | `grab` offset preserved (`:758`) — same net behavior | **match** |
| 22 | Drag-to-move a clip | horizontal move of all dragged clips, snapped, `>= 0` | **no move gesture** — a press in a clip body pans the view (`:735`) | **missing** |
| 23 | Drag between lanes | `round(dy / trackHeight)`, visual during drag, z applied on release, clamped only downward | none | **missing** |
| 24 | Alt+drag duplicate | clone in place, drag originals | none | **missing** |
| 25 | Empty-lane drag → range selection | rectangular time × row marquee, fully-contained clips selected, 1 ms epsilon | pans the view instead (`Gesture::Pan`, `:731`) | **divergent** (GPUI-only gesture; web has no drag-pan) |
| 26 | Press below the last lane | clears selection | pans | **divergent** |
| 27 | Double-click a clip | navigate to the pattern editor | no double-click handling | **missing** |
| 28 | Right-click | pattern search menu + insertion ghost, insert-vs-add by 0.25-row boundary proximity | `MouseButton::Left` only (`:1354`) | **missing** |
| 29 | Bare wheel | native scroll of the container, both axes | horizontal pan by `wheel.x + wheel.y` (`:1392`) — vertical wheel pans horizontally | **divergent** |
| 30 | Modifier wheel zoom | `secondary` (cmd) at `exp(-Δy*0.002)`, ctrl at `exp(-Δy*0.01)` | both mapped to `exp(Δy*0.002)` (`:1389`, `:848`) | **divergent** (ctrl rate, and sign convention differs by platform delta polarity) |
| 31 | Zoom anchor | pointer X, **latched for the gesture**, released after 100/120 ms idle | pointer X, recomputed per event (`zoom_about`, `:292`) | **divergent** (a momentum flick walks the anchor) |
| 32 | Zoom clamps | `[25, 500]` | `[25, 500]` (`:273-274`) | **match** |
| 33 | Default zoom | 50 px/s | `DEFAULT_ZOOM = 50.` (`:276`) | **match** |
| 34 | Scroll clamp | `max(0, …)`, upper bound by content width | `max(0.)` only — **can scroll arbitrarily past the end** (`:792`, `:851`) | **divergent** |
| 35 | Alt+wheel vertical zoom | `exp(-Δy*0.003)`, clamp `[0.5, 1.5]`, anchored on rows-from-bottom, ignored above `trackAreaY` | `zoomY` fixed at 1 | **missing** |
| 36 | Trackpad pinch (`gesture*`) | zoom to `startZoom * scale` | none | **missing** |
| 37 | Lane layout | bottom-anchored; z=0 pinned to the viewport floor; grows upward | top-anchored from `TRACK_START_Y` (`:1276`) | **divergent** |
| 38 | Lane stripes | alternating `muted @ 0.2 / 0.15`, 1px rules, row-0 and below-floor at `rgba(0,0,0,0.3)` | same (`paint_lanes`, `:1919-1966`) | **match** |
| 39 | Clip body alpha | `1` selected, `0.75` otherwise; header always opaque | same (`:1977-1989`) | **match** |
| 40 | Clip border | `0.35` / 1px, `0.9` / 1.5px selected | same (`:1990-1997`) | **match** |
| 41 | Label suppression | `w > 30` | `width <= 30` returns (`:2024`) | **match** |
| 42 | Label ink | sRGB luminance > 0.5 → black | same coefficients (`ink`, `:2055`) | **match** |
| 43 | Clip min width | `max(4, floor(…))` | same (`clip_box`, `:1270-1272`) | **match** |
| 44 | Hover cursor (bracket / grab) | four cursor states over the canvas | none | **missing** |
| 45 | Space play/pause | window listener on `e.code`, skipped in text inputs | `PlayPause` action bound to `space` in the `TrackEditor` context, excluding `TextInput` (`keymap.rs:70`) | **match** |
| 46 | Play resumes from the scrubbed position | `play()` seeks first (`store:818`) | `toggle_playback` seeks then plays (`:621-627`) | **match** |
| 47 | `Cmd+Z` / `Cmd+Shift+Z` | undo / redo | none | **missing** |
| 48 | `Cmd+E` split | split straddling clips in the cursor's row band | none | **missing** |
| 49 | `Delete` / `Backspace` | region delete or clip delete | none | **missing** |
| 50 | `Alt+Arrow` lane move | up unbounded, down all-or-nothing | none | **missing** |
| 51 | `Cmd+C/X/V/D` | region / object clipboard, top-left-anchored paste | none | **missing** |
| 52 | `Cmd+L` loop toggle | set from cursor range, or clear when it matches | none | **missing** |
| 53 | `F` follow playhead | centers the playhead each frame | none — no auto-scroll at all | **missing** |
| 54 | `H` auto-fit vertical zoom | `clamp((h-112)/((layers+1)*80), 0.5, 1.5)` | none | **missing** |
| 55 | `Escape` | unbound in the timeline (only closes the search menu) | bound to `Back` — leaves the editor (`keymap.rs:71`) | **divergent** (GPUI-only) |
| 56 | Loop region paint | yellow band from `headerHeight` down + boundary lines | none | **missing** |
| 57 | Selection-cursor paint | point line / range rect in `accent` | none | **missing** |
| 58 | Insertion ghost + line | add-mode lane highlight, insert-mode boundary line | none | **missing** |
| 59 | Minimap | 48px lens strip with move / resize / jump gestures and its own zoom clamp | none | **missing** |
| 60 | Clip heatmap previews | `ImageBitmap` per clip, nearest-neighbor, `w >= 8` | flat color body | **missing** |
| 61 | Persist on release | `persistAnnotations(ids)` for every dragged clip | `move_clip` for the one dragged clip, serialized, one in flight (`:936`) | **match** for the single-clip case |
| 62 | Compositor suppression during drag | `setIsDraggingAnnotation` gates the composite effect | no compositor here | **n/a** |
| 63 | Beat grid under waveform and clips | grid painted before waveform and clips | same order (`paint`, `:1419-1447`) | **match** |
| 64 | Bar-label step / beat culling | `ceil(80/pixelsPerBar)`, 6px cull against the last drawn beat | ported with goldens (`:1473-1560`) | **match** |
| 65 | Waveform source | fixed 30 000 stored buckets, stretched past 333 px/s | measured fine window at a bucket per pixel past that (`:326-347`) | **divergent by design** (documented at `:35-41`) |
| 66 | Playhead paint | 1px `chart-3` line + 8px triangle, DOM-composited | 1px line + pointer, painted (`:2065`) | **match** |
| 67 | Panel resize (timeline height) | drag handle, `clamp(200, 600)`, persisted | none | **missing** |
| 68 | Transport rate 1× / 0.5× | two buttons, `host_set_playback_rate` | none | **missing** |
| 69 | Timecode readout | BAR.BEAT / BEAT / SEC from the beat grid | `M:SS / M:SS` only (`:1140-1144`) | **divergent** |
