# Live control

Status: built: controls and the `control` node, MIDI and OSC bindings (`backend/src/inputs/`), the Perform tab and the Perform set (`backend/src/perform/`, `gpui/crates/app/src/perform*`). Written as the proposal; where the code differs, the code is right. Covers busking on the Perform screen: faders, pads, MIDI, OSC and any future sensor.

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

A fader that "controls a pattern" is just a wire from that pattern's parameter to a Control node. Anything a graph can express, a fader can drive, and that includes the patterns in a score: a score can read `energy` and the DJ can push it.

### 3. Bindings

A **binding** maps an input to a control:
- **Input:** anything that sends named channels: a MIDI CC or note, an OSC address, a mixer fader, a deck field such as `deck1.phase` or `master.bpm`, an on-screen fader or pad.
- **Shaping:** curve, invert, and soft takeover for faders without motors.
- **Pad behaviour:** momentary, toggle, or radio within a set.
- **Modifiers:** the existing `ModifierDef` idea, so Shift-style layers replace pages.

Bindings target control **ids**, so a rename can't break them. Feedback runs back out of the same binding, to LEDs and motor faders.

OSC is the universal adapter. A glove, a phone or TouchOSC all send OSC today, and rkbx_link already does. So "support a glove" means "bind its OSC addresses". Nothing glove-specific gets built.

### 4. The Perform set

A venue's Perform set (`perform_patterns`) holds ordinary patterns: the same `ClipGraph`, selection, blend mode and seed a score's pattern has, with no place on a timeline. Building the set is building a score live.

- **Each pattern plays on its own.** A colour pattern and a dimmer chase on the same lights are two patterns, both on, each at its own level. Where two write the same attribute of the same light, the higher one in the set wins (`z_index`), with the same blend modes the editor uses.
- **A level is built in; the pad is in the graph.** Every pattern has its own level (`perform::level(id)`, 1 until moved). Its pad is the button node its output's `alpha` depends on (`ClipGraph::pad`; the first by id if several), so pressing the pad fades the graph's own alpha, and the set scales that by its level × `perform_master`. A pattern whose pad is all the way off is skipped; a pattern with no pad plays at its level whenever the set is open. A new pattern gets a toggle pad wired into its alpha (`ClipGraph::with_pad`). Every input reaches the pad and the level the same way: an on-screen pad, a MIDI key, an OSC button. The set compiles once; pressing a pad never recompiles.
- **Faders and buttons in a graph belong to their pattern.** A graph's `control` node (a *fader* to the person playing it) and `button` node each give 0–1, the same at every head, and a curve maps that to whatever input they drive. In a Perform pattern each is a value of that pattern, keyed by node id (`perform::node(id, node)`), so two patterns can both have a `speed` fader and renaming a node keeps its bindings. The pattern's cell shows one fader or button per node (`ClipGraph::live_inputs`). In a score the same nodes read the venue's control of their `name`, so a score can read `drop` and the DJ can push it; a button there is a plain gate, 1 while the control is on.
- **Buttons have envelopes.** A button node, the pad among them, plays by its settings, read as a `luma_patterns::press::Press`. Its mode says what a press does: a **toggle** turns it on until the next press, a **flash** only while held. Its envelope (attack, decay, sustain, release, in beats or seconds, shaped linear, smooth, punchy or snap) is what its value follows through the press: a soft wash is a slow attack and release at sustain 1, a hit is a punchy decay at sustain 0, which fires once however long it is held. Beats are counted at the tempo the set plays at: the followed deck's, else the tapped one. The bank keeps only when each button went on and off; its value is a pure function of those and now.
- **One at a time.** A page's `one_at_a_time` rows hold patterns that take turns: turning one on releases the row's others, which fade out by their own release. The control bank enforces it at the one place every input writes, so it holds whatever pressed the pad.
- **The clock.** The set runs on the master deck's song, compiled on its beat grid so it lands on its bars. With no deck to follow (a live band), it runs on the tapped tempo: `perform_tap` is a control too, so a MIDI pad can tap it.

