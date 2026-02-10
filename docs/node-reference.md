# Luma Node Type Reference

Complete reference for all node types available in Luma's pattern graph editor.

This document covers every node in the system: its type ID, category, inputs, outputs, parameters, and behavioral details. Nodes are connected via typed ports to form dataflow graphs that produce time-varying lighting output.

---

## Port Types

Data flows between nodes through typed ports. Understanding these types is essential for building valid graphs.

| Type | Description | Typical Dimensions |
|------|-------------|-------------------|
| **Signal** | 3D tensor (N spatial x T temporal x C channels) | Varies |
| **Audio** | Mono audio buffer with sample rate and crop info | ~22050 Hz |
| **BeatGrid** | Beat positions, BPM, downbeat markers | Metadata |
| **Selection** | Set of fixtures with 3D positions | N items |
| **Color** | RGBA color value | 1x1x4 Signal |
| **Gradient** | Color interpolation specification | Metadata |

**Signal dimension conventions:**

- **N** (spatial): One value per fixture or spatial point.
- **T** (temporal): One value per time step. Typically `SIMULATION_RATE * duration` frames.
- **C** (channels): Data channels. 1 for scalar, 3 for RGB, 4 for RGBA, 12 for chroma, etc.

---

## Input Nodes

These nodes inject external data (audio, beat timing, user-defined arguments) into the graph.

### Audio Input

| Field | Value |
|-------|-------|
| **Type ID** | `audio_input` |
| **Category** | Input |
| **Description** | Provides the audio segment for the current pattern context. Automatically loaded from the track being annotated. |

**Inputs:** None

**Outputs:**

| Port | Type |
|------|------|
| `out` | Audio |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `trackId` | string | (set by context) | Track identifier (informational) |
| `startTime` | float | (set by context) | Annotation start time in seconds |
| `endTime` | float | (set by context) | Annotation end time in seconds |

**Behavior Notes:**
- The audio is cropped to the annotation's time range.
- Sample rate is typically 22050 Hz (downsampled for analysis).
- Parameters are set automatically by the pattern context and are informational only.

---

### Beat Clock

| Field | Value |
|-------|-------|
| **Type ID** | `beat_clock` |
| **Category** | Input |
| **Description** | Provides the beat grid (BPM, beat positions, downbeats) for the current context. |

**Inputs:** None

**Outputs:**

| Port | Type |
|------|------|
| `grid_out` | BeatGrid |

**Parameters:** None

**Behavior Notes:**
- Beat grid times are adjusted relative to the annotation's start time.
- BPM and downbeat markers come from the pre-computed beat analysis stored in the database.

---

### Pattern Arguments

| Field | Value |
|-------|-------|
| **Type ID** | `pattern_args` |
| **Category** | Input |
| **Description** | Exposes pattern arguments defined in the pattern's `args` array. Each argument becomes an output port. |

**Inputs:** None

**Outputs:** One port per argument definition:

| Argument Type | Output Port Type |
|---------------|-----------------|
| Color arg | Signal (N=1, T=1, C=4 RGBA) |
| Scalar arg | Signal (N=1, T=1, C=1) |
| Selection arg | Selection (resolved from tag expression) |

**Parameters:** None (configured via pattern arg definitions)

**Behavior Notes:**
- Output ports are dynamically created based on the pattern's argument schema.
- Color arguments produce RGBA signals with values normalized to 0.0-1.0.
- Selection arguments resolve tag expressions against the current fixture configuration.

---

## Audio Processing Nodes

These nodes transform audio signals: splitting stems, filtering frequencies, and extracting rhythmic envelopes.

### Stem Splitter

| Field | Value |
|-------|-------|
| **Type ID** | `stem_splitter` |
| **Category** | Audio Processing |
| **Description** | Separates audio into 4 stems using pre-computed Demucs htdemucs results. |

**Inputs:**

| Port | Type |
|------|------|
| `audio_in` | Audio |

**Outputs:**

| Port | Type |
|------|------|
| `drums_out` | Audio |
| `bass_out` | Audio |
| `vocals_out` | Audio |
| `other_out` | Audio |

**Parameters:** None

**Behavior Notes:**
- Stems are cached in memory (`Arc<Vec<f32>>`). First access loads from the database.
- Each stem is converted to mono and cropped to match the input's time range.
- Uses pre-computed Demucs htdemucs separation results; no real-time ML inference occurs during graph evaluation.

---

### Frequency Amplitude

