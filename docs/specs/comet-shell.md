# The shell

**Status:** built. This is the contract for the `gpui/crates/app` shell.
Section numbers are cited from code; keep them stable.
**References:** comet (motion, the agent panel) and zeron (`apps/zgx/ui/ds`:
the flush-panel tokens and the settings page).

---

## 0. Overview

The window is one venue, shown in one of three modes. A dark trim frames
one rounded page: the title bar runs across the trim above the page, the
status bar along the trim below it, and a thin margin of trim shows either
side. The page holds everything else — the agent panel beside the mode's
stage and body:

```text
 ●●● ◧  Venue ▾                                     [Rig|Arrange|Live]
 ╭────────────┬─────────────────────────────────────────────────────╮
 │         ↶+ │  stage (the venue lit by the mode's body)           │
 │            │           [ Aurora / #1 ▾ ]  (Arrange)              │
 │  agent     ├─────────────────────────────────────────────────────┤
 │  panel     │  the mode's body: the venue page, the score,        │
 │  (the      │  or the Perform set — or the song list over Arrange │
 │  body's    │                                                     │
 │  chat)     │                                                     │
 ╰────────────┴─────────────────────────────────────────────────────╯
 sync · DMX output · import · update                     context line
```

Nothing is destroyed to show something else: each mode keeps its body while
another mode is up, and each venue keeps its modes while another venue is up.

## 1. Regions

| Region | What it is | Code |
|---|---|---|
| Agent panel | the front body's chat; 300 min, a quarter of the window by default, drag-resizable, ⌘B hides it; stands aside while the song list is up | `shell::agent_panel`, `agent_split`, `Luma::agent_wanted` |
| Title bar | 44 high, on the trim across the whole window: traffic lights, agent toggle, the `venue / song / score` trail, the mode switch | `shell::title_bar`, `chrome` |
| Page | the inset card on the trim, rounded `PAGE_RADIUS` (12), `WINDOW_TRIM` (8) from the window's sides; holds the agent panel and the content | `shell::regions`, `glass::window_trim` |
| Stage | the venue, lit by the mode's body; collapsible, height-resizable. In Live a deck panel sits at its left, drag-resizable, its width kept per device (`deck-panel-width`) | `shell::stage_body`, `perform::deck_panel` |
| Body | the mode's body, or the song list in Arrange | `shell::content` |
| Status bar | 24 high, cached, on the trim under the page in muted text; sync, DMX output, import, update, account failure, and the mode's context line | `status_bar` |

The title bar and everything in it hold still whatever the page shows. Hiding
the agent panel takes the panel off the page and nothing else: the chat's own
header (history, new chat) is the panel's, at the top of the page, and goes
with it.

## 2. Per-venue state

`workspace::Modes` holds one optional body per mode — Rig (`Target::Venue`,
the venue page), Arrange (`Target::Score`, one open score) and Live
(`Target::Perform`, the Perform set) — and which mode is up.
`workspace::Venues` parks every other venue's `Modes`, so switching venue and
back finds the same mode and the same open score.

- Opening a score puts it in Arrange, replacing the score that was there; the
  replaced body's teardown releases its audio.
- Rig and Live open their body on first visit; Arrange with no score shows the
  song list.
- A new venue lands in Rig.
- `saved_venues` stores each venue's mode, open score and threads under one
  session item, debounced (`session_store`). A launch reopens the last venue;
  a restored score that no longer exists leaves Arrange on the song list.

Keys: 1 / 2 / 3 switch mode (not while a text field has focus), ⌘P toggles the
song list, ⌘B the agent panel, ⌘, settings.

## 3. Chats

Each body owns its chat (`agent::BodyChat`): the venue page has the venue's,
a score the score's, the Perform set the set's. The agent panel shows the
front body's chat; Arrange with no score open says so instead. A mode whose
chat is working, or finished unseen, carries a status dot on its toggle.

## 4. The song list

The venue's songs as a page over Arrange, laid out as Finder's column view
(`tracks`). The head holds the title and count, the search, Mine/All and Add
songs (add and import). Under it, three columns left to right, with hairlines
(`panel_seam`) between them:

1. **Places** (200 by default): "All songs", then a "Folders" heading over
   the venue's folders, then "New folder". All songs lists every song, filed
   or not; a folder holds links to songs and does not nest, so this is the
   only column of places. Each place shows how many songs the filters admit
   and a chevron; a folder also carries its songs' chat dots. Right-click a
   folder to rename or delete it (delete asks when it holds songs).
2. **Songs** (340 by default) in the selected place: square art the row's
   full height, flush left, then the title over `artist · bpm`, and at the row's end the length over the score count, the
   chat dot and the coverage dot. A press selects a song. Right-click lists
   the folders with checkboxes to file it in.
3. **Scores** of the selected song, filling the rest: this venue's under its
   name, "New score", then each other venue's under that venue's name — the
   score crumb's menu's order. The open score wears a check. A score
   here opens on the spot and closes the page a double-click interval later
   (so a double-click on its name renames it instead); another venue's asks
   "Switch to <venue>?" first, the same question and path as the score crumb.
   This venue's scores take a right-click Delete.

Selecting in one column fills the next, and choosing another place empties
the songs' selection. Every row in every column is one height (36,
`tracks::ROW`) and full column width; hover and selection are plain fills
edge to edge, no corners, inset or ring. The path stays lit: the column with
the keyboard wears the stronger fill (`WASH_EMPHASIS`), the others the
weaker (`WASH_PRESSED`); hover is `WASH_SUBTLE`. Every other row is
striped (`WASH_STRIPE`, under hover), counted from each column's top and
again from each heading under it. Up and Down move in a
column, Right goes into the next, Left steps back (clearing the column it
leaves), Enter opens the score under the cursor; Down in the search goes to
the first match. The selection outlives the page closing, so it reopens on
the folder and song it was left on. The first two columns' seams drag (the
grip's 5px reach over the hairline; double-click resets), and the scores
keep at least 260. A search lists matches from every place, with All songs
selected, and choosing a place ends it.

