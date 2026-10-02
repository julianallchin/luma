# Luma: Clip Graph Autoencoder and Placement Transformer

2026-10-02 · Julian. Live doc with the architecture diagram:
https://claude.ai/code/artifact/5d04a37d-a34d-4902-9937-4bc864ab0d9f

## Overview

Two models turn a track into a Luma score. A clip graph autoencoder learns a latent space of rendered effects. A placement transformer writes the show as a time-ordered sequence of clips, and each clip carries an effect latent that the graph decoder turns into a v3 graph.

- **Inputs:** full-track audio, the beat grid, and the rig.
- **Output:** clip rows as Luma stores them: start, duration, layer, blend mode, selection and graph.
- **Training order:** the graph autoencoder is trained first, on LLM-generated graphs, then frozen while the placement transformer trains.

The split follows the data. Effects are cheap to generate in bulk. Placements are scarce, so the placement model only has to learn where effects go, not how to build them.

The graph encoder and decoder are trained first on generated graphs. The placement transformer then predicts effect latents, and the frozen decoder turns each one into a graph.

## Clip graph autoencoder

The latent encodes what a clip looks like, not how its graph is written. Graphs that render the same share one latent, and the decoder always writes one canonical graph for it.

### What "the same effect" means

- Lower each graph with lower.rs and render it on a fixed probe set:
  - Canonical rigs: line, grid, ring, 3D cluster.
  - Clip lengths of 1, 4 and 16 beats.
  - A few audio inputs, for graphs with audio nodes.
- Color and strobe clips render as per-head color, brightness, rate and alpha over time.
- Aim clips render as per-head beam direction over time, or beam hit points on a plane above the audience. Raw yaw and pitch values are never compared, because equivalent graphs use different numbers.
- Two graphs are the same effect only if they match on every probe. This separates cases that look equal on one clip but not another: `time()` stretches with the clip and a fixed delay does not; a wrapped scale tiles on a long rig and curve-point width does not.

### Encoder

- A transformer over node tokens. Each token carries the node kind, its settings, and pointers to its inputs.
- It accepts any equivalent form of a graph. Equivalent rewrites are used as input augmentation, so the encoder learns to ignore how a graph is written.
- A contrastive loss pulls render-equivalent graphs together and pushes different renders apart.

### Canonical form

Every effect has one canonical graph, and the decoder only ever learns that form.

- Fewest nodes, constants folded in, no pass-through math.
- Inputs sorted in a fixed order, node ids assigned in topological order.
- Fixed preferences between equivalent forms:
  - Width in curve points over a wrapped scale.
  - `time()` over a fixed delay, unless a fixed length is intended.
  - For aim, `direction` holds the pose all heads share at τ = 0, and yaw/pitch hold the per-head spread and all motion. A shared pitch moves into it only when yaw is 0 on every head.
- The canonicalizer applies rewrite rules, then searches for the smallest render-equivalent graph where the rules don't decide.
- Every rewrite is accepted only if it renders the same on all probes, within a fixed tolerance per channel. Float renders never match bit for bit.

**Aim example.** "Direction low, pitch 0 → +60" and "direction high, pitch −60 → 0" both canonicalize to direction low, pitch 0 → +60. This fold is exact. `aim::offset` turns yaw in the beam's own frame, then tilts pitch toward that frame's up. With yaw 0, pitch stays in one vertical plane, so pitches add. It breaks only if the beam passes straight up or down. With Beam fan (yaw −35 to +35 across heads) the fold is not exact: pitch tilts toward the base direction's up, not each yawed beam's up. So a shared pitch stays in the pitch curve when yaw varies.

### Decoder

- Autoregressive, one node at a time in canonical topological order, ending with a stop token.
- Each node predicts its kind, its settings, and its inputs as pointers to earlier nodes or constants.
- Masks enforce the v3 rules at each step: a coordinate never feeds a number input, at most one clock per node's inputs, exactly one output node, at most 64 nodes.
- Graphs are mostly 3–12 nodes, so decoding is a few steps per clip, batched across clips.

| Value type | Examples | Head |
| --- | --- | --- |
| Musical timing | `every`, `duration`, `delay`, `phase` | Classification over a beat grid: 1/16 … 32, triplets, dotted |
| Discrete settings | space kind, ease, op, wrap, split by | Classification |
| Structured fractions | `scale`, `shift`, `at`, curve point x | Grid class, plus a residual where needed |
| Continuous | OKLab colors, directions, low/high, noise speed | Regression, checked by render |

Timing must be exact. RXGER CLUB! already shows the failure: "Triplits 63" uses `every = 0.3333` with `duration = 0.333333`, while "Stutter climb" uses the full float. A grid class makes it exactly 1/3.

### Losses

