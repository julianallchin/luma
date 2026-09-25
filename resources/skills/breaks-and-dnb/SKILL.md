---
name: breaks-and-dnb
description: Drum & bass, jungle, breakbeat, UK garage — fast broken drums. Use when the drums are the melody. Ride the break, not the bar; the sub is the floor, the break is the ceiling.
---
# Breaks & DnB

Fast music built on broken drums — 160–180 for dnb and jungle, 130ish swung
for UKG and breaks. The drum pattern isn't timekeeping here, it *is* the
music. Lighting that ignores the break and pulses the bar line misses the
entire genre.

## The one idea

Ride the break. The kick-snare placement inside the bar is a melody, and it
repeats with variations — so learn it, then assign it. Pull the actual kick
and snare onsets (`features.drum_onsets`), look at one loop of the pattern,
and give its two or three signature hits a consistent lighting answer that
recurs every bar the way the break does. Recurrence is what makes it read as
the music and not as flicker.

- At 174 BPM you cannot light every hit — you'd strobe the room into paste.
  Choose the skeleton hits (usually the downbeat kick and the mid-bar snare)
  and let the ghost notes live in a dim shimmer underneath.
- The sub-bass is the floor: give it a foundation that holds or slowly
  breathes. Two-layer physics — steady low glow under fast top activity — is
  the genre's whole visual.

## Reading the subgenre

- **Liquid / rollers** — smooth, jazzy, vocal. The break is soft-edged; keep
  the accents warm and rounded, let pads and vocals drive color, moderate
  contrast. Closer to melodic-bass in spirit at double speed.
- **Jump-up / neuro** — the bass talks (`luma.music.modulation` works at
  these rates too). Bass phrases get
  answered like dubstep wubs: accents on measured onsets, harder contrast,
  colder colors for neuro's mechanical growl.
- **Jungle** — chopped breaks, chaos with a smile. The edits themselves (break
  switches, stutters) are events worth accenting; find them with
  `luma.music.deviations`.
- **UKG / breaks** — swung, flirty, club-scale not festival-scale. Offbeat
  accents, restrained ceiling, groove circulation like tech house.

## Structure

DnB arrangements move fast — 16-bar sections, drops every minute. Terrace like
four-on-the-floor but with shorter blocks, and protect contrast: the double
drop (two basslines at once, or the drop after a one-bar cut) is the genre's
face-punch moment and deserves the reserved move. Breakdowns in liquid can be
long and gorgeous — treat them with melodic-bass patience.

## Listening

Depth: moderate for liquid and UKG, deep for jump-up, neuro and jungle.

- Listen for: the break's kick and snare placement over one loop, where it
  changes (edits, stutters, switches), the sub's motion, and in neuro the
  bass's rate switches.
- Detail: one careful loop analysis of the break at 16th resolution, reused
  across the track; `deviations` finds every bar that edits it. Ghost notes
  and hats are low-confidence in n2n. For neuro and jump-up drops, run the
  skeptic pass and parallel listeners as in `heavy-bass`.
- LDs love: the same light answer on the break's signature hits every bar,
  and a hard change on the break switch.

One pass for the arc. Delegate only where the track diverges in character,
split along those seams, with the bass evidence rows in each child's brief.
