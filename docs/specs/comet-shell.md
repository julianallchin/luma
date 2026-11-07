# The shell

**Status:** built. This is the contract for the `gpui/crates/app` shell.
Section numbers are cited from code; keep them stable.
**References:** comet (motion, the agent panel) and zeron (`apps/zgx/ui/ds`:
the flush-panel tokens and the settings page).

---

## 0. Overview

The window is one venue, shown in one of three modes. The agent panel sits
beside the mode, a title bar runs over it, and a status bar runs under
everything:

```text
┌────────────┬──────────────────────────────────────────────────────┐
│ ◧        ↶+│ Venue ▾         Aurora #1 ▾        [Rig 1|Arrange 2|Live 3] │
│            ├──────────────────────────────────────────────────────┤
│  agent     │  stage (the venue lit by the mode's body)            │
│  panel     ├──────────────────────────────────────────────────────┤
│  (the      │  the mode's body: the venue page, the score,         │
│  body's    │  or the Perform set — or the song list over Arrange  │
│  chat)     │                                                      │
├────────────┴──────────────────────────────────────────────────────┤
│ sync · DMX output · import · update                  context line │
└───────────────────────────────────────────────────────────────────┘
```

Nothing is destroyed to show something else: each mode keeps its body while
another mode is up, and each venue keeps its modes while another venue is up.

## 1. Regions

| Region | What it is | Code |
|---|---|---|
| Agent panel | the front body's chat; 300 min, a quarter of the window by default, drag-resizable, ⌘B hides it | `shell::agent_panel`, `agent_split` |
| Title bar | 44 high: venue menu, the mode's subject, the mode switch | `shell::title_bar`, `chrome` |
| Stage | the venue, lit by the mode's body; collapsible, height-resizable | `shell::stage_body` |
| Body | the mode's body, or the song list in Arrange | `shell::content` |
| Status bar | 24 high, cached; sync, DMX output, import, update, account failure, and the mode's context line | `status_bar` |

When the agent panel is up, the window's corner (traffic lights and the agent
toggle) sits over the panel's head and the title bar starts where the panel
ends. When it is hidden the title bar spans the window and leaves room for the
corner at its leading edge. The corner never moves.

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
bar's song title opens and closes the page; Escape closes it.

## 5. Surfaces

Zeron's tokens (`zeron-dark.json`), held in `luma_ui::glass`:

- **Panels are flush.** They touch, a 1px hairline between them
  (`panel_seam`, `#ffffff12`); a hairline inside a panel is `inner_rule`
  (`#ffffff0a`). No gaps between panels and no panel corners.
- **Corners** belong only to controls (6–8), clips and pads (4) and floating
  things — menus, popovers, settings cards (12). See `luma_ui::radius`.
- **Washes:** hover `WASH_SUBTLE` (`0x0d`), pressed `WASH_PRESSED` (`0x17`),
  selected `WASH_REST` (`0x1a`). A button's plate is the pressed wash and
  lifts to selected on hover. The segmented control is a `#ffffff08` track
  with no edge; the chosen cell takes the selected wash.
- **Muted text** is the primary at 60% (`TEXT_SECONDARY`,
  `ladder::muted_foreground`) and 42% (`TEXT_TERTIARY`).

## 6. Chrome

- **Venue menu** (title bar, leading): the venues with their status dots and a
  "Shared with you" mark, New venue…, Join with code…, Share, Settings…, and
  Sign out / Sign in. The venue dialog is still what a window with no venue
  shows (browse, create, join).
- **Subject** (title bar, centre): in Arrange the song title, which opens the
  song list; in Live the follow line.
- **Mode switch** (title bar, trailing): Rig / Arrange / Live with their keys.
- **Arrange** keeps today's timeline. Play, position and tempo sit in the
  audio lane's header over the waveform; export stays on the strip above the
  timeline. A double-click on a clip opens its graph where the timeline was.
- **Live**'s Perform/Setup toggle is an "Edit layout" / "Done" button.
- **Settings** is zeron's settings page inside the dialog: a nav, then a 640
  column with a title and description over sections of 52-high rows, each
  control at its row's end in a 240 slot.

## 7. Escape

One ladder (`shell` `dismiss_overlay`), first match wins: the venue menu,
score and song menus, subagents, visualizer settings, patch rungs, the stage's
own state, an open dialog, then the song list, a sheet menu, the clip graph,
the pattern sheet, and finally the selection.
