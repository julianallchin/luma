# Venue tabs

**Status:** approved by Julian 2026-10-01, phase 1 in build. This replaces the
region model of `comet-shell.md` §0 and §2 where they disagree.

## Rules

1. **A venue is a project.** Each venue has its own tab set. Switching venue
   parks the whole set and brings back that venue's set as it was.
2. **A tab is a score or the venue.** Tab key:
   `Score { venue, track, score }` or `Venue { venue }`. Many scores can be
   open at once, also two scores of the same track. One venue tab per venue.
3. **The chat lives inside the tab.** Each tab has its own chat panel and
   remembers which chat it has open, its scroll and its draft. Switching tabs
   never stops a running turn.
4. **Chats belong to a score or a venue.** A score tab's chats are that
   score's chats. The venue tab's chats are venue chats (`venue_rig`, already
   supported by the backend). Pattern chats are gone.
5. **The chat list follows the tab.** "Chats" lists the chats of the tab's
   score, or of the venue for the venue tab.
6. **Status dots.** A tab shows a dot while its agent is working, when it
   needs input, and when it finished while the tab was not in front. The same
   dot shows on the tab's sidebar row and, for other venues, in the venue
   picker. Viewing a finished tab clears its dot. Answering clears "needs
   input".
7. **The sidebar is a launcher.** It shows the venue row ("Venue setup") and
   the venue's songs with their scores. A click opens that tab, or brings it
   to the front. The sidebar no longer owns tab sets.
8. **Tabs come back at launch.** The app reopens the last venue, every
   venue's open tabs in strip order, the front tab of each venue, and the
   chat each tab had open. This is stored on this device only (a local
   session item, not synced) and saved on every change, debounced. A tab
   whose score, track or venue no longer exists is dropped without a word.

## Layout

```text
┌──────────┬───────────────────────────────────────────────────────┐
│ venue ▾  │ [Strobe · Main ●][Strobe · Alt][Opus · Main][⌂ Venue] │
│          ├───────────────────┬───────────────────────────────────┤
│ ⌂ Venue  │  chat (this tab)  │  editor (this tab)                │
│ songs    │                   │  track editor, or the patch page  │
│  scores  │  composer         │                                   │
└──────────┴───────────────────┴───────────────────────────────────┘
```

The chat stays left of the editor, as today. The tab strip spans the chat and
the editor, because both belong to the tab.

## Phases

- **Phase 1 (now):** rules 1–8 and the layout. "Needs input" (rule 6) waits
  for a turn event that asks the reader something; no such event exists yet.
  The planned local migration for the `pattern_graph` route is not needed:
  `20260912000000_row_model.sql` already dropped the agent thread route
  triggers.
- **Phase 2 (queued, needs Julian's OK before any Supabase apply):** folders.
  A venue has folders; a folder holds links to songs (many to many). Add a
  song, then pick folders. A song in many folders shows the same scores in
  each. Needs a synced table on Supabase and PowerSync.

Mockup: `tabs-mockup.html` from the 2026-10-01 session (throwaway).
