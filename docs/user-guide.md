# Luma User Guide

## Why Luma Exists

Here is the core problem with lighting today: **lighting is not portable between venues.**

Traditional lighting consoles record specific DMX instructions for specific hardware. They say things like "set channel 47 to 200, channel 48 to 150, channel 49 to 255." Those numbers only mean something if channel 47 is the red channel on a particular moving head in a particular position on a particular truss. Move to a different venue with different gear, and your entire show is useless. You have to reprogram everything from scratch.

This creates a real problem for the people who need lighting the most:

- **Small artists** (DJs playing frats, clubs, weddings) can't afford to program custom lighting for every gig. They show up, plug in their decks, and whatever lights are in the room either sit there doing nothing or run some generic auto-program that has no relationship to the music.
- **Professional lighting designers** charge thousands of dollars per venue, per show. For a touring DJ doing 3 different clubs a week, that's simply not feasible.
- **Most small events** end up with either no lighting at all or terrible static washes -- a single color that sits there all night.

Luma's answer is a fundamental shift in how lighting shows are described.

**Lighting should be procedural, not programmatic.** Instead of recording "turn fixture #101 to 50% brightness at the 32nd beat," Luma records intent: "play a red circular pulse on the front wash lights." The venue interprets that intent using whatever hardware is available.

Think of it like the difference between sheet music and an audio recording. Sheet music says "play a C major chord" -- it works on piano, guitar, synthesizer, anything. A recording is locked to the specific instrument that played it. Luma is sheet music for lighting. Your creative work travels with you. The venue provides the instruments.

---

## How It Works: The Big Picture

Luma separates **what you want** from **how to do it** using two distinct layers:

1. **Your Library** (`luma.db`) -- Your creative identity. Your tracks, your patterns, your annotations. Everything here is hardware-agnostic. It travels with you on a USB drive or syncs through the cloud. It never references a specific DMX channel or fixture model.

2. **Venue Projects** -- How your library gets translated into a specific room's equipment. A venue project knows what lights are in the room, where they are physically positioned, and what they can do. When you arrive at a new gig, you (or the house technician) set up a venue project once, and then every track in your library automatically works in that room.

The workflow from start to finish:

1. **Define your venue** -- Patch fixtures, set their physical positions
2. **Create fixture groups and tag them** -- This is the magic that makes portability work
3. **Import tracks** -- From Engine DJ or audio files
4. **Define patterns** -- Build reusable light behaviors as visual node graphs
5. **Annotate your tracks** -- Place patterns on a timeline, layer them, set blend modes
6. **Plug in your Denon DJ deck and perform** -- Luma syncs to your playback in real-time

The rest of this guide walks through each step in detail.

---

## Step 1: Define Your Venue

A venue is a project container representing a physical space. It holds all the information Luma needs to know about the room: what lights are installed, where they are, and what they can do.

### Patching Fixtures

Patching is the process of telling Luma what lights exist in the room and how they're connected to the DMX system. For each fixture you add:

1. **Browse the fixture library.** Luma includes a built-in fixture library containing thousands of fixture definitions sourced from the QLC+ community database. Search by manufacturer and model name. Each definition describes the fixture's capabilities: which channels control color, which control movement, which control the strobe, and so on.

2. **Set the DMX address.** Every fixture in a DMX system has a starting address (1 through 512) within a universe. A universe is a group of 512 channels -- the standard DMX-512 protocol. If your fixture uses 16 channels and starts at address 33, it occupies channels 33 through 48. If your venue has more than 512 channels of fixtures, you use multiple universes (universe 1, universe 2, etc.).

3. **Select the mode.** Most professional fixtures have multiple operating modes. A moving head might offer a "16-channel" mode with fine pan/tilt control and separate color mixing, or a compact "8-channel" mode with fewer features. The mode determines how many DMX channels the fixture uses and what each channel does. Pick the mode that matches how the fixture is actually configured on site.

4. **Set the 3D position.** This is where Luma diverges from traditional consoles. You specify where the light physically is in the room using X, Y, Z coordinates in meters:
   - **X** -- Left/right (negative is stage-left, positive is stage-right)
   - **Y** -- Up/down (0 is floor level, positive is up toward the ceiling)
   - **Z** -- Front/back (where the audience is vs. where the DJ booth is)

5. **Set the 3D rotation.** How the fixture is oriented in space, specified as roll, pitch, and yaw angles. This matters for ceiling-mounted fixtures (which are upside-down compared to floor fixtures) and for pixel bars that might be mounted vertically instead of horizontally.

### Why Positions Matter

Unlike traditional consoles where you just assign DMX addresses and manually program everything, Luma needs to know WHERE your lights are in physical space. This is what makes spatial effects possible:

