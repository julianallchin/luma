# The Comet shell

**Status:** built. This is the contract for the `gpui/crates/app` shell.
Section numbers are cited from code. Keep them stable.
**Reference implementation:** comet (`crates/ui/src/shell.rs`, `shell/tabs.rs`,
`rail.rs`, `motion.rs`, `theme.rs`).

---

## 0. Overview

The agent thread is the centre of the app. The track list is a sidebar beside
it. Editors are tabs in a workspace panel on the right.

```text
┌──────────────┬──────────────────────────────────┬─────────────────────────┐
│  venue       │                                  │ [track][graph][+]       │
│              │                                  │                         │
│  tracks      │        the agent thread          │   stage (optional)      │
│  …           │                                  │                         │
│              │                                  │   the active tab        │
│              │   ┌──────────────────────────┐   │                         │
│              │   │  composer                │   │                         │
│              │   └──────────────────────────┘   │                         │
└──────────────┴──────────────────────────────────┴─────────────────────────┘
   sidebar (256)            centre: flex_1              workspace
   ⌘B                                                   ⌘⇧B
```

Three regions, one persistent shell. Nothing is destroyed to show something
else, so there is no Back and no provenance chain.

---

## 1. What comet does

| Thing | comet | source |
|---|---|---|
| Regions | sidebar ∥ main ∥ right pane, one flex row | `shell.rs` `impl Render for Shell` |
| Sidebar | 208–400px, default 256, collapsible, drag-resizable | `settings.rs:24-42` |
| Right pane | 360–760px, default 520; expand mode takes everything right of the sidebar | `shell.rs::right_target` |
| Region width motion | a width tween with 200ms ease-out; a fixed-width inner inside an `overflow_hidden` container, so content never reflows mid-slide | `shell.rs`, `pane_container` |
| Right-pane tabs | per-session ordered list, drag-reorderable, fixed chip slots | `render_right_tab_strip` |
| Dead-tab healing | the stored pick renders only if it still exists, else the first tab, else the picker | `resolved_right_active` |
| Transcript | 736px column, 48px gutters, 14px/22 markdown, rail hidden below 768px | `transcript.rs`, `rail.rs` |

---

## 2. Region model

### 2.1 Sidebar — the subject list

The sidebar lists the selected venue's tracks. A button at its head reopens
the venue picker. The venue picker is the only way to choose a venue. It
opens by itself when there is no venue, no overlay and no tab.

A row shows a status lead, the title, and `artist · bpm`. A click selects the
track and opens its editor tab. A second click reveals the existing tab.

`⌘B` toggles the sidebar.

### 2.2 Centre — the agent thread

The centre is `luma_chat::AgentChat` at `flex_1`. It cannot be closed. The
chat's scope comes from the visible tab. The ChatHistory overlay lists every
conversation in the room. The Subagents overlay shows delegated work.

With no thread, the centre shows the new-thread canvas. The composer stays
mounted, and the first send creates the thread.

### 2.3 Right — the workspace panel

A tab strip and one visible tab. `⌘⇧B` toggles the panel. Expand / Show chat
switches between the split and the takeover layout.

A tab **is** its target (`gpui/crates/app/src/tabs.rs`):

```rust
enum Target {
    TrackEditor { track: String, venue: String },
    ScoreGraph { score: String, graph: String },
    Patch { venue: String },
}
```

- Opening a target that has a tab reveals that tab. There is no `TabId`.
- `Patch` is a singleton per venue, because there is one value per venue.
- Target fields name what a gesture can supply. A key widens only when a
  gesture needs it.
- Leaving a venue closes that venue's `Patch` tab only. A track editor is
  about its track, so it stays.

**State retention.** A tab owns its state for its whole life. Switching tabs
tears down nothing: playback continues and a loop region stays armed.
`Tabs::close` hands the body back, and only closing runs the close semantics.

**Dead-tab healing.** The stored active target renders only if it is still in
the list. Else the first tab renders. Else the picker renders.

**Stage.** The visualizer is not a tab. It is a stage view in the workspace
column. `⌘⇧V` hides or shows it. Hiding drops the stage and its GPU resources.
Shift-F opens it fullscreen.

---

## 4. Focus and keymap

Key contexts nest: `Luma > Workspace > TrackEditor`. The track-editor bindings
are scoped to `TrackEditor && !TextInput`, so they need no change when the
tab moves.

Focus follows the last click. A region does not take focus when the selection
changes.

| Key | Action | Context |
|---|---|---|
| `⌘B` | `ToggleSidebar` | shell |
| `⌘⇧B` | `ToggleWorkspace` | shell |
| `⌘T` | `NewTab` | shell |
| `⌘W` | `CloseTab` | `Workspace` (the window's `⌘W` still works elsewhere) |
| `⌘1`…`⌘9` | select tab | shell |
| `⌘⇧V` | `ToggleVisualizer` | shell |
| `⌘,` | `OpenSettings` | shell |
| `Escape` | dismiss the top overlay | overlays and dialogs |

`⌘W` under `Workspace` is a scoped binding, not a runtime branch. gpui resolves
it by context specificity.

---

## 5. Chat fidelity and the tier boundary

The centre matches comet: a 736px reading column with 48px gutters, a turn
rail, the empty-state canvas, and edge fades under the chrome.

The comet language is not bound to the chat crates. It is a named tier in
`luma_ui` that any crate can use. §9 states the rule.

---

## 6. Shared primitives

- **`luma_ui::motion`** — the one motion catalogue.
- **`luma_ui::glass`** — the translucent tier's paint.
- **`luma_ui::pane`** — `PaneWidth`, a region width that animates while its
  content is laid out at the target width.

---

## 8. Overlays

`Overlay` in `gpui/crates/app/src/shell.rs` is one slot, not a stack. Escape
dismisses it. The variants are Venues, Patterns (the library browser),
Settings, AddTracks, ChatHistory, Subagents, FixturePicker, InsertPattern,
AddFixtures, Confirm and GroupRepair.

The modal plane swallows presses, so covered shell controls cannot change
behind it. The window controls paint last, above any overlay, so the window
can always move and close.

---

## 9. Style boundary (normative)

The current style is the rounded, simple, glass look. Two tiers. The tier of a
component depends on *what it is*, not on its crate.

- **`luma_ui::ladder` — planes and instrument surfaces.** The sidebar, the
  thread column and the workspace, plus tab contents: the timeline, the graph
  canvas, tables and the stage. Planes are opaque. Panes tile the window, so
  their shared edges are square, and depth between panes is a seam.
- **`luma_ui::glass` — what floats.** Menus, popovers, dialogs, overlay grounds
  and chips. They are translucent and round their corners (`luma_ui::radius`).
  Movement uses the `luma_ui::motion` curves.

Glass mints no tones. Every glass surface is a ladder rung at a coverage.

Controls are rounded, with normal-case labels and subtle hover fills. Do not
use square, bordered or uppercase controls.

A component that paints from both tiers is in the wrong place.

---

## 10. Window chrome, as built

1. **No full-width titlebar.** Each region is a full-height column with its
   own head band (`chrome::band`). The bands align because they share one
   height.
2. **Regions are flush.** Depth between regions is a value step across one
   full-height seam. The shell draws no horizontal borders.
3. **Window controls are the top layer** (`chrome::window_controls`). The
   leftmost region's band reserves their width. The sidebar toggle rides the
   leftmost visible band, and the settings gear rides the rightmost, so hiding
   a region never hides the control that brings it back.
4. **Titlebar back and forward stay dimmed.** The shell has no navigation
   history.
5. **The composer has no attachment or effort control.** A control without a
   feature behind it is not added.
