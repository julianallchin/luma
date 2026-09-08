# Luma: audio→lighting model + voting arena — compressed state

## Core bet
Lighting show = program, not signal. Node graphs serialize → token DSL. One autoregressive model emits clips: node types/edges/args/group-selection, single stream, joint structure+args. Sharp timing lives INSIDE graphs (bind to onset/modulation streams at runtime, ~10ms); model decides at phrase/bar timescale (~100s ms latency budget). Model picks/composes programs, never regresses timing → learnable from small data.

## Output repr
- Grammar-masked decoding over 1–2k vocab DSL → invalid output impossible, syntax ~free, huge for small models.
- CLIP/REF+args over library defs; DEF for novel graphs (stage B).
- Factorized: p(clip|music)=p(when)·p(pattern)·p(group)·p(blend/z).
- Retrieval-augment: top-k pattern defs (graph-encoder embed, music-context query) prepended → REF library or DEF fresh. Library grows w/o retrain. Cold start = house library.

## Architecture (final rec, revised from chat)
1. **Start pretrained small code model (~0.5–1B, Qwen3.5-small class), text DSL+BPE, fine-tune.** Don't train from scratch yet. Code priors (syntax/scoping/composition) transfer to graph emission esp. DEF. From-scratch 20–50M/tiny-vocab/grammar-masked = distillation TARGET later, only if console-CPU-realtime needed. (Chat's from-scratch ledger valid but premature.)
2. **Planner/executor split.** Planner: bar-level, emits plan (section energy, palette, motif, restraint, hero groups). Executor: per-clip, conditioned on plan, near-deterministic. Taste/DPO trains planner ONLY → 2k prefs stop being thin. Plans human-editable → art-direct at plan level.
3. **Encoders:** codec (EnCodec/DAC, MIT/Apache) beat-pooled for texture + own descriptor streams (growl band, wobble rate, stereo width, roughness) as bar tokens + structure/drums/sections as explicit tokens from analyzers. MERT/Discogs-EffNet = NC → teacher-pipeline only, never ship. Own encoder if codec insufficient (see below).
4. **Realtime executor:** linear-attn/hybrid (GDN/Mamba) → constant state over 3hr set, cheap per-beat decode. Planner full-attn (bars, short).
5. One weights, both modes: randomized lookahead masking (wait-k). Causal+2–5s buffer=realtime; full-context=offline. Builds audible pre-drop → causal predicts boundaries ok.
6. Never leave token space (no diffusion/continuous latents) — critic loop + grammar masking = the leverage.

## Perception principle
LM never infers what numpy can compute. Beat grid, sections, drops, drums, stem activity, vocal presence (none/sustained/chopped + level), self-similarity (bar-pooled embed cosine vs prior bars → repeat/novelty token), energy trajectory: all explicit tokens from analyzers. LM reads structure, doesn't detect it. Making LM learn beat tracking from QA ≈ 10–100x data, frame-level supervision missing, wastes capacity — never.

### Beat-synchronous tokens
Pool encoder frames per beat via tracked grid → "bar 17"=positional, tempo normalized, patterns become fixed offsets. Fixed-grid assumption correct for EDM (95%+ DAW-quantized). Key rule: tracker jumps of exactly 2x/0.5x = metrical-level ambiguity (dubstep halftime/doubletime), NOT tempo change → snap grid back to parent tempo. Non-power-of-2 jump = real tempo change → follow (ABBA drift). Don't stretch audio (artifacts, no gain — pooling already normalizes). Gridless fallback (fixed 500ms windows + flag) for pulseless intros; modulation-following territory anyway. DJ path: CDJ grid overrides tracker phase (cue points locked to it). Note: consoles/Pioneer/Traktor/rekordbox/research models all assume fixed grid — beat-sync tracked tokens already ahead of field. Drum pattern (halftime feel) = aux prediction target → encoder encodes it; no feel token needed.

