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

Each app holds one bank of controls per venue (`controls::ControlBanks`), shared by the engine, the inputs, the Perform session and the commands, so two apps in one process (the UI tests) never play each other's.

A venue's Perform set (`perform_patterns`) holds ordinary patterns: the same `ClipGraph`, selection, blend mode and seed a score's pattern has, with no place on a timeline. Building the set is building a score live.

- **Each pattern plays on its own.** A colour pattern and a dimmer chase on the same lights are two patterns, both on, each at its own level. Where two write the same attribute of the same light, the higher one in the set wins (`z_index`), with the same blend modes the editor uses.
- **A level is built in; the pad is in the graph.** Every pattern has its own level (`perform::level(id)`, 1 until moved). Its pad is the button node its output's `alpha` depends on (`ClipGraph::pad`; the first by id if several), so pressing the pad fades the graph's own alpha, and the set scales that by its level × `perform_master`. A pattern whose pad is all the way off is skipped; a pattern with no pad plays at its level whenever the set is open. A new pattern gets a toggle pad wired into its alpha that starts on the next beat (`ClipGraph::with_pad`). Every input reaches the pad and the level the same way: an on-screen pad, a MIDI key, an OSC button. The set compiles once; pressing a pad never recompiles.
- **Faders and buttons in a graph belong to their pattern.** A graph's `control` node (a *fader* to the person playing it) and `button` node each give 0–1, the same at every head, and a curve maps that to whatever input they drive. In a Perform pattern each is a value of that pattern, keyed by node id (`perform::node(id, node)`), so two patterns can both have a `speed` fader and renaming a node keeps its bindings. The pattern's cell shows one fader or button per node (`ClipGraph::live_inputs`). In a score the same nodes read the venue's control of their `name`, so a score can read `drop` and the DJ can push it; a button there is a plain gate, 1 while the control is on.
- **Live colors.** A `control` node of kind `color` is a live color: linear Rec. 2020 rgb, wired straight into a color input, with no curve or gradient between. A graph reads it through the same `ControlSource` as a fader (`color()` beside `value()`), so the engine's kernels read a color as three numbers among the live inputs'. Like a button, the bank keeps only its last fade (from, to, when it began and how long, in beats or seconds), so its value is a pure function of time. One nothing has set is white.
- **Buttons have envelopes.** A button node, the pad among them, plays by its settings, read as a `luma_patterns::press::Press`. Its mode says what a press does: a **toggle** turns it on until the next press, a **flash** only while held. Its envelope (attack, decay, sustain, release, in beats or seconds, shaped linear, smooth, punchy or snap) is what its value follows through the press: a soft wash is a slow attack and release at sustain 1, a hit is a punchy decay at sustain 0, which fires once however long it is held. Beats are counted at the tempo the set plays at: the followed deck's, else the tapped one. The bank keeps only when each button went on and off; its value is a pure function of those and now.
- **Presses land on the music.** A button's `start` says when a press takes effect, turning on and turning off alike (a toggle's off, a flash's release): `now`, or on the set's next `beat` or `bar`. The bank stores that beat's time as the gate's, still to come, so the value stays a pure function of time and nothing is polled; a pad shows it waits, and so does the agent's `live()`. The opposite change before then calls it off: a toggle pressed twice before its beat does nothing. A flash let go before its beat stays on for one, so a tap lands. In a one-at-a-time row a waiting pad releases the others when it goes on, not when pressed, and calls off any still waiting. Saved graphs with no `start` play at once; a new pad starts on the beat.
- **Targets.** A target cell holds a venue group. Pressing a pattern's pad while targets are held plays an **instance** of the pattern on the union of their groups instead of its own selection. An instance is the pattern and a set of groups: it has a pad of its own (`perform::instance(pad, groups)`, a button played by the pattern's pad's press), so pressing again with the same targets held toggles it, and its release lets go of the instance its press went to. Its level, faders, buttons and live colors are the pattern's, in every instance. No target held is the pattern on its own selection, as before. A target is held while pressed: an undeclared control (`perform::cell(id)`), so a momentary MIDI pad holds it too. So a busk page of 8 effects and 6 targets is 14 pads, not 48. The engine compiles an instance the first time it plays, off the frame path: the frame sees the bank has made a new one and asks the set's builder, which compiles it, with the heads it aims, and swaps the grown set in; the set plays on meanwhile. A recompile of the set compiles the instances that still sound and forgets the rest. Holding a target while choosing a color swatch does nothing per group yet.
- **One at a time.** A page's `one_at_a_time` rows hold patterns that take turns: turning one on releases the row's others on the same groups, which fade out by their own release. On Front, pressing B releases A on Front only. The control bank enforces it at the one place every input writes, so it holds whatever pressed the pad.
- **The clock.** The set runs on the master deck's song, compiled on its beat grid so it lands on its bars. With no deck to follow (a live band), it runs on the tapped tempo: `perform_tap` is a control too, so a MIDI pad can tap it, and a run's first tap starts a bar. The bank knows the set's beat and bar in one place (`Bank::beat`): the tapped tempo, or the followed deck's beat as the engine reads it from the grid at each frame (`VenueControls::tick`), run on at the deck's tempo between frames. Until the engine has read the deck it follows, the clock is the tapped tempo, its beat and its tempo both, never the one with the other. Color cells step on it and quantised presses land on it.
- **Speed is a fader into a clock.** There is no speed control of its own: a chase's speed is a fader wired into its time's `every` (or `duration`, or a random's `every`) through a curve, stepped (`Steps 2` from 1 to 2 beats) or smooth (`Ramp up`), and it may be times a curve over heads. A global speed is a fader cell whose targets are several patterns' speed faders. Such a clock is a line kept between frames by the set's prepared graph (`PreparedGraph::played_live`, per clock and head), so moving the fader never jumps its turns: a speed that fits the bar takes over at the line's next event on that speed's grid, so beats and bars stay put; a speed that never lines up takes over at the next event and leaves the grid, and the next whole step bridges one event back to it. A seek or a loop keeps the speed and lines up with the bar again. The bars are the bank's (the set is compiled with its beats per bar and starts on a bar). A score reads the fader now, with no line kept (`docs/specs/clip-graphs.md`, 5.5).

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
- a **color** cell: swatches, each a control of its own (`perform::swatch(id, index)`) that a pad or a MIDI key presses. Choosing one fades every target, a live color of a pattern's graph, from the color it has now to the swatch over the cell's fade. Which swatch is on is live, in the bank, not in the layout. A cell that steps moves on to the next swatch every so many bars of the set's clock (the deck's beat grid, or 4-beat bars of the tapped tempo), after the last back to the first when it loops; the engine steps it as it renders. A fader cell moves only numbers and a color cell only live colors: storing a layout refuses anything else, and a target whose node goes or changes kind drops out of its cell;
- a **target**: a venue group (`group`, by name), labelled by the group unless it has a label of its own. While it is held, a pattern's pad plays the pattern there, and the pattern's cell names where it plays. On screen, with one pointer, a press on a target that lets go over a pad presses that pad with the target held;
- the **master**, or **tap**.

