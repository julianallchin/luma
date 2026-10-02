# Venue tabs

**Status:** approved by Julian 2026-10-01; phases 1 and 2 built. This replaces the
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
7. **The sidebar is a launcher.** It shows the venue row ("Venue setup"), the
   venue's folders, and all its songs with their scores. A click opens that
   tab, or brings it to the front. The sidebar no longer owns tab sets. A click on a score
   puts the sidebar away after about one double-click interval (400 ms), so
   a double-click on the score's name still reaches the row and renames it.
   The rename cancels the hide.
8. **Tabs come back at launch.** The app reopens the last venue, every
   venue's open tabs in strip order, the front tab of each venue, and the
   chat each tab had open. This is stored on this device only (a local
   session item, not synced) and saved on every change, debounced. A tab
   whose score, track or venue no longer exists is dropped without a word.
9. **The `+` is a combo box.** ⌘T presses it. It lists "Venue" and every
   song in the venue. Typing filters: "Venue" by its word or the venue's
   name, a song by title, artist or album (as the add-track dialog does).
   ↑/↓ move, ↵ or → picks, ← or ⌫ on an empty filter goes back one level,
   Escape closes. A song goes one level in, to its scores in this venue,
   also when it has only one. A score or "Venue" opens that tab, or brings
   it to the front, and closes the box. The `+` shows with no tabs too; the
   empty panel only points at it.

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

- **Phase 1 (now):** rules 1–9 and the layout. "Needs input" (rule 6) waits
  for a turn event that asks the reader something; no such event exists yet.
  The planned local migration for the `pattern_graph` route is not needed:
  `20260912000000_row_model.sql` already dropped the agent thread route
  triggers.
- **Phase 2 (built 2026-10-01): folders.** A venue has folders. A folder
  has a name and holds links to songs, many to many; a song can be in no
  folder. Scores belong to the song (track + venue), so a song in two
  folders shows the same scores in both. Deleting a folder deletes only its
  links.
  - **Data.** `folders (id, uid, venue_id, name)` and
    `folder_tracks (folder_id, track_id, uid, venue_id)`, id
    `folder_id:track_id`. Venue content: owner and members read and write
    (`can_access_venue`), the server keeps their history, and the `venue`
    sync stream carries both. Locally `folder_tracks.track_id` has no
    foreign key: a member receives every link in the venue but only the
    tracks behind its scores. Migrations
    `20261001000000_folders.sql` (local and Supabase); the one-time data
    step gave every venue a "Testing" folder holding every song it had.
  - **Sidebar.** Under "Venue setup": each folder (a press opens or closes
    it, showing its songs), "New folder", then "All songs". A song row in a
    folder opens the same scores level as under All songs. A closed folder
    shows the strongest status dot of its songs' tabs. A right-click on a
    folder offers Rename and Delete folder (it asks first when the folder
    holds songs). A right-click on a song lists the folders with
    checkboxes. Code: `gpui/crates/app/src/tracks/folders.rs`.
  - **Not yet.** Picking folders in the add-track dialog. Escape does not
    close the folder and song menus yet: they need a rung in
    `Luma::dismiss_overlay`. A song's scores open on the scores level, not
    inline under the song.

Mockup: `tabs-mockup.html` from the 2026-10-01 session (throwaway).
