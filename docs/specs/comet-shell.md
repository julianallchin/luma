# The Comet Shell — Luma's workspace redesign

**Status:** design settled, not built.
**Owner:** the `gpui/crates/app` shell.
**Supersedes:** the `Screen` router in `gpui/crates/app/src/lib.rs`.
**Reference implementation:** comet (`crates/ui/src/shell.rs`, `shell/tabs.rs`, `rail.rs`, `motion.rs`, `theme.rs`).

---

## 0. The one-line change

Today the agent chat is a 420px panel that slides in **over** whichever screen is
up. Tomorrow the agent thread **is** the app, the track list is a sidebar beside
it, and everything that used to be a screen is a **tab in a right-hand
workspace panel**.

```text
┌───────────────────────────────────────────────────────────────────────────┐
│ ○ ○ ○   Aurora — front wash             [track editor][graph][+]  ⤢  ▣   │ titlebar (38)
├──────────────┬──────────────────────────────────┬─────────────────────────┤
│  VENUE  ▾    │                                  │                         │
│              │                                  │                         │
│  Aurora    ● │        the agent thread          │   the active tab        │
│  Nightcall   │        (comet transcript)        │   (track editor,        │
│  Glasshouse  │                                  │    pattern graph,       │
│  …           │                                  │    visualizer,          │
│              │   ┌──────────────────────────┐   │    universe)            │
│              │   │  composer                │   │                         │
│              │   └──────────────────────────┘   │                         │
├──────────────┴──────────────────────────────────┴─────────────────────────┤
   sidebar 208–400 (256)          centre: flex_1            workspace 360–760 (520)
   ⌘B                                                       ⌘⇧B
```

Three regions, one persistent shell. Nothing is destroyed to show something
else, which is the whole point: **provenance chains exist only because the
screen they came from was thrown away.** A shell that never throws anything
away defines `Back` out of existence.

---

## 1. What comet actually does (read before arguing with §2)

Facts, with sources, so the port is a port and not a memory of one.

| Thing | comet | source |
|---|---|---|
| Regions | sidebar ∥ main ∥ right pane, one flex row, all three under a full-width overlay titlebar | `shell.rs` `impl Render for Shell` |
| Sidebar | 208–400px, default 256, collapsible, drag-resizable, width persisted debounced 400ms | `settings.rs:24-42` |
| Right pane | 360–760px, default 520; **expand** mode takes over everything right of the sidebar | `shell.rs::right_target` |
| Region width motion | manual `WidthTween` (`from`, `to`, `Instant`) evaluated per frame against `RESIZE` = 200ms ease-out; a fixed-width inner inside an `overflow_hidden` container so content never reflows mid-slide | `shell.rs:479`, `pane_container` |
| Resize handle | gpui drag-and-drop (`on_drag` empty ghost + root `on_drag_move::<Marker>`), floats over the seam at zero layout width; double-click resets to default | `shell.rs::resize_handle` |
| Session tabs | **deleted** — the sidebar *is* the session list; the titlebar names the selected session | `shell/tabs.rs:1-6` |
| Right-pane tabs | `RightSurface { Picker, Diff(id), Terminal(id), Subagent(id) }`, per-session ordered `Vec`, drag-reorderable, 112px fixed chip slots | `shell.rs:225`, `render_right_tab_strip` |
| Tab strip location | **in the titlebar band**, right-aligned, width == the pane's width, riding the pane's open tween — because the titlebar overlay owns that band's hit-testing and controls mounted in the pane itself would sit under the drag region | `shell/tabs.rs` `render_session_title_bar` |
| Tab chip anatomy | 24px tall, 112px wide, radius 6; leading 18px slot holds the type icon and **swaps in place** for a ✕ on hover; 11.5px label; active = `wash(0.10)`, hover = `wash(0.06)`; middle-click closes | `render_right_tab_strip` |
| Empty pane | `Picker` — "open a surface" cards, one per type; the `+` is the same list as a menu | `RightSurface::Picker` |
| Dead-tab healing | `resolved_right_active` — the stored pick renders only if it still exists, else the first remaining tab, else the picker | `shell.rs:1631` |
| Keys | `mod-s` sidebar, `mod-b` right pane, `mod-j` terminal, `mod-n` new session, `mod-k` palette — all rebindable, invalid combos fall back to the default | `shell.rs::apply_keymap` |
| Transcript | 46rem (736px) column with **48px** gutters, 14px/22 markdown, `RailTick` minimap hidden below 768px of main width | `transcript.rs:3291`, `markdown/render.rs:32`, `rail.rs:21` |
| Glass | `GLASS_ALPHA = 0.80` on macOS, 1.0 elsewhere; sidebar transparent on the frost; no inset cards, the conversation column sits flush | `theme.rs:435`, `Render for Shell` |

---

## 2. Region model

### 2.1 Sidebar — the subject list

**What it lists:** the selected venue's tracks. `tracks::Tracks` re-homed
verbatim; the table becomes a row list at sidebar width.

**Where venue selection lives.** Design it twice:

- **(A) Venue as a pre-shell gate.** Boot shows the venue grid; picking one
  enters the workspace. Keeps `welcome.rs` alive as a screen.
- **(B) Venue at the head of the sidebar.** A `luma_selector` above the track
  list. The grid dies; first run (no venue selected, or no venues at all) opens
  a **venue picker overlay** carrying the same cards.

**Chosen: B.** (A) reintroduces exactly the thing this redesign kills — a mode
the app enters and leaves, with the workspace destroyed on the way out. Under
(B) the venue is a *filter on the sidebar*, which is what it actually is: every
screen downstream of it (tracks, visualizer, universe) is already venue-scoped
and none of them is the venue itself. The picker overlay reuses the settings
overlay mechanism (§3.2), so "how do I open a modal over the shell" has one
answer.

`welcome::welcome`'s grid element survives as the picker overlay's body — the
cards keep their `role: "card"` a11y nodes, which is what keeps the harness
suite one edit away from green (§7).

**Row anatomy** (comet's session row, ported): status lead (a dot; the mini
gradient spinner while that track's thread has a turn running), title line,
`artist · bpm` under it in muted. Selecting a row:

1. sets the sidebar selection,
2. re-points the centre thread at that track's conversation,
3. becomes the default target for `+` in the workspace.

**Toggle:** `⌘B`. Collapsed is width 0 with the same `RESIZE` tween; the
titlebar's left inset glides with it (comet's `content_left`).

### 2.2 Centre — the agent thread

The centre is `luma_chat::AgentChat`, rendered `flex_1` with no width tween,
no open flag, no header of its own. **It cannot be closed.** The panel-ness of
`AgentChat` dies with the shell swap: `toggle`, `is_open`, `PANEL_WIDTH`, and
its private `WidthTween` all go (§3.1).

**Thread selection and history.** Luma's threads are not comet's: a thread is
*about* a subject (`ThreadScope { agent_kind, subject_kind, subject_id,
implementation_id, venue_id, score_id }`), and the model already allows several
threads per subject. So:

- **The sidebar row selects the subject; the subject selects the thread.**
  Choosing "Aurora" shows Aurora's conversation. This is why the sidebar can be
  the track list *and* the history at once, and why comet's second sidebar mode
  is not needed.
- **Within a subject**, the titlebar's thread title is a popover trigger:
  recent threads for this subject, `New thread`, `Rename`, `Delete`. Backed by
  `agent_thread_list` / `agent_thread_rename` / `agent_thread_delete`, which
  already exist in the dispatcher (`src-tauri/src/dispatch/mod.rs:252-264`) and
  need only `Library` methods.
- **Pattern threads** (`AgentKind::PatternGraph`) are reached the same way from
  the pattern graph tab's own title, not from the sidebar — a pattern is not a
  track and does not belong in the track list.

**With no thread:** comet's new-session canvas. Luma mark at 0.2 opacity, the
subject selectors under it, and a helper line — `Send a message to start a
conversation about Aurora.` / `Send a message to start.` with nothing selected.
The composer stays mounted; the first send mints the thread.

**What dies here:** `agent::retire_agent_chat`. It existed because the chat
followed the eye across screens and had to re-derive its scope at draw time.
Now the sidebar selection *is* the scope, set on click, in one place — a
navigation is no longer a field assignment that forgot to tell the chat.
`agent::scope_for` survives, re-pointed at the sidebar selection and the active
tab rather than at `Screen`.

### 2.3 Right — the workspace panel

A tab strip and one visible tab. Closed is width 0.

**Tab targets** — the closed vocabulary:

```rust
/// What a workspace tab shows. A tab is identified by its target, which is
/// what makes "open this" idempotent: opening a target that already has a tab
/// reveals that tab rather than minting a second view of one thing.
enum Target {
    TrackEditor { track: TrackId, score: ScoreId },
    Graph { pattern: PatternId, implementation: ImplementationId },
    Visualizer { venue: VenueId },
    Universe { venue: VenueId },
}
```

`Universe` is the DMX patch designer — `src/features/universe/components/universe-designer.tsx`:
the grouped fixture tree, the patch table, the DMX footprint strip and the
group-expression editor. It has **no gpui port today**. It ships in this phase
as a plate that lists the venue's patched fixtures from `Library::venue_rig`
(which exists) and names what it will grow into. A stub that lies about being
finished is worse than a stub that says what it is.

**Targeted opens.** One entry point, `Workspace::open(Target)`, and every
gesture routes through it:

| Gesture | Target |
|---|---|
| Double-click a clip in the track editor | `Graph { pattern, implementation }` for that clip |
| A pattern row in the pattern picker overlay | `Graph { … }` |
| A track row's "open editor" (or ⏎ on the selection) | `TrackEditor { track, score }` |
| `⌘⇧V` / the visualizer card | `Visualizer { venue }` |
| The `+` menu / the picker cards | whichever card, targeted at the sidebar's selection |
| **An agent tool call that names a subject** | that subject's tab, opened and focused |