⌘P and the song crumb open the page; ⌘P and Escape close it.
The list takes the window's width: the agent panel stands aside while it is
up and comes back when it closes, unless it had been hidden by hand; ⌘B over
the list shows it for that visit only.

## 5. Surfaces

Zeron's tokens (`zeron-dark.json`), held in `luma_ui::glass`:

- **The trim** (`window_trim` = `ladder::frame`, `#0e0e0e`) is the grey
  ladder's one rung under the page's ground (`#181818`), by zeron's own step
  from window to page (`#0e0e10` under `#18181b`), kept achromatic like every
  other rung. The title bar and the status bar are drawn on it with no card of
  their own; the page is the one rounded thing on it.
- **Inside the page, panels are flush.** A seam that drags — agent | content,
  stage / body, Live's decks | stage — is a 4px gutter of the trim, so the
  trim runs on into the page along it; the grip over it keeps its own 5px
  reach. The song list's columns are the one exception: Finder's hairlines,
  with grips over them. Panels that do not resize against each other take a 1px hairline
  (`panel_seam`, `#ffffff12`); a hairline inside a panel is `inner_rule`
  (`#ffffff0a`). No panel corners.
- **Corners** belong only to controls (6–8), clips and pads (4) and floating
  things — menus, popovers, settings cards (12). See `luma_ui::radius`.
- **Washes:** hover `WASH_SUBTLE` (`0x0d`), pressed `WASH_PRESSED` (`0x17`),
  selected `WASH_REST` (`0x1a`). A button's plate is the pressed wash and
  lifts to selected on hover. Every one-of-N switch is `float::segmented`:
  a `#ffffff08` track with no edge, the chosen cell at the selected wash —
  both laid over the page's ground once and painted opaque, so the mode
  switch on the trim and a toggle on a panel are the same colours.
- **Muted text** is the primary at 60% (`TEXT_SECONDARY`,
  `ladder::muted_foreground`) and 42% (`TEXT_TERTIARY`).

## 6. Chrome

- **The trail** (title bar, leading, after the agent toggle):
  `venue / song / score`, each crumb its own button with no chevron, the `/`
  muted text between them. A crumb takes the shared hover wash (`nav_row`).
  The song and score crumbs are Arrange's open score; with none, and in Rig
  and Live, the venue stands alone. Settings puts Back in the trail's place.
- **Venue menu** (the venue crumb). The menu holds the venues with their status dots and a
  "Shared with you" mark, New venue…, Join with code…, Share, Settings…, and
  Sign out / Sign in. The venue dialog is still what a window with no venue
  shows (browse, create, join).
- **Song crumb** (`score_switcher`): the song's title. A press opens the song
  list with the song selected and its scores listed — in the place the list
  was on when that place lists it, in All songs otherwise.
- **Score menu** (the score crumb; `score_switcher`): the score's name, or its
  `#n` when it has none. Its menu holds every score of the song — this
  venue's, then each other venue's under the venue's name — the open one
  checked. A score here opens on the spot;
  one in another venue asks "Switch to <venue>?" (`confirm`), and on Switch
  the window switches venue and opens that score in Arrange, in place of the
  score saved for that venue.
- **Mode switch** (title bar, trailing): Rig / Arrange / Live. The keys 1 / 2 / 3 are named in each cell's tooltip, not drawn.
- **Arrange** keeps today's timeline and the strip above it. The strip leads
  with the transport — play / pause, `0:00 / 3:32`, the tempo — then add,
  export and the readouts. Space (`PlayPause`) plays and pauses too, and the
  ruler scrubs. A double-click on a clip opens its graph where the timeline
  was.
- **Live**'s Perform/Setup toggle is an "Edit layout" / "Done" button at the
  end of the page tabs' row, after the page's line (or what is wrong with the
  source followed), and Edit layout arranges the grid only. The decks followed (or the tapped
  tempo) sit in the deck panel beside the stage, and under them the DJ
  source: "Add source" opens the sources and the latency inline; a followed
  source can be changed or disconnected, back to the tapped tempo. The MIDI
  and OSC status sits under it. What is followed is the status bar's line.
- **Settings** is a page, not a dialog: zeron's settings page in place of the
  page's whole content, swapped in one frame — the agent panel is not slid
  away, it is not drawn, and comes back at its width. The title bar keeps the
  window's corner and Back, one gap past the corner where the agent toggle
  sits; no agent toggle, no mode switch, and no room held for them. A 200
  nav: a search field, then the sections, each with a 16px Nucleo icon (page
  titles carry none). The search matches every section's row labels and help
  text; while it holds a query the nav's sections step aside and the column
  lists the matching rows under their section's title, or says nothing
  matched with a way to clear. Beside the nav, a 640 column with a title and
  description over sections, each a small title over a card of 52-high rows
  with hairlines between, each control at its row's end in a 240 slot;
  titles and row words share one 17px inset. Back or Escape returns to what
  was there; Escape empties a search first.

## 7. Escape

One ladder (`shell` `dismiss_overlay`), first match wins: the venue menu, the
score crumb's menu, score and song menus, subagents, the settings page
(an open select first, then a search), visualizer settings, patch rungs, the stage's
own state, an open dialog, then the song list, a sheet menu, the clip graph,
the pattern sheet, and finally the selection.
