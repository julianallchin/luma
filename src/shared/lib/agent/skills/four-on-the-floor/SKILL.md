---
name: four-on-the-floor
description: Scoring house, techno, and trance — anything with a kick on every beat. Use when the grid is steady and structure is phrase-of-8/16/32. Covers intensity terracing, hat-driven motion, and the breakdown-riser-drop cycle.
---
# Four-on-the-floor (house / techno / trance)

## The thesis

Here the grid carries you. A kick on every beat and a phrase structure that almost never
breaks means the beat grid, the downbeats, and the per-bar tags are a nearly complete
score sketch on their own. The craft is not detection — it is **restraint and terracing**:
deciding which of the 8/16/32-bar blocks gets more than the one before it, and holding
back so the peaks have somewhere to go.

Contrast this with bass music, where the drop's rhythm has to be measured out of the
audio. In four-on-the-floor, if you find yourself doing heavy signal analysis to decide
where the accents go, you have probably taken a wrong turn. Confirm the kick is on every
beat (`features.drum_onsets["kick"]` against `features.beats`, or the `four_four` tag —
F1 0.86, one of the most reliable in the set) and then work structurally.

## Structure

Phrases are 8, 16, or 32 bars and they line up. Get the phrase length once, from the
whole track, and then treat phrase boundaries as the only places your lighting state is
allowed to change wholesale:

- Diff `features.bars.intensity` between consecutive bars and look at where the large
  jumps fall. They will land on a consistent multiple — that multiple is your phrase.
- Sanity-check it against `features.bars.predictions` for `kick` (drops out in
  breakdowns), `riser`, and `build`.

Then read the track as a sequence of blocks: intro → build → drop/main → breakdown →
riser → drop → outro. Nearly every track in these genres is some arrangement of that.

## Terracing

Assign each block a level and hold it. `features.bars.intensity` (continuous 0-5, not a
probability) is your ladder — quantize it into 3 or 4 levels and map those to brightness
and to how many groups are active, rather than mapping intensity continuously. Continuous
mapping reads as drifting; discrete steps read as arrangement.

Rules that hold across the genre:

- Each drop should be at least one step above every preceding block.
- The second drop is the peak; do not spend the top of your range on the first one.
- A breakdown may go as low as one dim group. Silence in the lighting is legitimate.
- Never terrace *down* mid-block. Changes land on the phrase line.

## Motion

The kick is not the interesting rhythm — it is constant, and lighting every kick for six
minutes is hypnotic for one minute and numbing for five. Use it sparingly, for a bass
group at low brightness, or for the first bar of a phrase only.

**Hats carry motion.** `features.drum_onsets["hat"]` gives you the offbeat/sixteenth
layer, and its density is what actually changes across a track. Put your fastest, lightest
element there. When the `hats` tag turns on in a bar that previously lacked it, that is
the arrangement telling you to lift.

Give the chord/lead layer a slow counter-motion: a wide sweep or color drift over 8 or 16
bars, phase-locked to the phrase rather than the beat. `features.chords` (with
`starts_s` / `labels`) is useful in trance and melodic house — a chord change on the
phrase line is a good place to shift hue.

## Breakdown → riser → drop

This is the one moment that needs real attention, and it is the same shape every time:

1. **Breakdown.** Strip to one or two groups. Long fades. If `kick` drops out of the
   tags, drop the kick-linked element too — the room should notice the floor disappear.
2. **Riser.** Find the run of `riser` / `build` bars. Accelerate a pulse (quarter →
   eighth → sixteenth) and terrace brightness up over the run. In trance this run can be
   16 or 32 bars; pace it so it is still climbing at the end.
3. **The last bar.** Cut to near-black for one bar, or hold a single sustained white. Both
   work; a busy last bar does not.
4. **Drop.** Everything on, on the downbeat, then settle within a bar or two into the
   block's held state. The impact is one moment, not a section.

Note `impact` (F1 0.20) and `fill` (F1 0.38) are unreliable tags. Use the intensity jump
at the phrase line to place the drop; use `impact` only as a hint to go look.

## Genre inflections

- **House.** Warmer, less extreme dynamic range. Swing on the hats. Keep the whole track
  within a narrower brightness band and let color do more of the work.
- **Techno.** Monochrome, mechanical, longer blocks. Change less than you think. A single
  strobe element introduced at minute five is a bigger event than anything you could do
  earlier.
- **Trance.** Widest range, longest breakdowns, most melodic. Follow the chord progression
  with color; the emotional peak is often the breakdown's melody, not the drop.

## Delegation

Mostly unnecessary. The bar tags plus the phrase grid give you enough to score the whole
track in one coherent pass, and a single pass is what keeps the terracing consistent —
which is the entire point of the genre. Work section by section within one thread, and
apply a checked batch per block.

Delegate only if the track is unusually long (10+ minutes) or is a genuine multi-part
arrangement with distinct musical identities. In that case split on those identities, not
on drops, and re-check the terracing across the whole track after the children merge:
independent children will each use their own full brightness range, which flattens the
arc.