| Field | Value |
|-------|-------|
| **Type ID** | `frequency_amplitude` |
| **Category** | Audio Processing |
| **Description** | Extracts amplitude in selected frequency bands via FFT analysis. |

**Inputs:**

| Port | Type |
|------|------|
| `audio_in` | Audio |

**Outputs:**

| Port | Type |
|------|------|
| `amplitude_out` | Signal (N=1, T=FFT frames, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `selected_frequency_ranges` | JSON array | — | Array of `[min_hz, max_hz]` pairs defining frequency bands |

**Behavior Notes:**
- Uses the FFT service for spectral analysis.
- Multiple frequency ranges are averaged into a single amplitude value per frame.
- Useful for isolating specific instrument ranges (e.g., `[20, 200]` for kick drum, `[2000, 8000]` for hi-hats).

---

### Lowpass Filter

| Field | Value |
|-------|-------|
| **Type ID** | `lowpass_filter` |
| **Category** | Audio Processing |
| **Description** | IIR lowpass filter applied to audio signal. |

**Inputs:**

| Port | Type |
|------|------|
| `audio_in` | Audio |

**Outputs:**

| Port | Type |
|------|------|
| `audio_out` | Audio |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `cutoff_hz` | float | 200.0 | Cutoff frequency in Hz (clamped to [1, Nyquist]) |

**Behavior Notes:**
- Preserves sample rate, crop info, and track metadata from the input.
- Cutoff frequency is clamped to the valid range [1 Hz, Nyquist frequency].

---

### Highpass Filter

| Field | Value |
|-------|-------|
| **Type ID** | `highpass_filter` |
| **Category** | Audio Processing |
| **Description** | IIR highpass filter applied to audio signal. |

**Inputs:**

| Port | Type |
|------|------|
| `audio_in` | Audio |

**Outputs:**

| Port | Type |
|------|------|
| `audio_out` | Audio |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `cutoff_hz` | float | 200.0 | Cutoff frequency in Hz |

**Behavior Notes:**
- Behaves identically to the lowpass filter in terms of metadata preservation, but passes frequencies above the cutoff.

---

### Beat Envelope

| Field | Value |
|-------|-------|
| **Type ID** | `beat_envelope` |
| **Category** | Audio Processing |
| **Description** | Generates ADSR envelopes triggered at beat positions. The core rhythmic driver for most patterns. |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `grid` | BeatGrid | Yes |
| `subdivision` | Signal | No (overrides the subdivision parameter if connected) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=SIMULATION_RATE x duration, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `subdivision` | float | 1.0 | Beat subdivision: 0.25=sixteenth, 0.5=eighth, 1=quarter, 2=half, 4=whole |
| `only_downbeats` | bool | false | Only trigger on downbeats (bar starts) |
| `offset` | float | 0.0 | Beat fraction offset |
| `attack` | float | 0.1 | Attack phase weight |
| `decay` | float | 0.3 | Decay phase weight |
| `sustain` | float | 0.3 | Sustain phase weight |
| `release` | float | 0.3 | Release phase weight |
| `sustain_level` | float | 0.7 | Sustain floor (0-1) |
| `attack_curve` | float | 0.0 | Attack shape: -1=snappy/instant, +1=slow swell |
| `decay_curve` | float | 0.0 | Decay shape: -1=snappy, +1=slow |
| `amplitude` | float | 1.0 | Output scale (0-1) |

**Behavior Notes:**
- Weights (attack + decay + sustain + release) are normalized to the inter-beat duration.
- Multiple overlapping pulses are summed, which can produce values greater than 1.0.
- The `subdivision` input port, when connected, overrides the `subdivision` parameter.
- Common subdivisions: 0.25 (sixteenth notes), 0.5 (eighth notes), 1.0 (quarter notes), 2.0 (half notes), 4.0 (whole notes).

---

## Analysis Nodes

These nodes extract musical features from audio and present them as signals.

### Harmony Analysis

| Field | Value |
|-------|-------|
| **Type ID** | `harmony_analysis` |
| **Category** | Analysis |
| **Description** | Outputs 12-channel chroma distribution from pre-computed chord analysis. |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `audio_in` | Audio | Yes |
| `grid_in` | BeatGrid | No |

**Outputs:**

| Port | Type |
|------|------|
| `signal` | Signal (N=1, T=SIMULATION_RATE x duration, C=12) |

**Parameters:** None

**Behavior Notes:**
- Uses pre-computed consonance-ACE chord sections from the database.
- If binary logits are available, applies softmax per frame for smooth probability distributions.
- Each of the 12 channels represents a pitch class: C, C#, D, D#, E, F, F#, G, G#, A, A#, B (channels 0-11).
- Output values represent the relative strength of each pitch class at each time step.

---

### Harmonic Tension

| Field | Value |
|-------|-------|
| **Type ID** | `harmonic_tension` |
| **Category** | Analysis |
| **Description** | Computes musical tension/dissonance from chroma distribution using Shannon entropy. |

**Inputs:**

| Port | Type |
|------|------|
| `chroma` | Signal (C=12) |

**Outputs:**

| Port | Type |
|------|------|
| `tension` | Signal (N=1, T=input.T, C=1) |

**Parameters:** None

**Behavior Notes:**
- Entropy is normalized by ln(12) to produce a 0.0-1.0 range.
- Output 0.0 means perfectly consonant (single strong pitch dominates).
- Output 1.0 means maximum dissonance (all pitches equally present).
- Useful for driving intensity or color saturation from harmonic complexity.

---

### Mel Spectrogram Viewer

| Field | Value |
|-------|-------|
| **Type ID** | `mel_spec_viewer` |
| **Category** | Analysis / Visualization |
| **Description** | Computes mel spectrogram for visualization in the pattern editor. Does not produce wired output. |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `in` | Audio | Yes |
| `grid` | BeatGrid | No |

**Outputs:** None (stored in `state.mel_specs` for frontend display)

**Parameters:** None

**Behavior Notes:**
- Resolution: 128x128.
- Beat grid overlay is included in the visualization if the grid input is connected.
- This is a visualization-only node; it does not produce output signals for other nodes to consume.

---

### View Signal

| Field | Value |
|-------|-------|
| **Type ID** | `view_signal` |
| **Category** | Analysis / Visualization |
| **Description** | Debug node that captures a signal for visualization in the editor. |

**Inputs:** First connected input (any Signal)

**Outputs:** None (stored in `state.view_results`)

**Parameters:** None

**Behavior Notes:**
- Only active when `compute_visualizations` is enabled in the evaluation context.
- Accepts any Signal type on its first connected input.
- Useful for debugging intermediate values in a graph.

---

## Selection Nodes

These nodes define which fixtures are targeted and extract spatial attributes from fixture arrangements.

### Select

| Field | Value |
|-------|-------|
| **Type ID** | `select` |
| **Category** | Selection |
| **Description** | Resolves a tag expression into a set of fixtures with 3D positions. |

**Inputs:** None

**Outputs:**

| Port | Type |
|------|------|
| `out` | Selection |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `tag_expression` | string | `"all"` | Boolean tag expression to match fixtures |
| `spatial_reference` | string | `"global"` | `"global"` or `"group_local"` |

**Behavior Notes:**
- Tag expression syntax:
  - `all` -- matches every fixture
  - Simple tags: `front`, `left`, `circular`, `blinder`
  - Capability tokens: `has_color`, `has_movement`, `has_strobe`
  - Operators: `&` (AND), `|` (OR), `^` (XOR), `~` (NOT), `>` (fallback)
  - Parentheses for grouping: `(left | right) & has_color`
  - Examples: `front`, `left & has_color`, `circular & ~blinder`
- `spatial_reference="global"`: All matched fixtures returned in a single Selection with global coordinates.
- `spatial_reference="group_local"`: Separate Selection per fixture group, with coordinates relative to each group's local frame.

---

### Get Attribute

| Field | Value |
|-------|-------|
| **Type ID** | `get_attribute` |
| **Category** | Selection |
| **Description** | Extracts a spatial or ordering attribute from each fixture in a Selection as a scalar Signal. |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=total items, T=1, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `attribute` | string | — | The attribute to extract (see list below) |

**Available attributes:**

| Attribute | Description | Range |
|-----------|-------------|-------|
| `index` | Integer order within the selection | 0, 1, 2, ... |
| `normalized_index` | Order normalized to range | 0.0 - 1.0 |
| `pos_x` | Absolute global X position | meters |
| `pos_y` | Absolute global Y position | meters |
| `pos_z` | Absolute global Z position | meters |
| `rel_x` | X position relative to selection bounding box | 0.0 - 1.0 |
| `rel_y` | Y position relative to selection bounding box | 0.0 - 1.0 |
| `rel_z` | Z position relative to selection bounding box | 0.0 - 1.0 |
| `rel_major_span` | Position along axis with largest physical range | 0.0 - 1.0 |
| `rel_major_count` | Position along axis with most distinct positions | 0.0 - 1.0 |
| `circle_radius` | Distance from selection center | meters |
| `angular_position` | Angle on fitted circle (PCA + RANSAC) | 0.0 - 1.0 |
| `angular_index` | Index-based angular position (equal spacing) | 0.0 - 1.0 |

**Behavior Notes:**
- For angular attributes, circle fitting is performed using PCA plane projection and RANSAC. This works for 3D arrangements, not just planar layouts.
- `rel_major_span` and `rel_major_count` are useful when the fixture arrangement's primary axis is not known ahead of time.

---

### Random Select Mask

| Field | Value |
|-------|-------|
| **Type ID** | `random_select_mask` |
| **Category** | Selection |
| **Description** | Randomly selects N items from a Selection based on a trigger signal. Changes selection when trigger value changes. |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `selection` | Selection | Yes |
| `trigger` | Signal | Yes |
| `count` | Signal | No (number of items to select) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=items, T=trigger.T, C=1, values 0 or 1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `avoid_repeat` | int | 1 | If 1, avoids selecting the same items on consecutive triggers |

**Behavior Notes:**
- Selection is deterministic (hash-based seeding) for reproducibility across evaluations.
- Trigger changes (e.g., beat pulses crossing a threshold) cause re-selection of which items are active.
- Output is a binary mask: 1.0 for selected fixtures, 0.0 for unselected.

---

## Color Nodes

These nodes generate and transform color signals.

### Color

| Field | Value |
|-------|-------|
| **Type ID** | `color` |
| **Category** | Color |
| **Description** | Constant color generator. |

**Inputs:** None

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=1, C=4 RGBA) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `color` | JSON string | — | Color as `{"r": 0-255, "g": 0-255, "b": 0-255, "a": 0-1}` |