That last row is why the target-keyed identity matters: the graph agent editing
a pattern calls `open(Target::Graph { … })`, and whether the tab already
existed is not the caller's problem. The chat crate emits a
`ChatEvent::SubjectTouched(ThreadScope)` on the first tool call of a turn that
names one; the shell subscribes and opens. One event, one handler — the chat
does not know what a tab is, and the shell does not know what a tool is.

**Multiplicity.** `TrackEditor` and `Graph` may have many tabs (different
tracks, different patterns). `Visualizer` and `Universe` are singletons per
venue — a second view of one rig is a mirror with no purpose. This falls out of
target-keyed identity for free; there is no separate rule.

**State retention.** A tab owns its state entity for its whole life. Switching
tabs does not tear down: a graph keeps its pan/zoom, a track editor keeps its
selection and its transport. Closing a tab drops the state; reopening reloads.
No resurrection cache — a second source of truth about what a tab was showing
is exactly the drift this redesign is removing.

**Dead-tab healing:** comet's `resolved_right_active`, ported verbatim. The
stored active target renders only if it is still in the list; else the first
remaining tab; else the picker.

**Empty state:** the picker — four cards (Track editor / Pattern / Visualizer /
Universe), same list the `+` menu shows. Disabled cards say why (`Universe`
with no venue selected, `Track editor` with no track selected).

**Expand:** comet's takeover (`⤢` in the titlebar). The workspace grows to
everything right of the sidebar and the thread column collapses behind it.
Closing always leaves takeover — reopening at full bleed with the thread gone
reads as a broken app.

---

## 3. What dies, and where everything lands

### 3.1 The death list

| Dies | Why | Replaced by |
|---|---|---|
| `enum Screen` and all seven variants | the shell is persistent; there is no "which screen" | region state: sidebar selection + `Vec<Tab>` + overlay |
| `Screen::Graph.from`, `TrackEditor.browser`, `Visualizer.previous`, `Settings.previous` | provenance exists to restore what was destroyed; nothing is destroyed | — |
| `Luma::back`, `keymap::Back`, `secondary-[` | see above | `Escape` dismisses the top overlay only (§4) |
| `Screen::key_context`, `Luma::focused_screen`, `Luma::take_focus` | one focus handle for one screen; three regions need three | per-region focus handles (§4) |
| `Luma::show_venues`, `Luma::find_venue`, `welcome::welcome` as a *screen* | the venue is a filter, not a destination | venue selector + venue picker overlay (grid element reused) |
| `close_graph` / `close_track_editor` / `close_visualizer` | each was "restore the boxed screen" | `Workspace::close(TabId)` |
| `keymap::ToggleAgentChat`, `secondary-shift-l` | the centre cannot be hidden | nothing — the chord is freed and stays free |
| `AgentChat::toggle` / `is_open` / `theme::PANEL_WIDTH` / its `WidthTween` | it is not a panel any more | `flex_1` |
| `agent::retire_agent_chat` | scope is set on selection, not re-derived at draw | — |
| `luma_chat::motion`'s privacy; `luma_md::theme`'s "nothing outside these two crates" rule | the shell itself is now comet-language | promoted to `luma_ui::glass` + `luma_ui::motion` (§6) |

### 3.2 Every existing screen's new home

| Today | Tomorrow |
|---|---|
| `Screen::Welcome` (venue grid) | venue picker **overlay**, opened from the sidebar's venue selector; auto-opens when no venue is selected |
| `Screen::Tracks` | the **sidebar** body |
| `Screen::Patterns` | pattern picker **overlay** (`⌘P`), opened from the `+` menu's Pattern card; picking one opens a `Graph` tab |
| `Screen::Graph` | `Target::Graph` **tab** |
| `Screen::TrackEditor` | `Target::TrackEditor` **tab** |
| `Screen::Visualizer` | `Target::Visualizer` **tab** |
| `Screen::Settings` | settings **overlay** over the whole shell — the only survivor of the overlay family, and it loses `previous` because the shell persists underneath |
| (nothing) | `Target::Universe` **tab**, new |

Overlays are one mechanism: `enum Overlay { Venues(..), Patterns(..), Settings(..) }`,
one at a time, `Escape` dismisses, rendered above all three regions. Three
callers, one implementation — the alternative (each overlay inventing its own
scrim and dismissal) is the second way to do one thing.

**Render functions are untouched.** Every screen module already exposes
`fn <name>(state, &Entity<Luma>, …) -> Div`. The tab/overlay wrapper calls it
unchanged. The diff in those modules is confined to their `impl Luma`
open/close blocks and their `self.screen` destructures.

**`track_editor.rs` is 19 `self.screen` sites** (16 of them `Screen::TrackEditor { state, .. }`).
Every one becomes `self.active_editor_mut()` / `self.active_editor()`. That is a
mechanical substitution with no paint or layout change, and it must be
coordinated with the waveform-unification work rather than raced. Two smells to
flag while there: the transport / clip-editing `impl Luma` blocks live inside
`track_editor.rs` even though they are app-level action handlers, and they will
be the only thing in that file that knows about tabs. They belong on the
workspace. Moving them is **not** part of this phase — flagged, not fixed.