- Structure and grid values: cross-entropy against the canonical graph, with teacher forcing.
- Continuous settings: a render loss through a differentiable torch port of lower.rs, plus a small regression term.
- Latent: the contrastive render-equivalence loss.

### Training data

- 20k–50k distinct canonical clips, authored by LLMs through the Python builders.
- Balanced across color, aim and strobe, across node kinds, and across graph sizes. Aim needs to be over-sampled.
- Equivalent rewrites of each clip as extra encoder inputs.
- Settings and curve points perturbed for variety; a perturbed graph is kept only if it lowers.
- Deduplicated by render, not by JSON.
- The vocabulary has all 14 node kinds, `value` included (20 uses in the RXGER CLUB! export).

## Placement transformer

One transformer writes the show clip by clip, in start order. Only the clip sequence is causal. The model cross-attends to the whole track's audio and to the rig, so it can see a drop coming while it places the build.

### What placement looks like in RXGER CLUB!

- 187 clips, of which 74 are exact time-shifted copies of an earlier clip.
- Repeats come every 8 beats (34 times), 16 beats (20), 1 beat (8), 7 beats (6) and 32 beats (5).
- 169 clips start on the beat; the other 18 start on 8th or 16th notes.
- Three kinds of clips: beds of 16–44 beats, phrase patterns of 1–8 beats, and one-off hits of a quarter beat to 1 beat.
- 48 start times hold more than one clip. Circle, Circle trough and Strobe on spots_upper always start together.
- Layers run z 0–4. Blend follows the role: aim and strobe use replace, color uses max, masks use multiply, and screen appears only for the slash overlap.

### Inputs

- **Audio:** a pretrained music encoder over the full track, with weights licensed for shipping (see Open questions). Every audio token carries its bar and beat position.
- **Beat grid:** the same bar and beat positions are shared by audio tokens and clip starts, so a clip can find "now" and "8 bars from now" in the audio.
- **Rig:**
  - One token per head or fixture: position, orientation, fixture type, capabilities, head count and group.
  - One token each for the audience, the DJ or band, and the stage box.
  - Group tokens pooled from their heads.
  - Per-rig annotations from a frontier model, computed once offline: group roles, symmetry pairs and mirror axes, depth layers, which groups contain which, build orders, and accent versus background groups.
- Nothing else goes in at inference. No drum, section or drop features.

### Output sequence

Clips are written sorted by start, then by layer. Each step is either a full clip or a copy.

| Field | Full clip | Copy |
| --- | --- | --- |
| Start | Bar plus 16th-note position | Bar plus 16th-note position |
| Source | — | Pointer to an earlier clip |
| Duration | Grid class, or "until track end" | Inherited, or overridden |
| Layer | Raw z index, 0–4, as a class | Inherited |
| Blend | Class, conditioned on z and output kind | Inherited |
| Selection | Pointers to group tokens, a union when several | Inherited |
| Effect | Effect latent, decoded by the graph decoder | Inherited |

- A full clip is about 8–10 tokens and a copy about 2–3. RXGER CLUB! comes to roughly 1,100–1,300 tokens.
- A copy is its own decision at its own time. The model can skip one, shorten one or change it. That is how the one-beat gaps in the circles are represented.
- Motif reuse needs no planner. The model attends to earlier clips and copies or varies them, which is how drop 2 develops drop 1.
- The effect latent is predicted with a multimodal head, a mixture or a learned codebook, so the model commits to one effect instead of averaging two.
- Clip ends that were dragged to the track's end (2.90, 15.97 and 43.97 beats in RXGER CLUB!) become "until track end", so every duration is on the grid.

### Auxiliary losses

- Extra heads on the audio representation predict drum onsets, TANGO labels, drop positions and section boundaries.
- Luma's existing models label any track for free, so these heads pretrain on thousands of unscored tracks.
- The heads are dropped at inference. The model still takes only audio, beat grid and rig.

### Decoding

- Sampling with temperature, masked so starts stay on the grid, selections point at real groups, and copies point at clips that exist.
- Each effect latent goes through the graph decoder to a canonical v3 graph.
- Generation runs offline per track once the file is available. With Pro DJ Link that is when the track loads on a deck.

### Size

- About 50–150M parameters for the clip decoder, on top of the frozen audio encoder and the frozen graph autoencoder.

## Training data

Placement data is shows Claude authors in Luma's harness across 20 generated rigs, plus Julian's own scores. Concert video and a learned reward model are not used.

| Source | Amount | What it teaches |
| --- | --- | --- |
| LLM-authored clip graphs | 20k–50k canonical clips | The effect space, for the graph autoencoder |
| Claude-authored shows | 300–1,000 tracks × 20 rigs = 6,000–20,000 shows, about 7–25M clip tokens | Format, timing against audio, rig adaptation |
| Julian's scores | 50–100, weighted up or used for a final fine-tune | The target style |
| Unscored tracks | A few thousand, auto-labeled by Luma's models | Audio auxiliary heads |

