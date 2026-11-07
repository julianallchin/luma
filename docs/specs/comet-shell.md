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
 ●●● ◧  ▭ Venue ▾           Aurora #1 ▾            [Rig|Arrange|Live]
 ╭────────────┬─────────────────────────────────────────────────────╮
 │         ↶+ │  stage (the venue lit by the mode's body)           │
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
| Title bar | 44 high, on the trim across the whole window: traffic lights, agent toggle, venue menu, the mode's subject, the mode switch | `shell::title_bar`, `chrome` |
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

Today's track browser as a page over Arrange: search, Mine/All, folders, add
and import. A row expands in place to its scores in this venue and "New
score"; choosing a score opens it and closes the page a double-click interval
later (so a double-click on a score's name renames it instead). The title
bar's song title opens and closes the page; Escape closes it. The list takes
the window's width: the agent panel stands aside while it is up and comes
back when it closes, unless it had been hidden by hand; ⌘B over the list
shows it for that visit only.

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
  reach. Panels that do not resize against each other take a 1px hairline
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

- **Venue menu** (title bar, leading): a muted stage glyph, the venue's name and a chevron. The menu holds the venues with their status dots and a
  "Shared with you" mark, New venue…, Join with code…, Share, Settings…, and
  Sign out / Sign in. The venue dialog is still what a window with no venue
  shows (browse, create, join).
- **Subject** (title bar, centre): in Arrange the song title, which opens the
  song list; in Live the follow line.
- **Mode switch** (title bar, trailing): Rig / Arrange / Live. The keys 1 / 2 / 3 are named in each cell's tooltip, not drawn.
- **Arrange** keeps today's timeline and the strip above it. The strip leads
  with the transport — play / pause, `0:00 / 3:32`, the tempo — then add,
  export and the readouts. Space (`PlayPause`) plays and pauses too, and the
  ruler scrubs. A double-click on a clip opens its graph where the timeline
  was.
- **Live**'s Perform/Setup toggle is an "Edit layout" / "Done" button. The
  decks followed (or the tapped tempo) sit in the deck panel beside the
  stage; what is followed is the title bar's line, said once.
- **Settings** is zeron's settings page inside the dialog: a nav, then a 640
  column with a title and description over sections of 52-high rows, each
  control at its row's end in a 240 slot.

## 7. Escape

One ladder (`shell` `dismiss_overlay`), first match wins: the venue menu,
score and song menus, subagents, visualizer settings, patch rungs, the stage's
own state, an open dialog, then the song list, a sheet menu, the clip graph,
the pattern sheet, and finally the selection.