- **Linear chases** -- A wave of light that sweeps left-to-right across the room. Luma calculates which fixture is leftmost, which is rightmost, and animates them in spatial order. You don't need to know the fixture numbers -- just their positions.
- **Circular patterns** -- If lights are arranged in a ring (on a circular truss, for example), Luma detects this geometry and can animate them in rotational order.
- **Position-aware effects** -- Closer lights can react differently than far ones. Front-of-house wash lights can behave differently from rear truss spots, and the system knows the difference because it knows where everything is.

### Multi-Head Fixtures

Some fixtures contain multiple independently-controllable segments. An LED pixel bar, for example, might be a single physical unit with 12 individually-addressable RGB pixels. In DMX terms, each pixel has its own set of channels (red, green, blue), but they all share a single DMX starting address because they're one fixture.

Luma calls these segments "heads." When you patch a multi-head fixture, Luma automatically reads the head layout from the fixture definition and computes the 3D position of each head. If you place a 1-meter pixel bar at position (2, 3, 0) and rotate it 90 degrees, Luma calculates where each individual pixel sits in space. This means spatial effects work at the pixel level, not just the fixture level.

---

## Step 2: Tag Groups

This is the core innovation that makes shows portable between venues. If you understand nothing else about Luma, understand this section.

### What Are Groups?

Groups are logical collections of fixtures organized by their **role and position** in the venue, NOT by their DMX address. When you create a group, you give it a name that describes what those lights do in the room:

- "Front Wash" -- the wash lights pointing at the audience from the front
- "Back Truss" -- the moving heads on the rear truss
- "Floor Ring" -- uplights arranged in a circle on the floor
- "DJ Booth" -- lights illuminating the DJ

Groups are a venue-level concept. They exist inside a specific venue project. The same physical lights in two different venues will have different group names but might share the same tags.

### Spatial Axes

Each group has three spatial axis values, which describe where the group sits in the room on a normalized scale:

- **Left/Right** (-1 to +1) -- Stage left to stage right
- **Front/Back** (-1 to +1) -- Downstage (audience) to upstage (back wall)
- **Above/Below** (-1 to +1) -- Floor level to ceiling

These values are auto-calculated from the average positions of the fixtures in the group, but you can manually adjust them. They're used by the auto-tagging system and by patterns that want to treat groups differently based on their spatial role.

### Tags

Tags are labels attached to groups that patterns use for fixture selection. They're the bridge between your hardware-agnostic creative work and the venue's specific equipment.

There are two categories of tags:

**Auto-generated spatial tags** (computed from fixture positions and group axes):
- `left`, `right`, `center` -- based on the Left/Right axis
- `front`, `back` -- based on the Front/Back axis
- `high`, `low` -- based on the Above/Below axis
- `circular` -- detected when fixtures form a ring shape (Luma uses a PCA + RANSAC circle-fitting algorithm that works even if the fixtures aren't perfectly flat -- they can be on a tilted truss)
- `all` -- a universal tag that matches every fixture in the venue

**Auto-generated capability tags** (detected from fixture definitions):
- `has_color` -- the fixture has RGB color mixing or a color wheel
- `has_movement` -- the fixture has pan and tilt (it's a moving head or scanner)
- `has_strobe` -- the fixture has a shutter/strobe channel

**User-defined purpose tags:**
- Custom labels you create, like `blinder`, `wash`, `spot`, `chase`, `accent`, etc.
- These let you express intent: "these are the lights I want to use for blinder effects" or "these are my chase fixtures"

### Why Tags Are the Magic

When you build a pattern (Step 4), you don't say "animate fixtures 1 through 8." You say "animate everything tagged `circular`" or "strobe everything tagged `front` and `has_movement`."

Now your pattern works on ANY venue:

- **Small club** with 8 par cans in a ring? The `circular` tag picks them up automatically.
- **Stadium** with 200 moving heads on a circular truss? Same pattern, same tag. It animates all 200 lights in their circular arrangement, with zero changes to your pattern.
- **Dive bar** with 4 random LED pars on the ceiling? No `circular` fixtures exist, so the pattern gracefully finds the best available alternative.

This is the sheet-music analogy in action. Your pattern says "play a C major chord" and the venue provides whatever instruments it has.

### Tag Expressions

Tag expressions let you write boolean logic to precisely target fixtures. They support the following operators:

- `&` (AND) -- both conditions must be true
  - `front & has_color` -- front fixtures that have RGB control
- `|` (OR) -- either condition can be true
  - `left | right` -- both sides of the room (but not center)
- `~` (NOT) -- exclude matching fixtures
  - `circular & ~blinder` -- circular fixtures that aren't blinders
- `>` (FALLBACK) -- try the first option, fall back to the second if nothing matches
  - `has_movement > has_color` -- prefer moving heads; if no moving heads exist in the venue, use color fixtures instead

Parentheses work for grouping: `(front | back) & has_color` means "front or back fixtures, but only if they have color."

The `all` tag is the default if you leave the expression empty.

---

## Step 3: Import Tracks

Luma needs to know about your music before you can annotate it with lighting. There are two ways to get tracks in.

### From Engine DJ

Luma integrates directly with Engine DJ, the desktop software used by Denon DJ hardware. When you connect to your Engine DJ library:

- Browse your existing Engine DJ collection
- Select tracks to import
- Luma references the audio file in place (no copying) and runs its analysis pipeline

This is the recommended workflow if you use Denon DJ gear, since Luma's live performance mode also connects to Denon hardware via StageLinQ.

### Audio Analysis Pipeline

When a track is imported, Luma runs several analysis passes to extract musical information that patterns can use to make your lights react to the music:

1. **Beat Detection** (beat_this neural network) -- Finds individual beat positions, calculates BPM, and identifies downbeats (the first beat of each bar). This powers beat-synced effects like pulses, chases, and strobes.

2. **Stem Separation** (Demucs htdemucs model) -- Splits the full track into four separate audio stems: **drums**, **bass**, **vocals**, and **other** (synths, guitars, pads, etc.). This lets patterns react to specific instruments. Your lights can pulse to the kick drum while ignoring the vocals, or change color based on the bass line.

3. **Harmonic Analysis** (consonance-ACE model) -- Detects chord progressions and key changes throughout the track. The analysis runs on the bass + other stems (excluding drums and vocals for cleaner results) and produces frame-by-frame probabilities for each of the 12 musical pitch classes. This enables harmony-reactive effects where colors shift with the chords.

4. **Waveform Generation** -- Creates visual waveform data for the timeline editor so you can see the audio shape while placing patterns.

These analyses run in parallel where possible. Beat detection and stem separation happen simultaneously, while harmonic analysis waits for stems to finish (since it uses the separated stems for cleaner results).

---

## Step 4: Define Patterns

Patterns are the heart of Luma. They are reusable, venue-portable light behaviors defined as visual node graphs. If you've used Unreal Engine Blueprints, Blender shader nodes, or TouchDesigner, the concept is similar: you connect processing nodes with wires to build data flow pipelines that transform inputs (beats, audio, spatial position) into outputs (dimmer values, colors, movement).

### The Signal: Luma's Core Data Type

Before diving into node types, you need to understand the **Signal** -- the fundamental data type that flows through the graph.

A Signal is a 3D tensor with three dimensions:

- **N (Spatial)** -- One value per fixture in the selection. If you've selected 10 fixtures, N = 10. If your pattern doesn't care about individual fixtures (e.g., a single color for everyone), N = 1.
- **T (Temporal)** -- Time samples across the pattern's duration. For animated effects, T might be 256 or more steps. For static values, T = 1.
- **C (Channel)** -- Data channels per sample. For a dimmer signal, C = 1 (just brightness). For a color signal, C = 4 (red, green, blue, alpha).

Think of it as a spreadsheet: rows are fixtures, columns are time steps, and each cell can hold multiple values.

**Broadcasting** is key to how Signals work together. When you combine two signals of different sizes, the smaller one automatically expands to match:
- A color signal with N=1 (one color for all fixtures) combined with a dimmer signal with N=10 (per-fixture brightness) will apply that same color to all 10 fixtures, each at their individual brightness.
- A signal with T=1 (constant over time) combined with a signal with T=256 (animated) will repeat the constant value at every time step.

This is the same broadcasting concept used in NumPy and PyTorch, if you're familiar with those.

### The Pattern Editor

The pattern editor presents a visual canvas where you place nodes and connect their ports with wires. Each node has:

- **Input ports** (left side) -- Data flowing in
- **Output ports** (right side) -- Data flowing out
- **Parameters** (on the node body) -- Configuration knobs and settings

Data flows left to right. You wire an output port of one node to an input port of another to pass data between them.

### Node Reference

Here is every node type available in Luma, organized by category.

#### Input Nodes

| Node | Description |
|------|-------------|
| **Audio Input** | Provides the audio waveform for the current track segment. This is the entry point for any audio-reactive effect. |
| **Beat Clock** | Provides the beat grid (BPM, beat positions, downbeats) for the current track. This is the entry point for any beat-synced effect. |
| **Pattern Args** | Exposes the pattern's arguments (color pickers, sliders, selection choosers) as signal outputs. This is how you make patterns configurable when placing them on the timeline. |

#### Audio Nodes

| Node | Description |
|------|-------------|
| **Stem Splitter** | Takes an Audio Input and splits it into four separate audio streams: Drums, Bass, Vocals, and Other. Each output can be independently processed. |
| **Frequency Amplitude** | Extracts the amplitude (loudness) of a specific frequency range from audio. Use this to isolate the kick drum (20-100 Hz), snare (200-2000 Hz), hi-hats (8000-16000 Hz), or any custom range. Outputs a Signal. |
| **Lowpass Filter** | Removes frequencies above a cutoff point. Useful for isolating bass and sub-bass content before feeding into Frequency Amplitude. |
| **Highpass Filter** | Removes frequencies below a cutoff point. Useful for isolating high-frequency content like hi-hats and cymbals. |
| **Harmony Analysis** | Analyzes audio for harmonic content. Outputs a 12-channel chroma Signal where each channel represents the confidence (0 to 1) that a specific pitch class (C, C#, D, D#, E, F, F#, G, G#, A, A#, B) is present at each moment. |

#### Generator Nodes

| Node | Description |
|------|-------------|
| **Beat Envelope** | The workhorse for beat-synced effects. Generates an ADSR (Attack-Decay-Sustain-Release) envelope triggered on each beat. Parameters control subdivision (1 = every beat, 0.5 = every half-beat, 2 = every 2 beats), attack/decay/sustain/release timing, sustain level, curve shapes, and amplitude. Can be set to trigger only on downbeats. |
| **Scalar** | Outputs a constant numeric value. Use this to set fixed values like brightness levels, speeds, or angles. |
| **Color** | Outputs a constant RGBA color signal. Provides a color picker in the editor. |
| **Time Ramp** | Generates a linear ramp from 0 to N beats over the pattern's duration. Useful as a time base for animations -- combine with Modulo to create repeating cycles. |
| **Linear Ramp** | Generates a linear ramp between two arbitrary signal values over the pattern duration. More flexible than Time Ramp when you need custom start/end values. |
| **Sine Wave** | Generates a smooth sine wave oscillation. Parameters control frequency (Hz), phase offset (degrees), amplitude, and DC offset. Useful for smooth, organic motion. |
| **Noise** | Generates 3D fractal noise. Accepts time, X, and Y coordinates as inputs. Parameters control scale, octave count, amplitude, and offset. Use this for organic, non-repeating variation -- every fixture can sample the noise at a different spatial coordinate for a natural "shimmer" effect. |
| **Orbit** | Generates circular or elliptical position coordinates (X, Y, Z) that trace a path in 3D space. Parameters control center position, radii, speed (in cycles per beat), and plane tilt. Use with Look At Position to make moving heads track a point orbiting the room. |
| **Random Position** | Generates random 3D positions. A new random position is picked each time the trigger signal changes value. Parameters define the bounding box (min/max for X, Y, Z). |

#### Selection Nodes

| Node | Description |
|------|-------------|
| **Select** | The entry point for fixture targeting. Takes a tag expression (like `front & has_color`) and outputs a Selection -- the set of fixtures matching that expression in the current venue. Also has a spatial reference mode that can evaluate positions per-group (group_local) or globally. |
| **Get Attribute** | Extracts a spatial attribute from a Selection, producing a per-fixture Signal. Available attributes: `index` (0, 1, 2...), `normalized_index` (0.0 to 1.0), `pos_x`/`pos_y`/`pos_z` (absolute position in meters), `rel_x`/`rel_y`/`rel_z` (relative position 0-1 within the selection's bounds), `rel_major_span` (position along the axis with the largest physical extent), `rel_major_count` (position along the axis with the most distinct heads), `angular_position` (0-1 angle around a fitted circle, using PCA + RANSAC), `angular_index` (index-based circular ordering for equal spacing), `circle_radius` (distance from center). |
| **Random Select Mask** | Given a Selection and a trigger Signal, randomly picks N fixtures and outputs a binary mask (1.0 for selected, 0.0 for unselected). The selection re-rolls each time the trigger value changes. An "avoid repeat" option prevents the same fixture from being picked twice in a row. |

#### Transform Nodes

| Node | Description |
|------|-------------|
| **Math** | Performs element-wise math on two signals with full broadcasting. Operations: add, subtract, multiply, divide, max, min, abs_diff (absolute difference), abs, modulo, circular_distance. |
| **Modulo** | Wraps signal values to the range [0, divisor). Essential for creating looping/repeating animations from a ramp. |
| **Falloff** | Applies a soft falloff curve to a signal in the 0-1 range. Width controls how wide the "hot spot" is; curve controls the shape (linear to exponential). Turns a broad gradient into a sharp pulse. |
| **Normalize** | Rescales a signal to the 0-1 range using its observed min and max values. |
| **Invert** | Reflects a signal around its observed midpoint (turns peaks into valleys and vice versa). |
| **Threshold** | Binarizes a signal: values above the threshold become 1.0, values below become 0.0. |
| **Round** | Quantizes signal values. Operations: round, floor, ceil. |
| **Remap** | Linearly maps values from one range [in_min, in_max] to another [out_min, out_max]. Optionally clamps the output. Useful for converting normalized 0-1 signals to degree ranges for pan/tilt. |
| **Smooth Movement** | Applies a slew-rate limiter to pan/tilt signals, capping the maximum degrees-per-second that values can change. Prevents jarring jumps in moving head positions. |
| **Time Delay** | Shifts a signal in time on a per-fixture basis. Positive values add lag, negative values advance. Use this to create staggered/cascading effects where each fixture triggers slightly after the previous one. |

#### Color Nodes

| Node | Description |
|------|-------------|
| **Gradient** | Maps a scalar signal (0-1) to a color by interpolating between a start color and an end color. This is how you turn a brightness envelope into a color fade -- 0.0 maps to the start color, 1.0 maps to the end color, values in between are blended. Both colors can be connected as signal inputs or set as hex parameters. |
| **Harmonic Palette** | Takes a 12-channel chroma signal (from Harmony Analysis) and maps it to RGB colors using a palette. The default "Rainbow" palette assigns each pitch class a color (C=red, D=orange, E=yellow, F=green, G=cyan, A=blue, B=magenta). The output smoothly blends between colors based on chord probabilities. |
| **Spectral Shift** | Rotates the hue of an input color based on the dominant musical key from a chroma signal. The music literally shifts the color palette. |

#### Analysis Nodes

| Node | Description |
|------|-------------|
| **Harmonic Tension** | Takes a 12-channel chroma signal and computes a single "tension" value (0-1) based on the entropy/spread of chord probabilities. High tension = dissonant or ambiguous harmony (many notes active). Low tension = clear, consonant chord. Use this to drive intensity or strobe rate during tense musical moments. |

#### Movement Nodes

| Node | Description |
|------|-------------|
| **Look At Position** | Computes pan and tilt angles (in degrees) for each selected fixture head to aim at a target point in 3D space. Takes a Selection (for fixture positions) and X, Y, Z target coordinates. Outputs per-fixture Pan and Tilt signals. Use with Orbit or Random Position to make moving heads track a point. |

#### Output Nodes

Output nodes are the final stage of a pattern graph. They take a Selection (which fixtures) and a Signal (what values) and write the result to the compositing system. A pattern can have multiple output nodes targeting different capabilities and different fixture groups.

| Node | Description |
|------|-------------|
| **Apply Dimmer** | Sends a 1-channel intensity signal to the selected fixtures' dimmer channels. Values range from 0.0 (off) to 1.0 (full brightness). |
| **Apply Color** | Sends a 4-channel RGBA signal to the selected fixtures' color mixing channels. Values are 0.0-1.0 per channel. The alpha channel controls opacity for compositing with layers below. |
| **Apply Strobe** | Sends a 1-channel strobe signal to the selected fixtures' shutter channels. 0.0 = shutter closed/no strobe, 1.0 = maximum strobe rate. |
| **Apply Position** | Sends pan and tilt signals (in degrees, centered at 0) to the selected fixtures' movement channels. Pan and tilt are separate inputs so you can control each axis independently. |
| **Apply Speed** | Controls the movement speed of selected fixtures. Binary: values above 0.5 = fast movement, below 0.5 = frozen. Use this to freeze moving heads in position during slow sections and release them during energetic drops. |

#### View Nodes (Debugging)

| Node | Description |
|------|-------------|
| **View Signal** | Displays the incoming signal as a waveform visualization on the node canvas. Shows per-fixture curves (if N > 1) or per-channel curves (if C > 1). Essential for debugging complex graphs. |
| **Mel Spectrogram** | Shows a mel-frequency spectrogram of the incoming audio, optionally overlaid with beat grid markers. Useful for visually correlating audio features with your signal processing. |

### Building a Pattern: Walkthrough

Here is a complete example of building a simple beat-synced dimmer hit from scratch.

**Goal:** All fixtures flash on every beat with a sharp attack and linear decay -- a classic "4 on the floor" pulse.

Every pattern starts with a **Beat Clock** and **Audio Input** node already on the canvas. You do not need to add these.

1. **Add a Beat Envelope node.** Connect Beat Clock's "Beat Grid" output to Beat Envelope's "Beat Grid" input. Set the envelope parameters:
   - Subdivision: 1 (trigger on every beat)
   - Attack: 0 (instant rise to peak)
   - Decay: 1 (linear fade over the full beat)
   - Sustain: 0 (no hold)
   - Release: 0 (no tail)

   This produces a sawtooth shape: the signal jumps to 1.0 on each beat and ramps linearly down to 0.0 by the next beat.

2. **Add a Select node.** Set the tag expression to `all`. This targets every fixture in the venue.

3. **Add an Apply Dimmer node.** Connect the Select output to Apply Dimmer's "Selection" input, and the Beat Envelope output to Apply Dimmer's "Signal" input.

That's the complete graph. Three nodes (plus the two auto-added inputs), two wires. The result: every fixture in the venue flashes to full brightness on each beat and fades to black by the next beat.

### Advanced Technique: Spatial Chases

A spatial chase is when lights activate in sequence across physical space -- like a wave of light sweeping from left to right across the room.

The key insight is combining a **time signal** with a **spatial position signal** so that each fixture hits its peak at a different moment:

1. **Select** (tag: `all`) outputs a Selection.
2. **Get Attribute** (attribute: `rel_x`) reads the Selection and outputs a per-fixture signal where each fixture gets a value from 0.0 (leftmost) to 1.0 (rightmost) based on its physical X position.
3. **Time Ramp** connected to **Beat Clock** creates a signal that counts up by 1 for each beat.
4. **Math (subtract)** -- Subtract the spatial position (from Get Attribute) from the time ramp. Each fixture now has a time-shifted version of the ramp. The leftmost fixture (position 0.0) sees the ramp unchanged. The rightmost fixture (position 1.0) sees the ramp delayed by one unit.
5. **Modulo** (divisor: 1.0) -- Wraps the subtracted value to 0-1, creating a repeating sawtooth wave per fixture.
6. **Falloff** (width: 0.3) -- Turns the broad sawtooth into a narrow pulse. Each fixture now has a sharp pulse that hits at a different time.
7. **Apply Dimmer** -- Connect the Select output and the Falloff output.

Result: A wave of light that sweeps across your fixtures in spatial order, repeating every beat. Because it uses `rel_x` (relative X position), it automatically adapts to any venue layout. Eight fixtures or eighty -- the chase just works.

For circular chases, use `angular_position` or `angular_index` instead of `rel_x`. For vertical chases, use `rel_y`.

### Advanced Technique: Music-Reactive Colors

**Chord-driven color:**
1. **Audio Input** feeds into **Harmony Analysis**, which outputs a 12-channel chroma signal.
2. **Harmonic Palette** maps the chroma probabilities to colors. When the music plays a C major chord, the output shifts toward red. A D chord shifts toward orange. The transitions are smooth because the chroma values are probabilistic, not hard switches.
3. **Apply Color** sends the result to your fixtures.

**Kick-drum reactive strobes:**
1. **Audio Input** feeds into **Stem Splitter** to separate the drums.
2. **Drums** output feeds into **Frequency Amplitude** set to the kick drum range (20-100 Hz).
3. The amplitude signal drives **Apply Dimmer** or **Apply Strobe** for kick-synced intensity.

**Harmonic tension as energy:**
1. **Harmony Analysis** feeds into **Harmonic Tension**, which outputs a 0-1 "tension" value.
2. High tension (complex chords, key changes) drives brightness, strobe rate, or movement speed.
3. Low tension (clear major/minor chords) creates calm, gentle lighting.

### Advanced Technique: Moving Head Choreography

**Tracking a point orbiting the room:**
1. **Orbit** generates circular X, Y, Z coordinates (set center, radius, speed).
2. **Select** targets fixtures with `has_movement`.
3. **Look At Position** computes per-fixture pan and tilt angles so every moving head aims at the orbiting point.
4. **Smooth Movement** limits the pan/tilt speed so heads move gracefully instead of snapping.
5. **Apply Position** sends the smoothed pan/tilt to the fixtures.

**Random position jumps on each beat:**
1. **Beat Envelope** triggers on each beat.
2. **Random Position** generates a new X, Y, Z target each time the beat envelope value changes.
3. **Look At Position** computes pan/tilt for the new target.
4. **Apply Position** sends the result.
5. **Apply Speed** can freeze the heads between beats (connect a binary signal: 0 during sustain, 1 on beat hit).

### Pattern Arguments

Patterns can expose configurable arguments -- parameters that appear when you place the pattern on a track's timeline. This makes a single pattern infinitely reusable:

- **Color argument** -- A color picker appears when placing the pattern. The same "Beat Pulse" pattern can be red, blue, green, or any color.
- **Scalar argument** -- A number slider appears. Control speed, intensity, width, or any numeric parameter.
- **Selection argument** -- A tag expression field appears. Let the user choose which fixtures to target each time the pattern is placed.

To use pattern arguments, add a **Pattern Args** node to your graph. Define the arguments you want to expose (name, type, default value). The Pattern Args node's outputs become signals you can wire into the rest of your graph -- for example, wire a Color argument output into a Gradient node's color input.

---

## Step 5: Annotate Your Tracks

The Track Editor is a timeline view where you place patterns onto your tracks. This is where you choreograph your light show.

### Annotations

An annotation (also called a "score" internally) is a pattern placement on the timeline. Each annotation has:

- **Start/End Time** -- When the pattern activates and deactivates, specified in seconds along the track's timeline.
- **Pattern** -- Which pattern to run during this time window.
- **Z-Index** -- Stacking order for compositing. Higher z-index means the annotation is "on top" of lower ones. When two annotations overlap in time, the one with the higher z-index is composited on top.
- **Blend Mode** -- How this annotation's output combines with the layers below it (see next section).
- **Arguments** -- Values for the pattern's exposed arguments. If the pattern has a Color argument, you set the specific color here. If it has a Selection argument, you set the tag expression here.

You can place as many annotations as you want on a track. They can overlap, stack, and interact through the compositing system.

### Compositing and Blend Modes

When multiple annotations overlap in time, Luma resolves the result using a compositing system inspired by Photoshop's layer blending. Layers are processed bottom-to-top by z-index, and each layer's blend mode determines how it combines with the accumulated result below it.

**Available blend modes:**

- **Replace** -- The top layer completely overwrites the bottom. What you see is what you get. Use this when you want a pattern to take full control during its time window.

- **Add** -- Values are summed, clamped to 1.0 maximum. Lights get brighter. If the base layer has a fixture at 0.3 brightness and the top layer adds 0.5, the result is 0.8. Use this to layer multiple effects that accumulate -- a subtle background wash plus a beat pulse plus accent hits.

- **Multiply** -- Values are multiplied together. This always darkens (since multiplying by anything less than 1.0 reduces the value). Use this for "ducking" effects -- create a pattern that outputs low values on the kick drum to darken everything else in sync with the beat.

- **Screen** -- The inverse of Multiply. Values are combined using the formula `1 - (1-base) * (1-top)`. This always lightens. Useful for adding glow or bloom effects without washing out the base layer.

- **Max** -- Takes the brightest value at each point. If the base layer has red at 0.8 and the top layer has red at 0.3, the result is 0.8. Use this when you want multiple patterns to "compete" and the strongest signal wins.

- **Min** -- Takes the dimmest value at each point. Use this for masking -- a pattern that sets fixtures to 0.0 will cut through anything below it.

- **Value** -- The top layer's luminance (brightness) controls how much it overrides the base. Bright areas of the top layer dominate; dark areas let the base show through. This creates a natural "intensity-as-opacity" effect.

**Practical compositing examples:**

Layer a slow color wash at z-index 0 (Replace) as your base look. Add a beat-synced dimmer pulse at z-index 1 (Multiply) to create rhythmic breathing. Add a strobe hit at z-index 2 (Add) for impact moments. The wash provides the color palette, the multiply creates rhythm by darkening on beats, and the additive strobe punches through everything at key moments.

### Color Compositing Details

For color (RGBA) blending, Luma applies the blend mode to the RGB channels and then does alpha compositing on top. This means:

- A pattern outputting a color with alpha = 0.5 will blend at 50% opacity with the layer below, regardless of blend mode.
- A pattern outputting alpha = 1.0 at full opacity will apply the blend mode at full strength.
- A pattern outputting alpha = 0.0 will be completely transparent (invisible).

This gives you fine-grained control: use alpha to control how strongly a layer participates in the composite, and use blend modes to control *how* it participates.

### Pre-Positioning

During gaps between annotations -- moments where no pattern is active for a set of fixtures -- Luma automatically handles the transition. Moving head fixtures in particular benefit from this: Luma can move them to their starting position for the next pattern during the gap, so they're ready when the next pattern starts. This prevents the jarring "snap" of a moving head jumping to a new position at the instant a pattern begins.

---

## Step 6: Perform

### StageLinQ Connection

Connect your Denon DJ deck (Prime 4, SC6000, LC6000, or other StageLinQ-capable hardware) to your computer via Ethernet. Luma discovers the device automatically using the StageLinQ protocol -- Denon's proprietary networking system for DJ equipment.

Once connected, Luma receives real-time data:

- **Current track** -- Which track is loaded on each deck
- **Playback position** -- The exact position in the track, updated continuously
- **BPM and beat phase** -- For tight synchronization

**Networking note for macOS users:** If your Mac has both WiFi and Ethernet active, macOS may route link-local traffic (169.254.x.x, which StageLinQ uses) through WiFi instead of Ethernet. Luma handles this by binding its network sockets per-interface, but if you experience connection issues, you may need to adjust your network configuration or temporarily disable WiFi.

### ArtNet DMX Output

Luma broadcasts DMX data over the network using the ArtNet protocol. ArtNet is an industry-standard protocol for transmitting DMX-512 data over Ethernet (UDP packets on port 6454).

**What you need:**

- An ArtNet-compatible DMX node or interface connected to your network. These are small boxes that receive ArtNet packets via Ethernet and convert them to physical DMX-512 signals for your fixtures. Common brands include Enttec, DMXIS, and DMXking.
- Your computer and the ArtNet node on the same network (or connected directly via Ethernet).

Luma supports multiple DMX universes and broadcasts at 60 frames per second.

### Real-Time Rendering Pipeline

During a live performance, here is what happens every frame (60 times per second):

1. **StageLinQ tells Luma** what track is playing on the active deck and the current playback position in that track.

2. **The compositor looks up annotations** for the current playback position. It finds all active patterns (annotations whose start/end times contain the current position).

3. **Active pattern graphs execute.** Each pattern's node graph runs with the current audio and beat context. The graph processes audio signals, evaluates beat positions, computes spatial attributes, and produces output through Apply nodes.

4. **Within each pattern, Apply node outputs are merged.** Multiple Apply nodes in a single pattern (e.g., one for color and one for dimmer) merge their outputs into a single layer. If two Apply nodes try to write the same capability to the same fixture, that's a conflict -- keep your patterns clean with one Apply per capability per fixture group.

5. **Between patterns, layers are composited.** Overlapping patterns are stacked by z-index and blended according to their blend modes. The result is a single set of values for every fixture at the current moment.

6. **Fixture states are mapped to DMX.** The composited values (abstract concepts like "color," "dimmer," "pan," "tilt") are converted to specific DMX channel values based on each fixture's definition. Luma looks up which DMX channels correspond to which capabilities using the fixture definition's channel map. A "set color to red at 80% brightness" becomes "channel 5 = 204, channel 6 = 0, channel 7 = 0, channel 4 = 204" for one fixture and completely different channel numbers for a different fixture model.

7. **DMX is broadcast as ArtNet packets.** The final DMX buffer (one per universe) is packaged into ArtNet UDP packets and broadcast to the network. Any ArtNet node listening on that universe picks it up and sends it to the physical fixtures.

All of this happens in real-time at 60fps. The entire pipeline -- from StageLinQ playback position to ArtNet packet leaving the computer -- runs in Rust for performance.

---

## Appendix A: Complete Node Graph Quick Reference

### By Category

**Input:** Audio Input, Beat Clock, Pattern Args

**Audio:** Stem Splitter, Frequency Amplitude, Lowpass Filter, Highpass Filter, Harmony Analysis

**Generator:** Beat Envelope, Scalar, Color, Time Ramp, Linear Ramp, Sine Wave, Noise, Orbit, Random Position

**Selection:** Select, Get Attribute, Random Select Mask

**Transform:** Math, Modulo, Falloff, Normalize, Invert, Threshold, Round, Remap, Smooth Movement, Time Delay

**Color:** Gradient, Harmonic Palette, Spectral Shift

**Analysis:** Harmonic Tension

**Movement:** Look At Position

**Output:** Apply Dimmer, Apply Color, Apply Strobe, Apply Position, Apply Speed

**View:** View Signal, Mel Spectrogram

---

## Appendix B: Glossary

| Term | Definition |
|------|-----------|
| **Venue** | A project container representing a physical space with patched fixtures. Each venue knows what lights are in the room and where they are. |
| **Fixture** | A physical light unit -- moving head, LED bar, par can, strobe, etc. |
| **Patched Fixture** | A fixture assigned to a venue at a specific DMX universe and address, with a known 3D position and rotation. |
| **Head** | An independently-controllable segment of a multi-head fixture. A pixel bar with 12 pixels has 12 heads. A simple moving head has 1 head. |
| **DMX Universe** | A group of 512 channels. The standard DMX-512 protocol. Multiple universes are used for larger rigs. |
| **Group** | A logical collection of fixtures organized by their role and position (e.g., "Front Wash," "Back Truss"). Groups exist within a venue. |
| **Tag** | A label attached to a group used by patterns for fixture selection. Tags like `front`, `circular`, `has_movement` make patterns venue-portable. |
| **Tag Expression** | A boolean query combining tags with `&` (and), `|` (or), `~` (not), and `>` (fallback) operators. |
| **Pattern** | A reusable light behavior defined as a visual node graph. Patterns are venue-agnostic. |
| **Implementation** | A specific graph version of a pattern. Patterns can have multiple implementations (e.g., a simplified version for venues with limited fixtures). |
| **Signal** | A 3D tensor (N fixtures x T time steps x C channels) -- the core data type flowing through pattern graphs. |
| **Annotation (Score)** | A pattern placement on a track's timeline, with start/end time, z-index, blend mode, and argument values. |
| **Blend Mode** | How overlapping annotations combine: Replace, Add, Multiply, Screen, Max, Min, Value. |
| **Compositing** | The process of combining multiple annotation layers into a single output, using z-index ordering and blend modes. |
| **ArtNet** | An industry-standard network protocol for transmitting DMX data over Ethernet (UDP port 6454). |
| **StageLinQ** | Denon DJ's proprietary protocol for networking DJ equipment. Used by Luma to sync playback position and track information from Denon decks. |
| **ADSR Envelope** | Attack-Decay-Sustain-Release -- a standard envelope shape used by the Beat Envelope node to shape pulses. |
| **Chroma** | A 12-channel signal representing the probability of each musical pitch class (C through B). Used for harmony-reactive effects. |
| **Broadcasting** | Automatic expansion of smaller signals to match larger ones. A signal with N=1 expands to match a signal with N=10 by repeating its single value for all fixtures. |