### Harness for authoring shows

- Claude gets the track's audio features, the beat grid and the rig, and writes clips through the Python builders.
- Render feedback per bar shows what each group actually did, so Claude can check and revise its own show.
- Julian's scores sit in context as reference. RXGER CLUB! is the example of what good looks like.

### The 20 rigs

- Generated to spread across size, fixture mix (movers-heavy, pixel-heavy, no movers), geometry (line, grid, truss, towers, in-the-round, asymmetric), audience position, and whether the DJ is lit.
- Real rigs stay out of training and are used only for testing.

### Transfer across rigs

- Each track is authored once in full, then moved to the other 19 rigs.
- Timing, sections, instrument mapping, palette, gaps and fills carry over.
- Selections, directions, mirror axes, centres, brightness and density are adapted per rig.
- Where a motif can't exist (no movers means no circles, no towers means no tower-by-tower build), Claude designs a substitute. Those adaptation decisions are kept; they are the signal for unseen rigs.

### Augmentation

- Left/right mirroring of the rig, with slash and backslash swapped.
- Shuffled group order, so the model learns roles rather than slot positions.
- Coverage of at least 5 genres with roughly 100 or more tracks each, and a mix of structures: long breakdowns, halftime, double drops.

## Evaluation

Both models are judged on rendered output against Julian's scores, with real rigs held out.

### Graph autoencoder

- Decoded graphs render the same as their source on every probe.
- Every decoded graph lowers without error.
- Timing values come back exact: 1/3 is 1/3, not 0.3333.
- Render-equivalent pairs sit much closer in the latent than pairs that render differently. If they don't, the latent is still encoding how graphs are written.

### Placement transformer

- **Timing:** share of clip starts on the grid, and share of hit clips landing on the drum onsets they follow.
- **Structure:** measured on the render, from the habits in RXGER CLUB!:
  - Palette size, and the bar where the accent color first appears relative to the first drop.
  - Brightness range per section, and whether peak brightness lines up with drops.
  - How consistently each fixture type keeps one job across the track.
  - Whether selection widens and rhythm speeds up through each build.
  - Whether gaps or holds land right before phrase boundaries and drops.
  - Whether a repeated motif changes on its second appearance.
- **Corruption check:** Julian's scores must rank above broken copies of themselves: clips shifted off the grid, groups swapped, random colors, gaps filled, flat brightness, unchanged repeats.
- **Against Julian:** the model's show and Julian's score for the same track, compared side by side on the same rig.
- **Learning curve:** retrain with 10, 25 and 50 of Julian's scores and watch whether quality is still rising.

### Unseen rigs

- Hold out each real rig entirely. Measure zero-shot, then after fine-tuning on 5, 10 and 20 scores for that rig.
- Shuffle group order in the rig input. The show should be the same, only reassigned.
- Perturb a real rig (drop a group, add fixtures, remove a capability) and check that the show changes sensibly.

## Open questions and risks

The biggest unknown is the quality of Claude's synthetic shows, because the placement model can't exceed them plus Julian's scores.

- **Synthetic show quality.** Claude can't hear the track. Shows may be structurally right but generic, weak on one-beat gaps, motif development and fills. The first check: Claude authors RXGER CLUB! on the Gasworks Park rig from features only, compared with Julian's score.
- **Rig geometry.** The group axis columns in the RXGER CLUB! export are all null. The rig input needs real positions and orientations.
- **Effect latent head.** A mixture head or a learned codebook, decided by which keeps rare effects intact.
- **Audio encoder licence.** MERT weights are CC BY-NC 4.0 and Audio Flamingo weights are non-commercial, so neither can ship in Luma. Use them only to measure a quality ceiling. Ship a codec-based encoder or Luma's own.
- **Analyzer features as input.** Claude authors the training shows from Luma's analyzer features. With raw audio only, the placement model must learn drop and section detection again from 6k–20k shows. Recommendation: feed beat-level onset, section and drop tokens as inputs, and keep the auxiliary heads.
- **Latent smoothness.** The contrastive loss only says same or different. The placement model predicts in this latent, so similar renders must sit close. Weight the push by render distance.
- **Second renderer.** The torch port of lower.rs can drift from lower.rs. It needs a parity test on the probe set. Or v1 drops the render loss and checks continuous settings by render only.
- **Claude baseline.** The model must match Claude authoring directly in the harness, at a lower cost per track. If it does not, ship the harness.
- **`audio()` normalization.** It normalizes over the whole track, which needs the full file. Live audio with no file needs a running normalization.
- **Unusual rigs.** Expect zero-shot failures on rigs unlike the 20 generated ones.
- **Judging good versus bad.** There is no learned reward model. Quality rests on the hand-built checks under Evaluation and on Julian's own judgment.