### Alignment (projector)
Frozen encoder → 2-layer MLP → LM embed space. Stage 1: LM frozen, 500–2k tracks, captioning from analyzer outputs (free), few GPU-hrs. Coverage > volume: hundreds of dubstep tracks > +1k house. Stage 2: joint w/ score SFT (projector + LoRA/top layers). Data format: mix — 40% dense caption (bars X–Y: structure/energy/texture/stereo/vocal), 30% temporal QA (when's drop, buildup length, first riff repeat — answers positional b/c beat-sync), 20% comparative QA (bar 24 vs 16 louder? bass harsher thru build?), 10% genre/style. + lighting-adjacent warm-start ("natural blackout moment?"←drop boundary). Template-generated exact answers; frontier only for prose variety slice. No lyrics/speech (Whisper offline → timestamped events graphs bind to, if ever needed). Sanity probe: post-stage-1, LM describes clip → names section/energy/texture = done. Public sets (MusicCaps CC-BY-SA/messy, Song Describer ~1.1k clean, LP-MusicCaps NC) = minor mix-in only; own generation beats all for this domain.

### Own encoder (if codec+features insufficient — test codec FIRST, ~1wk verdict)
Codec gap: reconstruction not semantics — fine-grained/local, no structure prior. Mostly covered by explicit feature tokens; residual risk = texture semantics (warm pad build vs harsh noise riser). If needed: 30–90M conv/transformer on mel. Self-sup pretrain: 20–50k unlabeled EDM tracks, masked-spec (BEATs/Audio-MAE) or contrastive, few days 1 GPU. Then multi-task FT on labeled subset: heads = beat/section/drums/stems/descriptors + contrastive adjacent-vs-distant. MERT recipe reference: HuBERT-style masked pred, dual teacher (EnCodec RVQ codes = timbre + CQT recon = pitch/harmony), 160k hrs for 330M. Ours: codec-token + CQT/own-feature targets, EDM-scoped → fraction of data. Beats MERT *for this task* on bass music (MERT barely saw it; loses general grounding — acceptable). Fine-tuning MERT: quality ceiling measurement only, derivative stays NC, dead end for product. **Compute:** RTX 6000 Pro 96GB ample; <40GB w/ mixed precision; bottleneck=throughput; 5k hrs×10 epochs=days; cache codec+CQT targets (most of wallclock). **Disk:** audio 170MB/hr raw (FLAC ~half); CQT cache ~100–200MB/hr fp16; 2TB NVMe→5k hrs, 8TB→20k hrs; stems only for labeled subset (4x audio). Keep pretrain data clean/licensed → stack shippable.

## Data plan (no procedural — quality veto)
- **Tracks:** 500–1k unique, analysis cached once each, heavy bass-music representation.
- **Scores:** 5/track, each on different rig (rig diversity free off one analysis pass; >5–8/track → agent near-dupes). 1k×5=5k full-track scores. Floor: 500×5=2.5k for house v1. + few-hundred–2k own scores (SFT) + ~2k DPO pairs.
- **Venues:** 20–50 distinct rigs = FLOOR for group embeddings meaning anything / zero-shot venues. Single rig = PoC only.
- **Cost:** cached analysis + frontier-orchestrator/cheap-subagent split + $0.05–0.15/segment → ~$500–1.5k (vs $1/gen naive = $10k).
- **Bootstrap:** expert iteration — student samples (1000s/GPU-hr) → metric critics (free python: luminance-energy corr, active-fixture ratio vs section, inter-section contrast, pre-drop headroom, color entropy, strobe budget, sync error) → VLM filter (contact-sheet/10s clip, ~$0.005, then distill to token-critic → free) → SFT survivors → loop. Agents kept only for seed diversity + hard/uncertain adjudication. Reward-hack warning: keep VLM + periodic human DPO as anchor.
- **Synthetic RM negatives:** corruption ops (desync+80ms, palette scramble, z-shuffle, all-on, motif-delete) → orig≻corrupt, 10k pairs $0 = competence axis; human pairs spend on taste axis. 2k human alone → no RM; 2k+10k synth+policy-trunk init → real RM.
- **Measure don't guess:** train stage-A at 250/500/1k filtered, plot val loss + critic scores, scale what curve demands.

## Teacher/distillation honesty
- Teacher agents cross-reference features to correct analyzer errors (drum thinning + riser + energy ⊃ section label) → corrected labels in corpus. BUT student at inference has no agent: correction transfers only if disagreement cases learnable. Fix: teacher emits reconciled section labels as supervised aux target + oversample disagreement segments → agent judgment becomes learned skill.
- Buildup/structure→macro-behavior: easiest part, works (low-entropy conditional, critics enforce as floor). Ceiling = analyzer quality on bass tracks — verify.
- Distillation caps at teacher taste: critics filter competence not expression; agents suck at dubstep → student converges clean-safe-house-median. Expression = own DPO + telemetry, not synthetic pipeline. Correct-but-predictable buildups from distillation; variety from best-of-N + prefs.
- Inference-time search: sample 200 cands/16 bars → taste-model rank → top-1 = 99.5th pctile of own dist. Mediocre sampler + good selector = product day 1. Offline full search; realtime best-of-8 in buffer. AlphaGo shape: SFT human → RL past via renderer(sim)+taste(reward).

## Patterns/library
- Per-user, general, parameterized = correct shape. Keep.
- "Advanced taste" mostly ≠ new patterns: (a) modulation-source nodes — beam_width←centroid, chase_rate←lfo, chase_pos←stereo_pos, strobe_density←roughness; L/R chase = stereo-field following not wub trigger; wanting "wub-chase def" = missing binding signal. Wubs-too-binary solved: continuous descriptor streams supply shape, model picks mappings ("mid-growl, 1/8 wobble, hard stereo" → graphs exposing right mod inputs). (b) Rig scale = same def + group-local u,v coords + group selection + compatibility embedding; large rigs need LAYERING (z/blend composition, score-level, model already does).
- Residual true-custom: artist signatures, non-compositional genre shapes → DEFs by teacher/user, embedded, retrieved by context. Fixed core ~50 defs (chase, flash-dissolve, strobe, sweep, bleed, blinder) + growing retrievable corpus. Never per-venue/per-track (venue→group schema, track→score).
- Uniform arg surface: typed ranges, standard names — else taste stuck in idiosyncratic args.
- **Compatibility embeddings:** per rig session render every (def×group) 2 bars lowres → stats (motion dir, coverage, contrast) → bias group-pointer logits / tokens at REF decode. ~1k cheap renders once at load. Model sees the preview panel.
- **Group schema:** frontier describer compiles rig → categorical tokens (role=punctuation brightness=XXL duty_max=0.08 axis=X arrangement=linear elev=truss hero=true), offline once/rig. Blinder-prior relocates to describer. Editable flags = per-rig behavior injection, zero training.

### Teaching DEF
Stage A: student picks/binds only; novel patterns from teacher LLM (writes graphs from descriptions, render+critic-filtered, survivors → library). Stage B: DEF in vocab once few-k teacher DEFs exist; same grammar-masked stream; every teacher-DEF'd score = supervised (context→define→use). Def quality = render critics: novelty (render-stat embed dist vs library) + sync/readability. Small model = recombination mostly; invention stays teacher/user; product path: user describes → teacher writes → lands in library → student reuses.

### Community library
Package registry + canonicalizer, not uploads folder. Canonical form: deterministic serialization (sorted nodes, stable edges, normalized args, typed/range-declared vs shared schema) → also makes training data consistent. Dedup 3 levels: exact hash / graph-embed near-dupe (merge/variant) / render-stat functional dupe across ref rigs (catches most — noobs rebuild existing). Naming: describer over graph+render → schema (role/motion/axis/intensity_curve/tags) + display name (author overridable, schema machine-only). Tiers: core(~50, curated, ships) / published(gated) / personal(anything) — only core+published feed training/retrieval. Publish gate = critic threshold on 3–5 ref rigs + novelty floor (same pipeline as DEF filter). Immutable content-hashed versions; scores pin version; edits → new version.

## Arena (lightarena / lightbench — check .com first, .ai fallback, grab both; avoid double-vowel names; neutral non-Luma branding → benchmark credibility, researchers link it)
**One-paragraph:** public head-to-head arena for AI lighting shows: ~1k tracks × ~5 venues, each candidate model (frontier agents fixed public harness + own policy + human-LD baseline — human entry makes it a story not an ad) generates 1 score/cell, rendered to short clips, visitors vote blind same-track-same-venue pairs; votes → Bradley-Terry w/ per-voter reliability → hype model leaderboard; same log = DPO corpus (every vote = chosen/rejected on identical prompt, voter-quality weighted) → site = marketing + benchmark + data flywheel simultaneously.

### Math
Score β_{m,t,r} = μ_m + ε_{m,t,r}, ε~N(0,σ²). Vote: P(a≻b|v) = sigmoid(λ_v[(μ_a−μ_b)+(ε_a−ε_b)]); track/venue cancel. Fit μ,ε,λ jointly: Σ log sigmoid(λ_v Δβ) − Σε²/2σ² + Σ log p(λ_v); fix μ_1=0. ε-prior stops 2-vote scores swinging, shrinks to model mean. One score/cell fine: every vote = model comparison (ideal for leaderboard); per-score β only via μ+few votes → don't display score leaderboard, keep μ_m + hidden residuals. Within-model pairs later: generate K/cell for own model only, non-uniform ok.
- **Voter reliability λ_v≥0, all-probabilistic no flags:** perfect voter high λ (sharp); random λ≈0 (50/50 regardless of gap, zero info, influence self-shrinks). Prior (gamma/lognormal @ pop avg) → new voters start average, move w/ evidence. Dawid-Skene/item-response family. λ_v = DPO pair weight directly.
- **Always-right-tapper:** randomize sides → their vote = coinflip; detectable via sanity pairs (big known gap): binomial 0.5 vs ~0.9 separable n≈10, certain @20; 1 sanity per ~5 votes → identified in 50–100 votes. No side randomization → undetectable bias. RANDOMIZE SIDES.
- **Elo vs BT:** BT = Elo's underlying model (logistic in β-diff; Elo=base-10/400-scale sequential update). Elo order-dependent, early-vote noise, no error bars, can't cheaply un-apply bad voter. BT: all-at-once MLE (logistic regression, ±1 one-hot), std errors, refit-able. Live display = Elo or cached μ; truth = scheduled BT refit. Correction = zero-weight votes in log (never delete) + recompute; full replay 100k votes = ms. Ties: half-win each or Davidson ν; skips logged separately (indistinguishable vs can't-tell), dropped from DPO.
- **DPO from votes:** pairs direct (chosen/rejected same prompt). Filter: drop close-rating single-vote coinflips, keep consistent-multi-vote or meaningful-gap; weight by λ_v ×optional |Δβ| margin. Pair mining: stable ratings → any big-gap same-cell pair = synthetic pair w/o direct vote → 5k votes ≫ 5k pairs.
- **Taste layering (crowd ≠ taste):** crowd = competence+mass appeal, pulls to median (strobes-everywhere beats restraint publicly). 1) crowd volume 2) trusted-taste pool, λ pinned high or separate DPO stage 3) product telemetry (keep/edit/regen = dense free prefs, strongest). Clip selection: 8 bars around drop not random window; question "which would you rather see live" not "better."

