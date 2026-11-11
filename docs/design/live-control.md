# Live control

Status: built: controls and the `control` node, controllers, their bindings and lights (`backend/src/inputs/`), Live mode and its Perform set (`backend/src/perform/`, `gpui/crates/app/src/perform*`). Written as the proposal; where the code differs, the code is right. Covers busking in Live: faders, pads, MIDI, OSC and any future sensor.

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
- **A level is built in; the pad is in the graph.** Every pattern has its own level (`perform::level(id)`, 1 until moved). Its pad is the button node its output's `alpha` depends on (`ClipGraph::pad`; the first by id if several), directly or through a clock of its presses, so pressing the pad fades the graph's own alpha, and the set scales that by its level × `perform_master`. A pattern whose pad is all the way off is skipped, unless the pad only strikes a clock of presses (`ClipGraph::pad_gates` is false): its hits play out after the pad is let go; a pattern with no pad plays at its level whenever the set is open. A new pattern gets a toggle pad wired into its alpha that starts on the next beat (`ClipGraph::with_pad`). Every input reaches the pad and the level the same way: an on-screen pad, a MIDI key, an OSC button. The set compiles once; a pad press recompiles only its pattern, and only when it changes where the effect plays.
- **Faders and buttons in a graph belong to their pattern.** A graph's `control` node (a *fader* to the person playing it) and `button` node each give 0–1, the same at every head, and a curve maps that to whatever input they drive. In a Perform pattern each is a value of that pattern, keyed by node id (`perform::node(id, node)`), so two patterns can both have a `speed` fader and renaming a node keeps its bindings. The pattern's cell shows one fader or button per node (`ClipGraph::live_inputs`). In a score the same nodes read the venue's control of their `name`, so a score can read `drop` and the DJ can push it; a button there is a plain gate, 1 while the control is on.
- **Live colors.** A `control` node of kind `color` is a live color: linear Rec. 2020 rgb, wired straight into a color input, with no curve or gradient between. A graph reads it through the same `ControlSource` as a fader (`color()` beside `value()`), so the engine's kernels read a color as three numbers among the live inputs'. Like a button, the bank keeps only its last fade (from, to, when it began and how long, in beats or seconds), so its value is a pure function of time. One nothing has set is white.
- **Buttons have envelopes.** A button node, the pad among them, plays by its settings, read as a `luma_patterns::press::Press`. Its mode says what a press does: a **toggle** turns it on until the next press, a **flash** only while held. Its envelope (attack, decay, sustain, release, in beats or seconds, shaped linear, smooth, punchy or snap) is what its value follows through the press: a soft wash is a slow attack and release at sustain 1. Envelopes are for levels and holds; a hit is a clock of presses. Beats are counted at the tempo the set plays at: the followed deck's, else the tapped one. The bank keeps only when each button went on and off; its value is a pure function of those and now.
- **Hits are a clock of presses.** `time(on=pad, duration=1)` is a clock whose k-th event is born at the button's k-th turn-on, on the beat its start lands it on, and lives `duration` beats, so hits pressed fast overlap, each its own event: a curve over it is each hit's fade, and a `shuffle` or `random` on it draws anew for each press. With no duration an event lives until the next press. It is dark before the first press, and in a score nobody presses. The bank counts each button's turn-ons and keeps the beats of the last 16 (`Bank::presses`); a graph reads them on the frame path, as it reads a fader, so a press never recompiles. A fixed step whose clock counts presses plays live rather than baked.
- **Presses land on the music.** A button's `start` says when a press takes effect, turning on and turning off alike (a toggle's off, a flash's release): `now`, or on the set's next `beat` or `bar`. The bank stores that beat's time as the gate's, still to come, so the value stays a pure function of time and nothing is polled; a pad shows it waits, and so does the agent's `live()`. The opposite change before then calls it off: a toggle pressed twice before its beat does nothing. A flash let go before its beat stays on for one, so a tap lands. In a one-at-a-time row a waiting pad takes from the others when it goes on, not when pressed, and calls off any still waiting. Saved graphs with no `start` play at once; a new pad starts on the beat.
- **Where, then what.** Perform plays on two axes: where (venue groups) and the stack (patterns over each other by z and blend). Every control has one of five jobs: an effect (a pattern's pad), a group, a value (one of a list), a fader, or a utility (Select, Clear, tap, the master). The rule: choose where, then do something; nothing chosen means everything.
- **Groups.** A group cell holds a venue group (`all` among them). Tapped, it is chosen, and stays so: the chosen groups are live, in the bank, not in the layout. Held while another control is pressed, that one press acts on the held group alone and the choice stays as it was; the bank tells a tap from a hold by whether another control was pressed while it was down, so one pad does both. Several chosen or held act on all of them.
- **One pad, one effect.** A pattern with a pad is an effect: on or off, and where it plays, a set of groups (none is its own selection). Its pad tapped with nothing chosen turns it on or off. Tapped with groups chosen or held: off, it comes on there; on, if it already plays on all of them it leaves them (and goes off when none is left, fading out where it was), else it adds them. A flash plays where the groups are while held. When where an effect plays changes, the engine compiles that pattern again on the union of its groups, off the frame path: the frame sees the bank's count of changes (`VenueControls::changed`) move and asks the set's builder, which compiles it and swaps the set in. Until then the old layer plays on only if it already plays, so an effect never lights where it was not asked to.
- **One at a time.** A page's `one_at_a_time` rows hold effects that take turns where they play: an effect that comes on at some groups takes those groups from the row's others, each going off when it is left nowhere; an effect on its own selection (nothing chosen) takes the whole row. The others fade out by their own release. The control bank enforces it at the one place every input writes, so it holds whatever pressed the pad.
- **The base color.** Under every effect lies the base color: a color per light, set by a color list with no targets, on the lights of the groups chosen or held, or every light, fading by the list's fade. It lights nothing: the engine writes its color into each light with no dimmer (`VenueControls::color_base`), and where a layer leaves a light dark the color under it stays. An effect whose blend is `below` takes the color already under it at each light (the base color, or any lit layer under it) at its own brightness. An effect with its own color uses its live color and a normal blend. Wheel fixtures map the composited color to their nearest slot as before.
- **Select and Clear.** Holding Select (or tapping it, which latches it) while effects are pressed chooses them instead of turning them on or off. The next color tapped on a list with no targets is their own color (their first live color, and they play Own: `replace` for one stored as `below`); the list's "below" puts them back on the color below. Then nothing is selected. That choice is the pattern's stored blend, its one home: the bank hands it to the set's write path (`PerformSet::change`, as every change of the set is made) and plays it once the set is installed again; the screen reads the set again whenever one is installed. The own color itself is the live value of its color control, in the bank. Clear tapped chooses nothing, no group and no effect; held for half a second or more, it lets every effect go.
- **The clock.** The set runs on the master deck's song, compiled on its beat grid so it lands on its bars. With no deck to follow (a live band), it runs on the tapped tempo: `perform_tap` is a control too, so a MIDI pad can tap it, and a run's first tap starts a bar. The bank knows the set's beat and bar in one place (`Bank::beat`): the tapped tempo, or the followed deck's beat as the engine reads it from the grid at each frame (`VenueControls::tick`), run on at the deck's tempo between frames. Until the engine has read the deck it follows, the clock is the tapped tempo, its beat and its tempo both, never the one with the other. Lists step on it and quantised presses land on it.
- **Speed is a fader into a clock.** There is no speed control of its own: a chase's speed is a fader wired into its time's `every` (or `duration`, or a random's `every`) through a curve, stepped (`Steps 2` from 1 to 2 beats) or smooth (`Ramp up`), and it may be times a curve over heads. A global speed is a fader cell whose targets are several patterns' speed faders. Such a clock is a line kept between frames by the set's prepared graph (`PreparedGraph::played_live`, per clock and head), so moving the fader never jumps its turns: a speed that fits the bar takes over at the line's next event on that speed's grid, so beats and bars stay put; a speed that never lines up takes over at the next event and leaves the grid, and the next whole step bridges one event back to it. A seek or a loop keeps the speed and lines up with the bar again. The bars are the bank's (the set is compiled with its beats per bar and starts on a bar). A score reads the fader now, with no line kept (`docs/specs/clip-graphs.md`, 5.5).

A pattern on a track reaches Perform by copying it. The two are never linked: graphs are cheap to make, with an agent or by hand, so a copy you can change freely is worth more than a shared original.

Masters disappear as a concept:
- **Group master:** a Perform pattern on `front_wash`, in multiply, whose brightness is wired to a fader node.
- **Blackout:** the same on `all`.
- **Colour override:** a Color pattern in replace, at the top.

That replaces `ManualLayerState`, `MidiAction::SetIntensity` and the post-mix dimmer pass with things users already know.

### 5. The surface

In Live the deck panel beside the stage shows the decks followed (or the tapped tempo), and the body is the venue's layout (`perform_layouts`): pages of a grid, like a console's executor window. A cell covers whole grid cells, anywhere on the page, and holds one of:
- a **pattern**, which the cell is the home of: its pad (when its graph has one), its level, and a fader or button for each other fader and button node of its graph;
- a **fader** of its own (`perform::cell(id)`), which moves every one of its targets together: patterns' levels, fader nodes of patterns' graphs, or the venue's own controls. One knob on three chases' speeds;
- a **list** of values of one kind, colors or numbers 0–1, each a control of its own (`perform::value(id, index)`) that a pad or a MIDI key presses, and the list's own control (`perform::cell(id)`), which taps its next value; a list one place big shows as a single Next button. A value tapped sets every target: a color fades each live color of a pattern's graph it targets from the color it has now over the list's fade; a number sets each level, fader node or venue control. A color list with no targets sets the base color, or after Select the chosen effects' own colors; it may hold "below", the color below, and only it may. A number list with no targets sets nothing. Which value is on is live, in the bank. A list that steps moves on to the next value every so many bars of the set's clock (the deck's beat grid, or 4-beat bars of the tapped tempo), after the last back to the first when it loops; the engine steps it as it renders. A fader cell and a number list move only numbers and a color list only live colors: storing a layout refuses anything else, and a target whose node goes or changes kind drops out of its cell;
- a **group**: a venue group (`group`, its id, or `all`), labelled by the group unless it has a label of its own, lit while chosen;
- **Select** and **Clear** (`perform_select`, `perform_clear`), the **master**, or **tap**.

A stored layout read by a build that does not know one of its cells' kinds (such as a target or color cell of an earlier build) drops that cell as it reads the page.

Nothing on the surface is tied to a controller's shape. A controller reaches a cell through a binding to its control: **Learn** on a cell's control, then touch the hardware, or **Map this page**, which puts the page's cells on the controller's grid by its profile. A binding's mode says what the hardware sends, never how the control plays: a **fader** sends its position; a **momentary** pad sends a press and a release, each a press of the control's button, which plays by its own mode; a **toggle** key latches by itself, so each message is its new state and the button is gated to it, with its own envelope. A venue with no layout gets a first page: a cell per pattern, then tap and the master.

A **busk page** is made from the rig in one step (`services::busk_page`, "Busk page" under Edit layout, or the agent's `busk()`), as a new page of new patterns in one change of the set: it never names a fixture, so a small rig and a large one get the same page and only the groups row grows. Top to bottom: groups on `all`, the role groups and the groups named for a place (eight at most: roles first, then the biggest places); a base color list with no targets (it colors the groups chosen, and ends with "below"); an accent color list on the accent effects' live colors; base and accent effects from presets (Wash, Pulse, Breathe, on the color below; Chase, Sparkle, Ripple, with their own color) on the color fixtures, each with a pad, a live color and a speed fader into its clock (its period at rest, twice as fast pushed up); movement presets one at a time, when something aims; and hits from the press: strobe and black on top while held, and blinder (when there is a blinder group) and white struck by each press, each fading over a beat on its own. Select, Clear, tap, the master and fader cells on the effects' levels and speeds stand at the right. A row the rig cannot use is left out.

### 6. One look, drawn by the screen and the controllers

Luma decides what every control shows; the screen and each controller draw that as well as they can. One function (`controls::surface`, `VenueControls::surface`) gives, for every control on every page, its job (effect, group, value, fader, utility) and its look: on, waiting for its beat, held, selected by Select, the color it shows, a fader's position and tap's beat. It goes to the screen with the polled state (`Playing::surface`); a cell's face only draws it, and plays a pad's envelope on between polls. A controller shows a look as a light (`Look::light`): an effect dim while off, bright while on, blinking while it waits for its beat and pulsing while Select has it, in the color it plays (its own, or the base color where it plays when it takes the color below; a hit in amber); a group lit while chosen or held; a value in its color, bright while on; a utility lit while on, tap on each beat.

A venue has any number of **controllers** (`perform_controllers`, synced): each is the MIDI port it matches, by name, and a **profile**, plain JSON an agent can write (`backend/src/inputs/profile.rs` documents it): where each pad and fader sits on the device's grid, how to send a color (a palette, sent as the nearest entry, or a SysEx template for full rgb), how to show steady, pulse and blink (a channel per mode, a velocity offset, or a fixed velocity for a one-color LED), and how bright a dim light is. Controls that light another way than the rest, such as single-LED buttons beside rgb pads, name a rule of the profile's `lights`, each its own whole color, modes and dim; the others light by the device's. A binding belongs to a controller and hears its address on that controller's port; a controller not yet heard has no port and hears everything, and the first learn on it gives it the port heard. Bindings sync with the venue (`control_bindings`); those a machine kept before move into one controller of their own as its venue's inputs open. When sync downloads controllers or bindings, the hub reads its venue again (`inputs::synced`), whether or not Live is open. The screen polls each controller's name, port and how many controls its profile places, not the profile.

The input hub opens the output of each MIDI input port that has one, and a task of its own draws the open venue's looks on every connected controller every 40 ms (`inputs::lights`), sending only the controls whose look changed, dark for a control that left the surface. With no profile, or none that says how to send a color, a light is echoed: the control's own note or CC at 127 lit and 0 dark. A blink the device cannot show is drawn by turning it on and off. **Map this page** binds each cell's controls to the controls at its places, a pad for a pad and a fader for a fader, in place of what the controller had bound there; a list one place big is its Next.

Luma ships no controller profiles. The agent writes one with the person: it reads the controller and its port, writes the profile from the device's programmer reference on the web and what it knows of it, lights one control at a time (`light`, by a place on the grid) and asks what they see, corrects the profile, then maps the page. **Set up with the agent**, in Edit layout's Controller section, opens Live's chat with that request written in. Light colors go out as the screen shows them: linear Rec. 2020 to display sRGB (`color_space::to_display_srgb`), then to the device's range.

Live has a chat of its own (`perform_set` threads, apart from the venue's). Its agent reads and changes the set through `luma.perform`: patterns from presets or built graphs into cells, the layout, bindings and a learn, and what plays now. Every verb is the same change Live's own commands make (`services::perform_set`), with the same checks and the same recompile, and Live reads the set again when a turn changed it.

## What it takes in the engine

- **A control frame.** Today the only runtime argument is beats; changing a value means recompiling the scene. Instead, `Control` compiles to a slot index, and `evaluate_batch(beats, &ControlFrame)` reads slots. Values change at 0 cost, and seek-safety holds, because a frame stays a pure function of its inputs plus the control snapshot.
- **Live clocks.** A clock period a live control moves cannot be integrated over the clip ahead of play. Instead its clock is a line the live set's prepared graph keeps from frame to frame, the one state in otherwise pure evaluation; every other clock is still integrated once, and the graph stays in beats.
- **One input hub.** It replaces the two near-identical MIDI managers (controller and mixer), which duplicate port, learn and reconnect code. Mixer faders become ordinary inputs bound to controls. That is also where deck volume should come from: today's mixer fader state is computed and never used.

## Phases

1. Control node and control frame, on-screen faders, then the input hub with MIDI and OSC learn. Delete the manual layer and its MIDI actions.
2. Launching patterns in Perform, the live beat clock, pad modes. Done: the Perform set, the layout, tap tempo, Map.
3. Feedback (LEDs, motor faders), modifiers, device templates. Done: the one look, controllers with profiles, their lights, Map this page.
4. Later: live audio input, MIDI 2.0 property exchange.

## Other things worth redesigning

1. **Audio features are offline only.** They are analysed per track and normalised over the whole track. For unscripted tracks, a live audio band-energy input is the "autoloop" fallback SoundSwitch users expect.
2. **The docs have drifted.** The www architecture docs describe forms and say a pattern never holds its own graph, which is the opposite of the code. The node reference names a generator that doesn't exist.
3. **Dead code:** the `clips.graph` and `inputs_json` columns are unused.
