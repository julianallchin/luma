# Live control: a proposal

Status: proposal, not built. Covers busking on the Perform screen: faders, pads, MIDI, OSC and any future sensor.

## What consoles do, and what hurts

Every console has the same core idea under different names: a **playback**, which is some content plus a fader whose job you pick. grandMA3 calls the jobs "fader functions" (Master, XFade, Temp, Rate, Speed), Hog calls them "control sets", and Titan's default fader scales effect size. Around the playbacks sit **masters**: grand, group, speed/BPM (absolute) and rate (relative). Separate effects stay in sync by sharing a speed master. Layering is HTP for intensity, LTP for everything else, and priorities on top.

The pain points are consistent:
- **Too many object types and modes.** MA has a four-way recipe/preset precedence. Hog has five control sets per master.
- **Pages and banks on fixed hardware.** Faders without motors don't match their levels after a page change.
- **Hard limits.** Titan allows one speed source per playback; Hog's tap only works on physical masters.
- **Mappings that break.** They break on rename, get lost on restart (Resolume), or show units as 0–127 (Ableton macros).
- **Unclear precedence** between overrides and scripted shows (SoundSwitch, Onyx).

The best ideas come from outside lighting:
- **Bitwig:** one modulator drives many targets, each with its own amount.
- **TouchDesigner:** inputs are named channels, bound by name.
- **ChamSys MagicQ:** an execute grid whose items link to existing objects rather than copying them.

## The proposal: controls are wires

Luma already has the right core: a pure graph, where "anything that varies is a wire". Live control adds one value type and one node kind, and needs no new mode system.

### 1. Controls

A **control** is a named live value in a venue, such as `drop`, `front level`, `hue`, `strobe` or `glove.x`. Its kinds:
- number (0–1)
- toggle
- trigger (momentary)
- vector
- color

A control has a default, because "unwired is not zero". It has no units and no range. Those belong to whatever reads it.

### 2. The `Control` node

A new ClipGraph node kind outputs a control's value. It takes a `min`/`max` (or a Curve wired after it), so one fader drives many parameters, each with its own amount, the way Bitwig does.

A fader that "controls a pattern" is just a wire from that pattern's parameter to a Control node. Anything a graph can express, a fader can drive, and that includes score clips: a score can read `energy` and the DJ can push it.

### 3. Bindings

A **binding** maps an input to a control:
- **Input:** anything that sends named channels: a MIDI CC or note, an OSC address, a mixer fader, a deck field such as `deck1.phase` or `master.bpm`, an on-screen fader or pad.
- **Shaping:** curve, invert, and soft takeover for faders without motors.
- **Pad behaviour:** momentary, toggle, or radio within a set.
- **Modifiers:** the existing `ModifierDef` idea, so Shift-style layers replace pages.

Bindings target control **ids**, so a rename can't break them. Feedback runs back out of the same binding, to LEDs and motor faders.

OSC is the universal adapter. A glove, a phone or TouchOSC all send OSC today, and rkbx_link already does. So "support a glove" means "bind its OSC addresses". Nothing glove-specific gets built.

### 4. Live clips

A **live clip** is an ordinary clip (same `ClipGraph`, selection, blend mode, z) that lives on the Perform surface instead of a track:
- It runs on the **live beat clock**: the master deck's tempo and phase, or tap.
- Its `level` is a control, so its pad and fader come for free.
- It has a rate setting (×¼ to ×4).

Live clips composite over the deck mix with the existing blend modes. That gives one precedence rule (z and blend mode), which is the same rule the editor already uses.

Masters disappear as a concept:
- **Group master:** a live clip on `front_wash`, in multiply, whose brightness is wired to the `front level` control.
- **Blackout:** the same on `all`.
- **Colour override:** a Color clip in replace, at the top.

That replaces `ManualLayerState`, `MidiAction::SetIntensity` and the post-mix dimmer pass with things users already know.

### 5. The surface

The Perform screen is the deck cards plus a grid of live clips and on-screen faders. Every cell links to a control or clip; none is a copy. **MIDI learn** works by touching the hardware, then touching the cell.

## What it takes in the engine

- **A control frame.** Today the only runtime argument is beats; changing a value means recompiling the scene. Instead, `Control` compiles to a slot index, and `evaluate_batch(beats, &ControlFrame)` reads slots. Values change at 0 cost, and seek-safety holds, because a frame stays a pure function of its inputs plus the control snapshot.
- **Tempo integration moves to the host.** Clock periods are pre-integrated over the clip span, so a live rate change would need a recompile. The host should own beat integration (`beats = ∫ tempo × rate`), for scores and live clips alike, and the graph stays pure in beats.
- **One input hub.** It replaces the two near-identical MIDI managers (controller and mixer), which duplicate port, learn and reconnect code. Mixer faders become ordinary inputs bound to controls. That is also where deck volume should come from: today's mixer fader state is computed and never used.

## Phases

1. Control node and control frame, on-screen faders, then the input hub with MIDI and OSC learn. Delete the manual layer and its MIDI actions.
2. Live clips, the live beat clock, pad modes.
3. Feedback (LEDs, motor faders), modifiers, device templates.
4. Later: live audio input, MIDI 2.0 property exchange.

## Other things worth redesigning

1. **Patterns can't be reused.** Every clip owns its graph, and a clip can't expose inputs, even though the kernel has `Binding::Input`. Busking needs a library of reusable looks. Exposed clip inputs and controls are the same mechanism: a saved pattern's knobs are its exposed inputs, and binding one to a control makes it live.
2. **Audio features are offline only.** They are analysed per track and normalised over the whole track. For unscripted tracks, a live audio band-energy input is the "autoloop" fallback SoundSwitch users expect.
3. **The docs have drifted.** The www architecture docs describe forms and say a clip never holds its own graph, which is the opposite of the code. The node reference names a generator that doesn't exist.
4. **Dead code:**
   - `PatternArgType`, `PatternArgDef` and `ParamOption` are unused.
   - The `clips.graph` and `inputs_json` columns are unused.
   - Identify hardcodes 16 heads.
   - The controller writes dark frames to output directly, bypassing the render loop.