**Behavior Notes:**
- RGB values in the parameter are 0-255 integers; they are normalized to 0.0-1.0 in the output signal.
- Alpha is already 0.0-1.0 in the parameter.

---

### Gradient

| Field | Value |
|-------|-------|
| **Type ID** | `gradient` |
| **Category** | Color |
| **Description** | Maps a scalar input signal to a color range via linear interpolation. |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `in` | Signal (C >= 1) | Yes |
| `start_color` | Signal | No (overrides start_color parameter) |
| `end_color` | Signal | No (overrides end_color parameter) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=in.N, T=in.T, C=4 RGBA) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `start_color` | hex string | `"#000000"` | Color at input value 0.0 |
| `end_color` | hex string | `"#ffffff"` | Color at input value 1.0 |

**Behavior Notes:**
- The input signal's first channel (C=0) is used as the interpolation factor.
- Input values are expected in the 0.0-1.0 range; values outside this range will extrapolate.
- Wired `start_color` and `end_color` inputs override the corresponding parameters.
- Interpolation is performed independently per RGBA channel.

---

### Chroma Palette

| Field | Value |
|-------|-------|
| **Type ID** | `chroma_palette` |
| **Category** | Color |
| **Description** | Maps 12-pitch chroma distribution to RGB color via weighted palette lookup. |