---

## 4. Focus and keymap, redrawn

The existing context vocabulary (`keymap::context`) extends. It does not fork.

```rust
pub const ROOT: &str = "Luma";          // the window (kept)
pub const SIDEBAR: &str = "Sidebar";    // new — the region
pub const THREAD: &str = "Thread";      // new — the region
pub const WORKSPACE: &str = "Workspace";// new — the region
// Tab contexts, declared by the tab's own root INSIDE Workspace:
pub const TRACK_EDITOR: &str = "TrackEditor";  // kept, verbatim
pub const GRAPH: &str = "Graph";               // kept, verbatim
pub const VISUALIZER: &str = "Visualizer";     // kept, verbatim
pub const UNIVERSE: &str = "Universe";         // new
// Overlay contexts:
pub const VENUES: &str = "Venues";      // was WELCOME
pub const PATTERNS: &str = "Patterns";  // kept, now the overlay
pub const SETTINGS: &str = "Settings";  // kept, now the overlay
pub use luma_ui::TEXT_INPUT;            // kept
```

Contexts now **nest**: `Luma > Workspace > TrackEditor`. That is what lets
`space` keep meaning PlayPause under `TrackEditor && !TextInput` with the
binding **unchanged** — the whole track-editor binding block survives
character-for-character, because it was already scoped to a context that is now
a nested one instead of a top-level one. This is the strongest argument for
extending the vocabulary rather than replacing it.

**Focus.** Three handles, one per region, plus the overlay's. The rule replacing
`take_focus`: **focus follows the last click, and lands on the composer when
nothing holds it** (comet's `on_focus_lost` → composer). A region does not steal
focus on selection change; selecting a sidebar row re-points the thread without
taking the keyboard away from the composer, which is the behaviour you want when
you are mid-sentence.

**Bindings.**

| Key | Action | Context |
|---|---|---|
| `⌘B` | `ToggleSidebar` | `Luma` |
| `⌘⇧B` | `ToggleWorkspace` | `Luma` |
| `⌘T` | `NewTab` (opens the picker / `+` menu) | `Luma` |
| `⌘W` | `CloseTab` | `Workspace` — nests under the window's ⌘W, so the most specific context wins while a tab is focused and the window close still works everywhere else |
| `⌘1`…`⌘9` | `SelectTab(n)` | `Luma` |
| `⌘⇧V` | `OpenVisualizer` (kept) | `Luma` — now opens a tab |
| `⌘,` | `OpenSettings` (kept) | `Luma` |
| `⌘P` | `OpenPatterns` | `Luma` |
| `Escape` | dismiss the top overlay; else nothing | `Luma && !TextInput` |
| `⌘N` | `NewThread` for the current subject | `Luma` |
| everything track-editor | unchanged | `TrackEditor && !TextInput` |

`⌘W` under `Workspace` is the one binding that looks like a mode and is not:
gpui resolves by context specificity, so this is a *scoped* binding, not a
runtime `if`. Defining it as a binding rather than a branch is what keeps "which
⌘W did I get" answerable from the focus path.

The freed `⌘⇧L` stays free. Nothing is rebound onto it — a chord that meant one
thing for a release and another thing after is worse than a chord nobody presses.

---

## 5. Chat fidelity, folded in

The centre must be comet-fidelity, and the recorded deltas are all consequences
of the panel having been 420px wide. Concrete values:

| Delta | Today (panel) | Target (centre) | Source |
|---|---|---|---|
| **Reading column** | `MAX_CONTENT_WIDTH * 0.75` on user bubbles, `SPACE_LG` (16) gutters | 736px column, **48px** gutters, centred; user bubbles at 0.75 of the column | `transcript.rs:3291` |
| **Type scale** | 14px/22 in `luma-md` ✓ already correct | unchanged — verify the *chrome* around it also matches (11.5px tab labels, 12px titles, 13px sidebar rows) | `markdown/render.rs:32-33` |
| **Composer anatomy** | plate radius 18, textarea 76–260, actions row 46, send Ø28, model chip 24 | unchanged geometry, but the plate now centres on the 736 column rather than filling a 420 panel, and the file dropzone spans the **whole centre column** (transcript + composer), not the plate | `theme.rs:66-84`, comet `render_main` `chat-dropzone` |
| **Chip rail** | none | comet's `MessageRail` — a left minimap of user prompts, active tick brightens, hover previews (160/200 char caps), click smooth-scrolls. Gated on ≥768px of centre width, exactly as comet gates it | `rail.rs` |
| **Empty state** | `HERO_WIDTH` 260, glyph 40 | comet's canvas: mark at 0.2 opacity, subject selectors, helper line — wider than 260 now that it has a column | comet `render_main` |
| **Glass** | panel is glass, app is ladder, seam is a brutalist trim | the **shell chrome** is glass (`GLASS_ALPHA` 0.80 macOS / 1.0 elsewhere); the sidebar is transparent on the frost; the thread column sits flush and unbordered; **tab contents stay ladder** | comet `Render for Shell`, `theme.rs:435` |
| **Transcript fade** | 24px bottom band | asymmetric top+bottom `edge_faded` bands sized to the chrome, so content fades under the titlebar and the composer stack | comet `render_main` |

**The style boundary moves, and this is a real architectural decision.**
`docs/specs/agent-chat-gpui.md` §0 scoped the comet language to the `luma-chat`
+ `luma-md` crate boundary. That boundary cannot hold once the shell itself is
comet. The new rule, which keeps the same one-canonical-way force:

- **`luma_ui::ladder`** — instrument surfaces. Tab contents, controls, tables,
  the graph canvas, the timeline. Square, no motion, the six greys.
- **`luma_ui::glass`** — chrome surfaces. Titlebar, sidebar, tab strip, panel
  seams, the thread column, overlays. Translucent, `RESIZE`/`TAB_SLIDE` motion,
  comet's radii.

Two named surfaces, each internally singular, and which one a component belongs
to is decided by *what it is*, not by which crate it happens to live in. This
requires an edit to `CLAUDE.md`'s UI-design-system section; do not land the
shell without it, or the next agent reads a contract the code has already
broken.

---

## 6. New shared primitives (build the seam first)

Three things move into `luma-ui` **before** anything is rewired. All three are
extractions of code that already exists and is currently private — none is
speculative.

1. **`luma_ui::motion`** — `luma-chat/src/motion.rs` promoted whole. It is
   already a faithful port of comet's catalog (`RESIZE` 200ms ease-out,
   `TAB_SLIDE` 150ms, `FADE_IN`, `CubicBezier`, the shared 30fps pulse clock).
   The shell needs it; the chat keeps using it; there is exactly one catalog.