### Routing
Not uniform-coverage — max info/vote: uniform over cells, prefer close-rating + low-vote-count pairs, cross-model >> within (leaderboard info), never repeat pair/voter, sanity pairs injected flagged 1-in-N. Skip button (≠tie) logged.

### Infra
- Clips: R2 (zero egress — 100k votes×2×2MB=400GB, Supabase bills that; Supabase Storage ok day-1, migrate = URL swap). 720p H.264/AV1, keyframe@0, faststart, plain mp4 <video> no HLS, 1–3MB/10s. Content-hashed names + `max-age=31536000, immutable`. Range passthrough.
- First-pair problem (depends on state+voter): decouple — precomputed good-pair pool (few hundred, refit every few min) in KV/Durable Object; edge Worker picks random from pool into initial HTML w/ preload tags; new voter has no info anyway; returning voter → KV seen-set filter; suboptimal first pick ≈ zero cost. Pair N+1 prefetched during pair N → full voter-aware backend selection has whole clip duration. Pre-warm edge post-upload.
- Stack: Next.js on CF Pages (OpenNext), KV (pool+seen-sets), R2 (clips), Supabase (Postgres/auth/RLS, pooler or Hyperdrive from Workers), Postgres fn for atomic vote-write+seen-update. BT fit = Python, scheduled (GH Action/Modal/small box), reads log writes ratings — never in request path. Workers: 30s CPU, no Python.
- Identity: anon UUID cookie 1yr + localStorage fallback, votes+seen keyed on it; login later merges (union seen-sets). Cross-device duplicate-pair collision rare+harmless (two λ estimates). No fingerprinting. Threat = many-IDs-spam not cross-device: IP rate-limit at Worker, votes/ID/hr cap, <2s votes→skips; fresh-ID λ shrunk to prior anyway → 100 new IDs×1 ≪ 1 ID×100. Sign pair-ID in vote request; clips public.

## Open verifications
1. Codec+features vs pretrained-encoder ablation (policy quality, held-out dubstep) — decides encoder build.
2. Analyzer accuracy on bass tracks (structure/drop detection) — student ceiling.
3. Scaling curve 250/500/1k filtered segments before trusting any data number.
4. MERT license terms re: training on outputs (murky) before teacher-pipeline reliance.
5. Section-aware pass in fixed-grid fitter for real tempo changes (140→174 segments).
6. lightarena/lightbench .com availability.