**Inputs:**

| Port | Type |
|------|------|
| `chroma` | Signal (C=12) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=chroma.T, C=3 RGB) |

**Parameters:** None

**Behavior Notes:**
- Fixed pitch-to-color palette:
  - C = Red
  - C# = OrangeRed
  - D = Orange
  - D# = Gold
  - E = Yellow
  - F = GreenYellow
  - F# = Green
  - G = Cyan
  - G# = DeepSkyBlue
  - A = Blue
  - A# = BlueViolet
  - B = Magenta
- Output color is the weighted sum of all 12 palette entries, where weights come from the chroma distribution.
- Auto-gain normalization ensures consistent brightness regardless of chroma magnitude.

---

### Spectral Shift

| Field | Value |
|-------|-------|
| **Type ID** | `spectral_shift` |
| **Category** | Color |
| **Description** | Rotates input color's hue based on the dominant pitch in a chroma signal. |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal (C >= 3, RGB) |
| `chroma` | Signal (C=12) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=min(in.T, chroma.T), C=3 RGB) |

**Parameters:** None

**Behavior Notes:**
- Converts input color to HSL, shifts hue by `(dominant_pitch / 12) * 360` degrees, converts back to RGB.
- The dominant pitch is the channel index with the highest value in the chroma distribution at each time step.
- Creates harmonically-responsive color shifting that tracks chord changes.

