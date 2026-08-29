# Light arena

A public benchmark for AI-authored lighting. An agent authors a full show for a
(track, venue) pair; visitors watch two anonymous renders of the *same* track in
the *same* venue and pick one. Votes fit a Bradley–Terry model with a per-voter
reliability weight, and the result is a leaderboard over authoring systems
(model + harness), not over tracks.

Ratings attach to a **harness version**, not just a model name. A harness
version is the tuple (authoring prompt, skills bundle, MCP tool catalog) and is
stamped into every score row — a prompt edit or a new tool is a new entry, and
old scores are not silently inherited.

## Decisions

- **~200 tracks**, selected from the real Luma library by
  `scripts/arena/select_tracks.ts` — genre mix plus boring filters (duration
  120–480s, analysis complete, ≤2 tracks per artist, one title per remix
  family). Genre comes from the existing per-bar Discogs-EffNet rows; there is
  deliberately no structure or energy analysis in the selector.
- **~5 venues**, fixed for the season. Same-venue pairing is what makes votes
  comparable: rigs differ enough that a cross-venue pair measures the rig.
- **20% hold-out**, stratified within genre, flagged in `arena/tracks.csv` and
  never served publicly. It exists so a system tuned against the public pool can
  still be measured on tracks it never saw.
- **Authoring is full-track.** The agent gets the whole track and writes a whole
  show; nothing about the clip boundary is visible to it. Clipping only ever
  happens downstream.
- **Render full, cut on demand.** Each show renders once end-to-end; the serving
  layer cuts a 10–20s clip at request time, chosen structurally (a drop, a
  build) from the bar classifications already in the library. Re-cutting is free,
  re-rendering is not.
- **Audio is commercial, under a fair-use posture**: short clips at a structural
  boundary, framed as benchmark evidence rather than listening, on a site that is
  not a music player and offers no seek, download, or full-track playback. Every
  clip carries artist/title attribution and the site carries a takedown form with
  a named contact; a takedown pulls the track from the pool and voids its votes.

## Pipeline

    select  →  author  →  render  →  cut
    (this   )  (parallel processes over          (one full   (10–20s at
    (script )  scripts/headless/author_score.ts)  render per  serve time)
                                                  show)

`author_score.ts` already runs one authoring session headlessly. The arena driver
fans it out across (track, venue, system) triples, bounded by per-plan usage
limits rather than by CPU — authoring is the slow, rate-limited stage; rendering
is the expensive-but-schedulable one.

## Outputs

`arena/tracks.csv` — one row per selected track: id, title, artist, duration,
bpm, primary genre, top-3 genre mix, source playlist, hold-out flag.
`arena/manifest.json` — selection params, seed, pool and eligible counts,
per-genre quota/selected/hold-out, library hash and mtime. Both are deterministic
given the same library and parameters, so a regenerated pool either matches
byte-for-byte or the library changed underneath it.

## Genre notes (from the first real run)

The classifier's per-track argmax is the bucket key, and it collapses some
things: `Hard Techno` is a separate Discogs style from `Techno` but only 6 tracks
land there, and UK Garage (7) splits against Bassline (40) — most of the UKG
playlist reads as Bassline. Buckets under `--min-per-genre` fold into a single
`other` bucket that receives one equal share; with the current library that is
77 eligible tracks competing for ~24 seats, which is the coarsest part of the
selection and the first thing to revisit if the pool feels lopsided.

## Open

- **Driver / queue.** Nothing schedules the (track, venue, system) fan-out yet:
  no retry story, no resumption, no place the per-plan usage gate lives.
- **Venue set.** Five venues, but not which five. They need to differ in rig
  class (small bar / club / festival stage), not just in fixture count.
- **Render bitrate and format.** Clip quality is a confound if it varies between
  systems; the encode settings must be fixed per season and stamped alongside the
  harness version.
- **Vote surface.** Per-voter reliability needs seeded pairs with a known answer,
  and none are defined.