A pattern on a track reaches Perform by copying it. The two are never linked: graphs are cheap to make, with an agent or by hand, so a copy you can change freely is worth more than a shared original.

Masters disappear as a concept:
- **Group master:** a Perform pattern on `front_wash`, in multiply, whose brightness is wired to a fader node.
- **Blackout:** the same on `all`.
- **Colour override:** a Color pattern in replace, at the top.

That replaces `ManualLayerState`, `MidiAction::SetIntensity` and the post-mix dimmer pass with things users already know.

### 5. The surface

The Perform screen is the decks (or the tapped tempo), then the venue's layout (`perform_layouts`): pages of a grid, like a console's executor window. A cell covers whole grid cells, anywhere on the page, and holds one of:
- a **pattern**, which the cell is the home of: its pad (when its graph has one), its level, and a fader or button for each other fader and button node of its graph;
- a **fader** of its own (`perform::cell(id)`), which moves every one of its targets together: patterns' levels, fader nodes of patterns' graphs, or the venue's own controls. One knob on three chases' speeds;
- the **master**, or **tap**.

Nothing on the surface is tied to a controller's shape. A controller reaches a cell through a binding to its control: **Map** mode, click the control, touch the hardware. A binding's mode says what the hardware sends, never how the control plays: a **fader** sends its position; a **momentary** pad sends a press and a release, each a press of the control's button, which plays by its own mode; a **toggle** key latches by itself, so each message is its new state and the button is gated to it, with its own envelope. A venue with no layout gets a first page: a cell per pattern, then tap and the master.

The Perform tab has a chat of its own (`perform_set` threads, apart from the venue's). Its agent reads and changes the set through `luma.perform`: patterns from presets or built graphs into cells, the layout, bindings and a learn, and what plays now. Every verb is the same change the tab's own commands make (`services::perform_set`), with the same checks and the same recompile, and the tab reads the set again when a turn changed it.

## What it takes in the engine

- **A control frame.** Today the only runtime argument is beats; changing a value means recompiling the scene. Instead, `Control` compiles to a slot index, and `evaluate_batch(beats, &ControlFrame)` reads slots. Values change at 0 cost, and seek-safety holds, because a frame stays a pure function of its inputs plus the control snapshot.
- **Tempo integration moves to the host.** Clock periods are pre-integrated over the pattern's span, so a live rate change would need a recompile. The host should own beat integration (`beats = ∫ tempo × rate`), for scores and patterns launched in Perform alike, and the graph stays pure in beats.
- **One input hub.** It replaces the two near-identical MIDI managers (controller and mixer), which duplicate port, learn and reconnect code. Mixer faders become ordinary inputs bound to controls. That is also where deck volume should come from: today's mixer fader state is computed and never used.

## Phases

1. Control node and control frame, on-screen faders, then the input hub with MIDI and OSC learn. Delete the manual layer and its MIDI actions.
2. Launching patterns in Perform, the live beat clock, pad modes. Done: the Perform set, the layout, tap tempo, Map.
3. Feedback (LEDs, motor faders), modifiers, device templates.
4. Later: live audio input, MIDI 2.0 property exchange.

## Other things worth redesigning

1. **Audio features are offline only.** They are analysed per track and normalised over the whole track. For unscripted tracks, a live audio band-energy input is the "autoloop" fallback SoundSwitch users expect.
2. **The docs have drifted.** The www architecture docs describe forms and say a pattern never holds its own graph, which is the opposite of the code. The node reference names a generator that doesn't exist.
3. **Dead code:**
   - The kernel's `Binding::Input` is unused: patterns copy rather than share, so nothing exposes inputs.
   - The `clips.graph` and `inputs_json` columns are unused.
   - The controller writes dark frames to output directly, bypassing the render loop.