---

## Signal Processing Nodes

These nodes perform mathematical operations, generate waveforms, and manipulate signal timing and shape.

### Math

| Field | Value |
|-------|-------|
| **Type ID** | `math` |
| **Category** | Signal Processing |
| **Description** | Element-wise binary math operations with broadcasting. |

**Inputs:**

| Port | Type |
|------|------|
| `a` | Signal |
| `b` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `operation` | string | — | One of the operations listed below |

**Available operations:**

| Operation | Formula | Description |
|-----------|---------|-------------|
| `add` | `a + b` | Addition |
| `subtract` | `a - b` | Subtraction |
| `multiply` | `a * b` | Multiplication |
| `divide` | `a / b` | Division |
| `max` | `max(a, b)` | Element-wise maximum |
| `min` | `min(a, b)` | Element-wise minimum |
| `abs_diff` | `|a - b|` | Absolute difference |
| `abs` | `|a|` | Absolute value (only uses input `a`) |
| `modulo` | `a % b` | Modulo |
| `circular_distance` | `min(|a-b|, 1-|a-b|)` | Shortest distance on a [0,1] ring |

**Behavior Notes:**
- Full broadcasting: output shape is `max(a, b)` per dimension (N, T, C).
- `circular_distance` is useful for angular/cyclic patterns where 0.0 and 1.0 are adjacent.

---

### Round

| Field | Value |
|-------|-------|
| **Type ID** | `round` |
| **Category** | Signal Processing |
| **Description** | Rounding operations. |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `operation` | string | — | One of: `floor`, `ceil`, `round` |

**Behavior Notes:**
- `floor`: rounds toward negative infinity.
- `ceil`: rounds toward positive infinity.
- `round`: rounds to nearest integer (half rounds away from zero).

---

### Threshold

| Field | Value |
|-------|-------|
| **Type ID** | `threshold` |
| **Category** | Signal Processing |
| **Description** | Binary threshold (step function). |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `threshold` | float | 0.5 | Threshold value |

**Behavior Notes:**
- Output is 1.0 if input >= threshold, else 0.0.
- Useful for converting continuous envelopes into binary triggers.

---

### Normalize

| Field | Value |
|-------|-------|
| **Type ID** | `normalize` |
| **Category** | Signal Processing |
| **Description** | Min-max normalization to [0, 1]. |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:** None

**Behavior Notes:**
- Formula: `(value - min) / (max - min)`.
- If all values are equal (max == min), outputs 0.0 to avoid division by zero.
- Min and max are computed across all dimensions of the input signal.

---

### Falloff

| Field | Value |
|-------|-------|
| **Type ID** | `falloff` |
| **Category** | Signal Processing |
| **Description** | Non-linear attenuation with adjustable width and curve shape. |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal (values 0-1) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `width` | float | 1.0 | Controls spread of the falloff |
| `curve` | float | 0.0 | Shape: -1=snappy/sharp, 0=linear, +1=gentle/swell |

**Behavior Notes:**
- Processing pipeline: clamp to [0, 1], scale by width, apply shape curve (exponential).
- Essential for creating sharp vs. soft spatial transitions in chase patterns.
- A narrow width with a snappy curve creates a tight "spotlight" effect; wide width with a gentle curve creates a broad wash.

---

### Invert

| Field | Value |
|-------|-------|
| **Type ID** | `invert` |
| **Category** | Signal Processing |
| **Description** | Reflects values around the midpoint of the observed range. |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:** None

**Behavior Notes:**
- Formula: `reflected = 2 * midpoint - value`, where midpoint is `(min + max) / 2`.
- Result is clamped to [min, max] of the input range.
- For a signal in the 0.0-1.0 range, this is equivalent to `1.0 - value`.

---

### Scalar

| Field | Value |
|-------|-------|
| **Type ID** | `scalar` |
| **Category** | Signal Processing |
| **Description** | Constant scalar value. |

**Inputs:** None

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=1, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `value` | float | 1.0 | The constant value |

**Behavior Notes:**
- Broadcasts to any shape when connected to nodes that expect larger signals.

---

### Ramp

| Field | Value |
|-------|-------|
| **Type ID** | `ramp` |
| **Category** | Signal Processing |
| **Description** | Linear beat counter from 0 to total beats. |

**Inputs:**

| Port | Type |
|------|------|
| `grid` | BeatGrid |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=SIMULATION_RATE x duration, C=1) |