Nothing on the surface is tied to a controller's shape. A controller reaches a cell through a binding to its control: **Map** mode, click the control, touch the hardware. A binding's mode says what the hardware sends, never how the control plays: a **fader** sends its position; a **momentary** pad sends a press and a release, each a press of the control's button, which plays by its own mode; a **toggle** key latches by itself, so each message is its new state and the button is gated to it, with its own envelope. A venue with no layout gets a first page: a cell per pattern, then tap and the master.

A **busk page** is made from the rig in one step (`services::busk_page`, Setup's "Busk page" or the agent's `busk()`), as a new page of new patterns in one change of the set: it never names a fixture, so a small rig and a large one get the same page and only the targets row grows. Top to bottom: targets on `all`, the role groups and the groups named for a place (eight at most: roles first, then the biggest places); a base and an accent color cell, setting the effects' live colors; base and accent effects from presets (Wash, Pulse, Breathe; Chase, Sparkle, Ripple) on the color fixtures, each with a pad, a live color and a speed fader into its clock (its period at rest, twice as fast pushed up); movement presets one at a time, when something aims; and flash hits from the press: strobe, blinder (when there is a blinder group), white, and black on top. Tap, the master and fader cells on the effects' levels and speeds stand at the right. A row the rig cannot use is left out.

The Perform tab has a chat of its own (`perform_set` threads, apart from the venue's). Its agent reads and changes the set through `luma.perform`: patterns from presets or built graphs into cells, the layout, bindings and a learn, and what plays now. Every verb is the same change the tab's own commands make (`services::perform_set`), with the same checks and the same recompile, and the tab reads the set again when a turn changed it.

## What it takes in the engine

- **A control frame.** Today the only runtime argument is beats; changing a value means recompiling the scene. Instead, `Control` compiles to a slot index, and `evaluate_batch(beats, &ControlFrame)` reads slots. Values change at 0 cost, and seek-safety holds, because a frame stays a pure function of its inputs plus the control snapshot.
- **Live clocks.** A clock period a live control moves cannot be integrated over the clip ahead of play. Instead its clock is a line the live set's prepared graph keeps from frame to frame, the one state in otherwise pure evaluation; every other clock is still integrated once, and the graph stays in beats.
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