2. **`luma_ui::glass`** — `luma-md/src/theme.rs`'s palette promoted (§5).
3. **`luma_ui::pane`** — the region primitive:
   ```rust
   /// A region whose width animates between two values and whose content is
   /// laid out at its target width for the whole transition, so nothing
   /// reflows while it slides.
   pub struct PaneWidth { /* from, to, started */ }
   impl PaneWidth {
       pub fn retarget(&mut self, to: f32);
       /// This frame's width. Requests the next frame while still moving.
       pub fn eval(&mut self, window: &mut Window) -> Pixels;
   }
   pub fn pane(width: Pixels, inner: AnyElement) -> Div;
   /// The floating drag handle: zero layout width, resets to `default` on
   /// double-click. gpui's drag pattern, one implementation for both seams.
   pub fn resize_handle<M: 'static>(id: &'static str, default: f32, …) -> Div;
   ```
   `AgentChat::width` is the same tween, private and duplicated the moment the
   shell needs a second one. Extract it now; the chat's copy dies in the same
   commit that gives it away.

Width/collapsed state persists — the shell writes `sidebar_width`,
`sidebar_collapsed`, `workspace_width` through `Library::set_setting`, debounced
400ms (comet's `SAVE_DEBOUNCE_MS`). No new settings store.

---

## 7. Migration order

Sized so each step lands green. The hard constraint is the harness suite: every
navigation test today starts with `app.click(home.find({ role: "card", label:
"Test Venue" }))` and walks a screen chain.

**P0 — pane primitives.** `luma_ui::{motion, glass, pane}` extracted; `luma-chat`
and `luma-md` re-export from them so no call site changes. Suite untouched.
*Green by construction.*

**P1 — `Library` thread methods.** `threads(scope)`, `rename_thread`,
`delete_thread` over the existing `agent_thread_*` commands. Unused until P5;
covered by a `Library` round-trip test.

**P2 — the tab model, headless.** `Target`, `TabId`, `Tabs` (ordered vec, active
target, `open` / `close` / `select` / `reorder` / `resolve_active`). Pure logic,
unit-tested — comet's `resolved_right_active` healing, the target-keyed
idempotent open, close-then-heal. Rendered nowhere.

**P2.5 — test navigation helpers.** Extract every test's screen walk into
`crates/agent/tests/support/nav.js`: `nav.venue(app, name)`,
`nav.track(app, name)`, `nav.pattern(app, name)`. **No behaviour change** — the
helpers do exactly what the tests do today. This is the step that makes P3 a
one-file edit to the suite instead of a ten-file one, and it is the whole reason
P3 can land at all.

**P3 — the shell swap.** One commit, because a `Screen` router and a persistent
shell cannot coexist without being two ways to do one thing:
- `shell.rs` (regions, titlebar band, tab strip, overlays), `workspace.rs` (tab
  hosting), `lib.rs` reduced to the facade + the state struct.
- Each screen module's `impl Luma` open/close block rewritten to
  `open_tab`/`close_tab`/`open_overlay`. **Render functions untouched.**
- `track_editor.rs`: the 19 `self.screen` destructures → accessors. Mechanical.
  Coordinate with the waveform work.
- `keymap.rs`: contexts nest, `Back`/`ToggleAgentChat` deleted, new region keys.
- `support/nav.js` rewritten once.
- The centre is `AgentChat` at `flex_1`; its panel-ness is deleted here.

**P4 — chat fidelity.** The §5 table: 736/48 column, rail, empty-state canvas,
column-wide dropzone, asymmetric edge fades, glass chrome. Pure paint; the pixel
tests (`crates/agent/tests/pixel.rs`, `gauntlet_chat.rs`) get their baselines
re-blessed in this commit and nowhere else.

**P5 — thread history.** Titlebar thread popover, new/rename/delete, sidebar
row liveness dots wired to running turns.

**P6 — targeted opens.** Clip double-click → `Graph` tab.
`ChatEvent::SubjectTouched` → shell opens and focuses the tab. This is the step
that makes the agent feel like it is driving the app.

**P7 — tab polish.** Drag-reorder with the slide tween, middle-click close,
hover icon→✕ swap, `⌘1`…`⌘9`, expand/takeover, the `+` menu.

**P8 — `Universe`.** The real patch designer, replacing P3's fixture-list plate.
Separate work item; the tab type and its seam exist from P3.

---

## 8. Open questions

1. **Does the sidebar need a second mode after all?** Pattern threads have no
   home in the track list. If pattern work becomes common the answer is a
   segmented `Tracks | Patterns` head, not a second sidebar. Deferred until it
   hurts.
2. **Per-venue tab sets?** comet keys its tab list per session. Switching venues
   with a track editor open from the old venue is currently undefined. Cheapest
   correct answer: closing a venue closes tabs targeting it. Decide at P3.
3. **The visualizer viewport.** A concurrent workflow is landing a
   `luma-render` viewport component and possibly a `Screen` variant. P3
   **consumes** whatever it lands as the `Visualizer` tab body and re-homes any
   `Screen` variant it added — it does not duplicate it. Coordinate before P3
   starts.
4. **Rail on a narrow centre.** With both the sidebar and a 520px workspace
   open, the centre is under 768px on a 1440 display and the rail hides. That is
   comet's own behaviour and probably right, but it means the rail is invisible
   in the app's most common layout. Watch it in P4.

---

## 9. Style boundary (normative)

`CLAUDE.md` is Julian's file; an agent does not edit it. Until the UI-design
section carries this, **the boundary below is normative for the shell work** and
supersedes `docs/specs/agent-chat-gpui.md` §0's crate-boundary rule.

Two surfaces. Which one a component belongs to is decided by *what it is*, not
by which crate it lives in.

- **`luma_ui::ladder` — instrument surfaces.** Tab contents: the timeline, the
  graph canvas, tables, controls, the visualizer's plates. Square, no motion,
  the six greys. Every "Hard rule" in `CLAUDE.md` applies here without
  exception.
- **`luma_ui::glass` — chrome surfaces.** Titlebar, sidebar, tab strip, panel
  seams, the thread column, overlays. Translucent (`GLASS_ALPHA` 0.80 macOS,
  1.0 elsewhere), comet's radii, and the one place motion is allowed:
  `luma_ui::motion` drives region slides (`RESIZE`, 200ms) and tab transitions
  (`TAB_SLIDE`, 150ms). Nothing else animates.

  **One curve, one ladder.** Everything that moves eases on `motion::ROOT` —
  `cubic-bezier(0.16, 1, 0.3, 1)`, comet's signature expo-out — over a duration
  from `SNAP`/`QUICK`/`BASE`/`SLOW`. Comet's panel slides ride plain `ease-out`;
  the two are the same idea at different strengths, and one is the design. A
  new animation picks a rung; a fifth rung is a decision, not a literal.

A component that paints from both tiers is a component in the wrong place.

## 10. Deviations from this spec, as built

Recorded here in the same change that made them, per §0's contract.

1. **No `TabId`** (§7 P2 listed one). §2.3 already rules that a tab's identity
   *is* its target; a `TabId` beside it is a second key that can disagree with
   the first, which is the drift this redesign exists to remove. `Tabs::close`
   and `Tabs::select` take `&Target`; `reorder` and `select_index` take strip
   positions; gpui element ids come from `Target::element_key()`.
2. **Ids are `String`, not `TrackId` / `ScoreId` / `PatternId` / `VenueId`**
   (§2.3 shows newtypes). The crate has no id newtypes today, so minting four
   inside `Target` would put a second id vocabulary at the wrong layer and make
   every call site a conversion. Flagged as a follow-up, not silently dropped —
   the right change is a crate-wide one, not a tab-model one.
3. **Open question 2 is answered in the type.** `Target::venue()` returns a
   venue only for `Visualizer` and `Universe`. A track editor's score names a
   venue, but the tab is *about the track*: closing somebody's open timeline
   because they glanced at another room would be the shell throwing work away,
   which is the thing this redesign exists to stop. Leaving a venue closes that
   venue's rig views and nothing else.
4. **Switch-vs-close is structural, not remembered.** `Tabs::close` hands the
   body back; nothing else does. There is no way to express "switch away and
   stop the transport", so the ruling — tab switch tears down nothing, tab
   close runs the old close semantics — is a property of the interface rather
   than a rule each call site has to keep.
5. **P2.5's helpers poll — and the step *after* a step must too.** The suite
   had six spellings of "click the venue card", some wrapped in `until` and
   some not; `nav.*` unifies on the most robust (every step waits for the node
   it is about to press). But a polling step returns *earlier* than the fixed
   frame counts it replaced, so a bare `app.frames(N)` after `nav.track(...)`
   that used to land after the editor's opening load can now land inside it —
   which broke keyboard, track_editor and track_editor_lanes intermittently.
   The rule that came out of it: **after `nav.track`, wait for the timeline by
   its result** (`until("the timeline", …Waveform card…)`), never by a frame
   count. Applied to every converted editor-opening suite. A cautionary note
   on proof-by-failure-text: identical pre/post failure messages do not prove
   an unchanged cause — the conversion here reproduced a flake's symptom while
   adding its own cause underneath.
6. **Four test files are not on `nav.*` yet.** `track_editor_waveform.rs` and
   `track_editor_waveform_pixels.rs` declare a *local* `until(check, limit)`
   whose signature collides with the suite's `until(what, pred)` — a function
   declaration shadows the global, so splicing `nav` into them breaks it. That
   duplicate helper is a real smell and its removal is owed; both files are the
   waveform work's live territory, so it waits. `visualizer.rs` and
   `visualizer_budget.rs` are the viewport work's, and click *any* venue card
   rather than a named one. All four still walk the app by hand and must be
   converted before P3's shell swap, or they will be the tests P3 misses.

P3 landed the shell swap. Its deviations, in the same numbering:

7. **The workspace opens in takeover by default.** §2.3 defaults to the 520px
   side-by-side split; P3 inverts that (`Luma::expanded = true`) and adds
   `ToggleExpand` to switch. Reason: the harness suite's geometry premises —
   graph zoom-1, the editor's wheel arithmetic, the waveform FINE threshold,
   the visualizer's whole-window luminance fractions — were all authored
   against near-full-window surfaces, and re-deriving every one of them in the
   same commit as the structural swap is how a swap ships red. The 520 default
   lands with P4, the phase that re-blesses baselines anyway.
8. **`Target`'s fields are what today's gestures can name.** `TrackEditor
   { track, venue }` (the score is resolved from the pair), `Graph { pattern }`
   (the implementation arrives with the document). §2.3's wider keys assumed
   gestures that do not exist; a key wider than any gesture forces every call
   site to invent the missing half. The key widens in the change that adds the
   gesture.
9. **A sidebar row click opens the editor tab** as well as selecting the
   subject (idempotently — a second click reveals). §2.1 separates selection
   from an "open editor" gesture; until a second gesture exists, one gesture
   doing both is one gesture rather than a hidden mode.
10. **The chat's scope comes from the visible tab only.** §2.2 lets the
    sidebar selection carry a thread; a track thread's scope names a score,
    which only the editor resolves today. P5's thread history is where the
    sidebar row learns to resolve its own.
11. **The sidebar's venue head is a button that reopens the picker**, not the
    `luma_selector` §2.1 sketches. The picker overlay is the one
    venue-choosing mechanism; a second dropdown selector would be a second way
    to pick a venue.
12. **The sidebar row list dropped the table's added-by and preprocessing
    columns** (and the display-name lookup feeding added-by). §2.1's row
    anatomy — status lead, title, artist · bpm — has no place for them; they
    return with a wider tracks surface if one is wanted.
13. **No `Universe` tab body yet.** §2.3 ships it as a fixture-list plate in
    this phase, but no opening gesture exists until P7's `+` menu — a plate
    nothing can reach is dead code, not a stub. `Target::Universe` and its
    venue-scoped close semantics exist and are tested.
14. **`NewTab`/`NewThread`/the picker are not bound.** ⌘T's picker is P7's
    `+` menu; ⌘N's thread is P5. Binding a chord to a verb that does not exist
    yet is worse than the chord staying free.
15. **Widths are constants, not persisted, and regions cut rather than
    slide.** `luma_ui::pane`'s tween and the debounced `set_setting` writes
    (§6) are wired in the polish phase; P3's regions are fixed-width.
16. **Geometry tests state their canvas.** `Fixture::window(w, h)` lets a test
    whose pixel arithmetic was authored against a 1200×762 canvas grow the
    window by exactly the sidebar's 256 and the tab strip's 28
    (`track_editor_ux`, `track_editor_waveform`, `track_editor_waveform_pixels`).
    The alternative — re-deriving every constant — would have changed what
    those tests assert, not just where.
17. **The venue picker auto-opens only while the shell has nothing to show**:
    no venue, no overlay, *and no tabs*. A pattern's graph needs no venue, and
    a picker that camped over it would be the welcome screen refusing to
    leave.

P4 landed chat fidelity and the chrome anatomy, against the blind critic's
round-one verdict (scratchpad `shell-critic-verdict-r1.md`). Its deviations:

18. **§10.7 is repaid: the split (520) is the default again**, with
    `ToggleExpand` as the takeover switch. Geometry tests state takeover
    explicitly (`nav.expand()`), which is a user gesture and not a test
    backdoor.
19. **The titlebar's back/forward are permanently dimmed.** Comet's chrome
    carries them; this shell has no navigation history — nothing is destroyed,
    so there is nothing to go back to. Dimmed-at-rest keeps the anatomy
    without a control that lies.
20. **No composer paperclip or effort control** (verdict #8, partially
    declined). The chat cannot take attachments and has no effort knob — the
    model chip is the whole of that vocabulary here — and a control with no
    verb behind it is a silent-failure stub. The actions row order and the
    status strip are comet's; the missing controls arrive with their features.
21. **The status strip's idle line is the thread's subject** ("Pattern
    thread") — comet shows a VCS checkout, which a light show does not have.
22. **Turn timestamps stamp only witnessed turns.** The durable transcript
    carries no message times yet; a restored row shows none rather than a
    fabricated one. Adding times to the model is flagged as follow-up.
23. **The turn rail is ticks + click-to-scroll**, gated on window (not pane)
    width; comet's hover-preview cards and scroll-tracked active tick are P7
    polish.
24. ~~The shell plane is painted at comet's *effective* frost tone.~~
    **Resolved.** The plane is `glass::glass()` and the thread column is
    `glass::panel()`; the window backing is `Blurred`, so both are real
    translucency. See §10.27 for what the tones became.
25. **The sidebar's search and filter pills are hand-set glass controls** in
    `tracks.rs`, not `luma_ui` ladder components: the sidebar is chrome (§9),
    and the ladder's one-true-button rule governs instrument surfaces, not the
    frame. If a second glass surface needs these, they move to a
    `luma_ui::glass` widgets module.
26. **The capture fixture seeds a venue and three bare tracks** so the plates
    show the shell a user with a venue sees — round one's captures were
    venue-less and therefore sidebar-less, which the critic correctly read as
    "no sidebar anywhere".

27. **The glass tier is the grey ladder at a coverage — it mints no tones.**
    §9 named two tiers but left them two *palettes*: the ladder's `0e / 19 /
    21 / 27` beside comet's sampled `06 / 08 / 0d`, which is why the timeline
    read as a bright slab dropped into a near-black frame. `luma_ui::glass`
    now derives every surface from a ladder rung — plane = `titlebar_background`
    at 0.80, chrome card = `gutter` at 0.50, floating surface = `trim` at 0.85
    — so the app is one progression `0e → 19 → 27` across both tiers, and
    `glass::grey()` is gone. `luma_md::theme::Theme` names *roles* over those
    tokens and defines no surface tone of its own.
28. **A content card has no default fill.** Which ground a card takes is which
    tier its contents are (§9), and only the caller knows: the thread column
    paints `glass::panel()`, a workspace tab paints `ladder::background()`
    opaque. `shell::card()` is geometry only — a default there would be the
    wrong tier half the time and invisible when it was.
29. **The overlay screens are still full-bleed opaque ladder planes.** §9 calls
    overlays chrome, but venues / patterns / settings are re-homed instrument
    screens and a translucent ground under ladder controls is a component
    painting from both tiers. The overlay *plane* is now `glass::scrim()`, so
    the day a body is inset as a card the surround is already right; insetting
    them is geometry and unowned.

## 11. Smells flagged, not fixed

- The transport and clip-editing `impl Luma` blocks live inside
  `track_editor.rs` although they are app-level action handlers. After P3 they
  are the only thing in that file that knows about tabs; they belong on the
  workspace. (Already noted in §3.2 — confirmed against the code.)
- Two `until` helpers with the same name and different signatures (§10.6).
- `glass::ink` and `glass::hairline` are byte-identical functions
  (`hsla(0, 0, 1, a)`), separated only by a doc comment predicting a light mode
  that does not exist. Two names for one value, and every call site has to
  guess which. They merge, or one of them acquires a different body.
- The chat tier keeps its own recessive greys (`neutral(0.708)` /
  `neutral(0.556)`) beside `ladder::muted_foreground` (`#777`). Defensible —
  a recessive grey is calibrated to its ground and the two tiers' grounds are
  four rungs apart — but it is still two answers to "what colour is a
  secondary label", and nothing in the types says so.
- `track_editor_lanes.rs`, `_stack.rs` and `_ux.rs` each declare their own
  `node(role, label)` snapshot accessor. Three copies of one accessor; it
  belongs beside `nav`.
- The committed gauntlet PNGs (`harness/gauntlet/*`, `harness/gauntlet-chat/*`)
  predate the shell and are stale references until their `#[ignore]`d
  generators are re-run; `gauntlet.rs`'s per-shot window sizes were
  reverse-engineered for full-window fitView and need redoing against the tab
  layout.