**Parameters:** None

**Behavior Notes:**
- Formula: `beat_count = (current_time - start_time) * (BPM / 60)`.
- Output is monotonically increasing over the pattern duration.
- Commonly used with `modulo` and `math(subtract)` to create repeating spatial chases.

---

### Ramp Between

| Field | Value |
|-------|-------|
| **Type ID** | `ramp_between` |
| **Category** | Signal Processing |
| **Description** | Linear interpolation from start value to end value over the pattern duration. |

**Inputs:**

| Port | Type |
|------|------|
| `grid` | BeatGrid |
| `start` | Signal |
| `end` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=SIMULATION_RATE x duration, C=1) |

**Parameters:** None

**Behavior Notes:**
- Formula: `output = start + (end - start) * (beats / total_beats)`.
- At the first beat, output equals `start`; at the last beat, output equals `end`.
- Useful for creating gradual transitions (e.g., increasing intensity, shifting color over a section).

---

### Modulo

| Field | Value |
|-------|-------|
| **Type ID** | `modulo` |
| **Category** | Signal Processing |
| **Description** | Modulo operation (positive remainder). |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `divisor` | float | 1.0 | The divisor for the modulo operation |

**Behavior Notes:**
- Uses Euclidean mod: `((value % divisor) + divisor) % divisor` (always positive).
- With `divisor=1.0`, wraps any value into the [0, 1) range.
- Essential for creating repeating patterns from monotonically increasing ramps.

---

### Sine Wave

| Field | Value |
|-------|-------|
| **Type ID** | `sine_wave` |
| **Category** | Signal Processing |
| **Description** | Continuous sinusoidal oscillator. |

**Inputs:** None

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=1, T=256, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `frequency_hz` | float | 0.25 | Oscillation frequency in Hz |
| `phase_deg` | float | 0 | Phase offset in degrees |
| `amplitude` | float | 1.0 | Output amplitude |
| `offset` | float | 0.0 | DC offset added to output |

**Behavior Notes:**
- Formula: `output = offset + amplitude * sin(2 * pi * frequency * t + phase_rad)`.
- Output range (with defaults): [-1.0, 1.0].
- Useful for slow LFO effects (e.g., breathing, swaying) at low frequencies.

---

### Remap

