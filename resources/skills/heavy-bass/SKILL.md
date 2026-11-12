---
name: heavy-bass
description: Dubstep, riddim, tearout, hard trap — punch music. Use when drops are carried by aggressive bass and the snare hits like a weapon. The bass is the show; measure what it does and light exactly that.
---
# Heavy bass

Use this when the music wants to hurt. Halftime feel, snare on 3, and drops
where the bass is doing violent, articulate things. If the bass at the drop is
smooth and pretty instead, you want `melodic-bass` — same tempo, opposite soul.

Read `reading-music-into-light` first. It is how a sound becomes a look;
this adds what is particular to the style. What follows are the habits of
the genre and what to listen for, not instructions: the track in front of
you decides.

## The one idea

The kick and snare are scaffolding. The event is the bass: its rhythm, its
texture, how fast it moves, when it changes. Two bars with identical drums can
be a slow neck-snap growl or a triplet screech, and lighting them the same is
the most common failure in this genre. So the work is per-phrase: measure the
bass (`luma.music.modulation`, the loop in `finding-things-in-audio`), then
light what you measured.

- Accents go on the **measured bass onsets**, not on eighth notes. Wubs swing,
  drag, and go triplet. Snapping them to the grid is exactly the mistake.
- Deep gated wub → hard on/off. Shallow rolling wub → a breathe, no blackout.
- The fastest strobe belongs to the phrase whose measured rate is actually
  fastest. Save it for that phrase.

## Feel

- Halftime drums: at 140 the snare lands every fourth felt beat, so the
  track is still 140 (`luma.music.feel` says "halftime drums"). A pulse on
  every snare is glacial — the snare gets one hit: short, hard, high contrast.
- Hats and rides do the subdivision. Give them something dim and small. Texture,
  not statement.
- Empty beats stay empty. Space is part of this genre. Don't fill it.

## Reading the intensity

When the snare is artillery (high `level_db` on `luma.music.onsets["snare"]`)
and the bars read intense:

- Think **vertical**. Up/down punches, ceiling-to-floor drops of light, whole-rig
  slams. This music moves heads, and heads move up and down.
- A kick/snare alternation effect — one group answers the kick, another answers
  the snare — turns the drum skeleton into a visible machine. It works exactly
  when the drums are hard enough to deserve it.
- No vertical spread in the rig? Substitute contrast: alternate depth (front
  rig vs back rig) or slam brightness floor-to-ceiling instead of position.

## The drops tell a story

Compare the drops before scoring any of them (spectral character, wub rate and
depth from `modulation`, `similar(mode="sound")`). What you're looking for:

- **The second drop turns on the first.** One is melodic or controlled, the
  other suddenly noisy and distorted, flatness way up, the rate faster or
  uglier. The sound changed character, so the light does: the size of the
  change you measured is the size of the change you make, in palette, edge
  and level. What it becomes comes from what the new bass is doing.
- **The drops are twins.** The music repeats itself, so a quiet variation is
  honest: move the same idea to other lights, turn it around, or answer the
  gaps where it answered the hits. A copy is a missed chance; a new show is
  a lie.
- **The second drop is smaller** (a strip-back, a vocal drop). Sparser and
  darker than the first, on fewer lights. Less, not the same thing dimmer.

## Builds and the bar before

Ride the riser: one group pulsing, rate tightening — quarter, eighth,
sixteenth — brightness terracing up, timed to where the riser actually peaks
in the audio. Follow the fill that leads in: a snare roll or a stutter is
detail worth placing, small and on few lights. If the music then empties out
before the drop, the room empties with it, and that near-dark is what makes
the drop land. And check the drop is real before you
spend all this: `listen` the first bars after the build for sub and low at
full level — a feint gets a dry cut, and the real drop gets the payload.

## Listening

Depth: maximum. Intricate detail that mirrors the audio is the product here.

- Listen for: the growl's rate and every rate switch, scoops (pitch or
  filter? check), womps per bar, cuts where the bass drops out for a 16th, and
  what the second half of a 2-bar phrase changes.
- Detail: 16th resolution on every drop phrase. Run the full loop, the
  skeptic pass, and parallel listeners (bass, drums and cuts, vocals,
  transitions) on each distinct drop phrase.
- LDs love: a light that switches rate exactly when the LFO does, and a
  blackout on the one 16th the bass cuts.

To design, map sections and fix palette and group roles yourself, then give
each child one drop phrase with the bass evidence rows and your arc decisions
in its brief. Stitch, walk the seams, and confirm the drops outrank everything
else on the heatmap.