| Field | Value |
|-------|-------|
| **Type ID** | `remap` |
| **Category** | Signal Processing |
| **Description** | Linear range remapping (like Arduino's `map()` function). |

**Inputs:**

| Port | Type |
|------|------|
| `in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (same N and T as input, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `in_min` | float | -1.0 | Input range minimum |
| `in_max` | float | 1.0 | Input range maximum |
| `out_min` | float | 0.0 | Output range minimum |
| `out_max` | float | 180.0 | Output range maximum |
| `clamp` | bool | false | Whether to clamp output to [out_min, out_max] |

**Behavior Notes:**
- Formula: `output = out_min + (input - in_min) / (in_max - in_min) * (out_max - out_min)`.
- When `clamp` is false, values outside the input range will extrapolate beyond the output range.
- Useful for converting signal ranges (e.g., sine wave [-1, 1] to pan angle [0, 360]).

---

### Noise

| Field | Value |
|-------|-------|
| **Type ID** | `noise` |
| **Category** | Signal Processing |
| **Description** | 3D fractal Perlin-like value noise with octaves. |

**Inputs:**

| Port | Type |
|------|------|
| `time` | Signal |
| `x` | Signal |
| `y` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal (N=max(x.N, y.N), T=max inputs, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `scale` | float | — | Spatial/temporal frequency multiplier |
| `octaves` | int (1-8) | — | Fractal detail layers |
| `amplitude` | float | — | Output scale |
| `offset` | float | — | Output offset |

**Behavior Notes:**
- Uses smoothstep-interpolated value noise with fractal octave composition.
- Each octave doubles frequency and halves amplitude (standard fBm).
- The 3D input (time, x, y) allows spatially and temporally varying noise.
- Useful for organic, non-repeating effects like flickering, turbulence, or natural movement.

---

### Time Delay

| Field | Value |
|-------|-------|
| **Type ID** | `time_delay` |
| **Category** | Signal Processing |
| **Description** | Per-fixture time offset with interpolation. |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `in` | Signal | Yes |
| `delay` | Signal | No (per-N delay values) |

**Outputs:**

| Port | Type |
|------|------|
| `out` | Signal |

**Parameters:** None

**Behavior Notes:**
- For each fixture n: `sample_time = current_time - delay[n]`.
- Creates chase effects when combined with spatial attributes (e.g., `get_attribute(normalized_index)` as delay values).
- Linear interpolation between samples for smooth sub-frame offsets.

---

### Orbit

| Field | Value |
|-------|-------|
| **Type ID** | `orbit` |
| **Category** | Signal Processing |
| **Description** | Elliptical 3D circular motion (for moving light targets). |

**Inputs:**

| Port | Type | Required |
|------|------|----------|
| `grid` | BeatGrid | Yes |
| `phase` | Signal | No (per-N phase offsets) |

**Outputs:**

| Port | Type |
|------|------|
| `x` | Signal (N=phase.N or 1, T=256, C=1) |
| `y` | Signal (N=phase.N or 1, T=256, C=1) |
| `z` | Signal (N=phase.N or 1, T=256, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `center_x` | float | — | Center point X coordinate |
| `center_y` | float | — | Center point Y coordinate |
| `center_z` | float | — | Center point Z coordinate |
| `radius_x` | float | — | Ellipse radius along X axis |
| `radius_z` | float | — | Ellipse radius along Z axis |
| `speed` | float | — | Rotations per beat cycle |
| `tilt_deg` | float | — | Plane tilt angle in degrees |

**Behavior Notes:**
- Full 3D orbit with tilt applied to the orbital plane.
- The `phase` input enables per-fixture offset for staggered orbits (e.g., fixtures tracing the same circle but at different positions).
- Produces three separate coordinate outputs for use with `look_at_position` or `apply_position`.

---

### Random Position

| Field | Value |
|-------|-------|
| **Type ID** | `random_position` |
| **Category** | Signal Processing |
| **Description** | Random 3D point in bounding box, held until trigger changes. |

**Inputs:**

| Port | Type |
|------|------|
| `trigger` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `x` | Signal (N=1, T=trigger.T, C=1) |
| `y` | Signal (N=1, T=trigger.T, C=1) |
| `z` | Signal (N=1, T=trigger.T, C=1) |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `min_x` | float | — | Bounding box minimum X |
| `max_x` | float | — | Bounding box maximum X |
| `min_y` | float | — | Bounding box minimum Y |
| `max_y` | float | — | Bounding box maximum Y |
| `min_z` | float | — | Bounding box minimum Z |
| `max_z` | float | — | Bounding box maximum Z |

**Behavior Notes:**
- Hash-seeded randomness ensures deterministic output per trigger value.
- Position is held constant until the trigger signal changes value (e.g., on a beat pulse).
- Useful for making moving lights jump to random positions on each beat.

---

### Smooth Movement

| Field | Value |
|-------|-------|
| **Type ID** | `smooth_movement` |
| **Category** | Signal Processing |
| **Description** | Rate-limited pan/tilt movement simulating physical motor speed. |

**Inputs:**

| Port | Type |
|------|------|
| `pan_in` | Signal |
| `tilt_in` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `pan` | Signal |
| `tilt` | Signal |

**Parameters:**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `pan_max_deg_per_s` | float | 360 | Maximum pan speed in degrees per second |
| `tilt_max_deg_per_s` | float | 180 | Maximum tilt speed in degrees per second |

**Behavior Notes:**
- Clamps the delta per time step to the maximum speed.
- Prevents unrealistic instant movement that would not be physically achievable by real fixtures.
- Place between position-generating nodes and `apply_position` for realistic motion.

---

## Position Nodes

These nodes compute aiming angles for moving lights.

### Look At Position

| Field | Value |
|-------|-------|
| **Type ID** | `look_at_position` |
| **Category** | Position |
| **Description** | Computes pan/tilt angles to aim each fixture at a target 3D point. |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |
| `x` | Signal |
| `y` | Signal |
| `z` | Signal |

**Outputs:**

| Port | Type |
|------|------|
| `pan` | Signal (degrees) |
| `tilt` | Signal (degrees) |

**Parameters:** None

**Behavior Notes:**
- Computes inverse kinematics for each fixture based on its position and orientation.
- Accounts for fixture mounting angle.
- Output angles are in degrees, suitable for direct connection to `apply_position`.

---

## Output / Apply Nodes

Apply nodes are the terminal nodes of a graph. They have no wired outputs. Instead, they generate `LayerTimeSeries` entries that are collected at the end of graph execution and sent to the compositing system.

### Apply Dimmer

| Field | Value |
|-------|-------|
| **Type ID** | `apply_dimmer` |
| **Category** | Output |
| **Description** | Sets fixture dimmer/intensity channel. |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |
| `signal` | Signal (C >= 1) |

**Outputs:** None

**Parameters:** None

**Behavior Notes:**
- Uses the first channel (C=0) as the dimmer value.
- Broadcasts N dimension to match selection size (e.g., a single-N signal applies the same value to all fixtures).
- Dimmer values are typically 0.0 (off) to 1.0 (full intensity).

---

### Apply Color

| Field | Value |
|-------|-------|
| **Type ID** | `apply_color` |
| **Category** | Output |
| **Description** | Sets fixture color channels (RGB or RGBA). |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |
| `signal` | Signal (C >= 3) |

**Outputs:** None

**Parameters:** None

**Behavior Notes:**
- If C=3, treats the signal as RGB.
- If C=4, the fourth channel is alpha (tint strength).
- Broadcasts N and T dimensions to match the selection and time range.

---

### Apply Position

| Field | Value |
|-------|-------|
| **Type ID** | `apply_position` |
| **Category** | Output |
| **Description** | Sets moving light pan/tilt angles. |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |
| `pan` | Signal (degrees) |
| `tilt` | Signal (degrees) |

**Outputs:** None

**Parameters:** None

**Behavior Notes:**
- Disconnected pan or tilt inputs produce NaN, which tells the compositing system to hold the previous position.
- Angles are absolute (not relative to current position).

---

### Apply Strobe

| Field | Value |
|-------|-------|
| **Type ID** | `apply_strobe` |
| **Category** | Output |
| **Description** | Sets fixture strobe/shutter effect. |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |
| `signal` | Signal (C >= 1) |

**Outputs:** None

**Parameters:** None

**Behavior Notes:**
- Value 0.0 = strobe off (open shutter).
- Value 1.0 = maximum strobe rate.
- Intermediate values scale the strobe speed linearly.

---

### Apply Speed

| Field | Value |
|-------|-------|
| **Type ID** | `apply_speed` |
| **Category** | Output |
| **Description** | Sets gobo/pattern rotation speed (binary: frozen or fast). |

**Inputs:**

| Port | Type |
|------|------|
| `selection` | Selection |
| `speed` | Signal (C >= 1) |

**Outputs:** None

**Parameters:** None

**Behavior Notes:**
- Thresholded output: speed > 0.5 produces 1.0 (fast rotation), otherwise 0.0 (frozen).
- Binary behavior simplifies control -- there is no gradual speed adjustment.

---

## Common Pattern Recipes

The following examples show how nodes are typically composed to create lighting effects.

### Basic Beat Pulse

```
beat_clock --> beat_envelope --> gradient --> apply_color
                                   ^
select --------------------------> apply_color
color (red) ----> gradient.start_color
```

A beat envelope drives a gradient that fades between a color and black on each beat. The select node targets a set of fixtures, and apply_color writes the result.

### Spatial Chase

```
select --> get_attribute(normalized_index) --> math(subtract) --> modulo(1.0) --> falloff --> gradient --> apply_color
beat_clock --> ramp -----------------------------> math(subtract)                                            ^
                                                                                               select ----> apply_color
```

The `normalized_index` gives each fixture a 0-1 position. Subtracting the monotonically increasing time ramp creates a traveling wave. The modulo wraps it into a repeating cycle, and falloff shapes the transition width.

### Music-Reactive Strobe

```
audio_input --> stem_splitter --> frequency_amplitude([20, 200]) --> threshold(0.8) --> apply_dimmer
                 (drums_out)                                                              ^
                                                                                 select --+
```

Isolates the kick drum frequency range from the drums stem, then triggers full brightness on strong hits using a threshold.

### Circular Chase

```
select(circular) --> get_attribute(angular_position) --> math(subtract) --> modulo(1.0) --> falloff(width=0.3) --> apply_dimmer
beat_clock --> ramp ----------------------------------------> math(subtract)                                         ^
                                                                                                            select --+
```

Uses `angular_position` (PCA + RANSAC circle fit) to create a rotating point of light on circular fixture arrangements. The falloff width controls how wide the "beam" is as it sweeps around the circle.

### Harmonic Color Wash

```
audio_input --> harmony_analysis --> chroma_palette --> apply_color
                                                          ^
                                                 select --+
```

Colors follow the song's chord progression. The harmony analysis extracts chroma features, and the chroma palette maps them to colors (C major = red, G major = cyan, etc.).
