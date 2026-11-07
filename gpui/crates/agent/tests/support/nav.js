// `nav.*`: the suite's one description of how to reach a view.
//
// Every test that drives Luma used to spell its own walk, and the shell's
// moves are why that mattered: tests say where they want to be, and this file
// changes when the way there does.
//
// The shell's vocabulary, as the walks see it:
// - the venue picker overlay auto-opens while no venue is open, and its cards
//   are the venues; the title bar's venue menu switches venue after that;
// - the window is a venue in one of three modes — Rig (the venue page), Arrange
//   (the open score) and Live (the Perform set) — switched by 1/2/3 or the
//   title bar's segmented control;
// - Arrange with no score open is the song list page, in Finder's column
//   view: places (All songs, then folders), the songs in the selected place,
//   and the selected song's scores; choosing a score opens it and closes the
//   page;
// - a double-click on a clip opens its graph where the timeline was;
// - `luma::DismissOverlay` is what Escape means.
//
// Requires `until` (until.js). `luma-test` loads both into every test; a
// cargo test splices `support::NAV` or `support::script(...)`, which carry
// both.
//
// Assigned onto the global for the same reason `until` is: one interpreter
// context per session, so a `const` would be a redeclaration on the second
// paste.
// Readers the suites share, globals for the same reason `nav` is one.
//
// The label of every text, or every chip, in snapshot `s`.
globalThis.texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
globalThis.chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
// Whether a chat turn is in flight: its status line says so. Reads the global
// `texts`, so a test's own `texts` cannot change what it means.
globalThis.busy = (s) =>
	globalThis.texts(s).some((label) => label === "Working" || label === "Sending");
// The chat composer's field.
globalThis.composer = (s) => s.find({ role: "input", label: "Do anything…" });
// The `role` node labelled `label`, once it is on screen.
globalThis.node = (role, label) =>
	until(label, (s) => s.find({ role, label })).find({ role, label });
// The same node in the current frame, or undefined: for asserting it is gone.
globalThis.peek = (role, label) => app.snapshot().find({ role, label });

globalThis.nav = {
	// Wait for `label` to appear as `role`, then click it. Every step below is
	// this: the loads behind these views run on a runtime gpui cannot see, so
	// clicking a node found in a snapshot taken before the load landed is the
	// suite's one recurring flake.
	step(what, role, label, options) {
		const shot = until(what, (s) => {
			const node = s.find({ role, label });
			return node !== undefined
				&& node.enabled !== false
				&& node.bounds.width > 0
				&& node.bounds.height > 0;
		});
		app.click(shot.find({ role, label }), options);
	},

	// The venue picker's card for `name`. The picker is up whenever no venue
	// is open — the app opens there — so this is the first line of almost
	// every walk. Opening the venue closes the picker.
	//
	// Closing is not instant: a dismissed dialog stays mounted, and occluding,
	// while its out-animation plays. Waiting for it to actually go is part of
	// "go to this venue" — otherwise the next click lands on a dying dialog's
	// scrim instead of the shell behind it.
	venue(name) {
		nav.step(`the venue ${name}`, "card", name, { restale: "match" });
		until("the venue picker to finish leaving", (s) =>
			s.find({ role: "card", label: "Venue dialog" }) === undefined ? s : undefined);
	},

	// Put a mode on screen ("Rig", "Arrange" or "Live") by its action, and
	// wait for its body.
	mode(name) {
		app.action(`luma::Show${name}`);
		until(`the ${name} mode`, (s) => nav.shownMode(s) === name);
		app.frames(2);
	},

	// Which mode `snapshot` shows, read off the body a person would see: the
	// venue page for Rig, the Perform header's layout button for Live, the
	// song list or a score for Arrange.
	shownMode(snapshot) {
		if (snapshot.find({ role: "button", label: "Edit layout" })
			|| snapshot.find({ role: "button", label: "Done" })?.bounds.y < 120) return "Live";
		if (snapshot.find({ role: "card", label: "Song list" })
			|| snapshot.findAll({ role: "text" }).some((n) => n.label.startsWith("Score #"))) return "Arrange";
		if (snapshot.findAll({ role: "card" }).some((n) => n.label.endsWith(" Venue"))) return "Rig";
		return undefined;
	},

	// The song list page, over Arrange. Up by itself while no score is open;
	// otherwise the title bar's song title opens it.
	songs() {
		const shot = app.snapshot();
		if (shot.find({ role: "card", label: "Song list" }) === undefined) {
			app.action("luma::ToggleSongList");
		}
		until("the song list", (s) =>
			s.find({ role: "card", label: "Song list" }) !== undefined
				&& s.find({ role: "input", label: "Search tracks" }) !== undefined);
	},

	// One song's scores, in the song list's scores column: the song selected
	// in the songs column. What is in the column is `nav.inScores`.
	scores(track) {
		nav.songs();
		if (app.snapshot().find({ role: "card", label: `Scores of ${track}` }) === undefined) {
			nav.step(`the track ${track}`, "row", track);
		}
		until("the scores", (s) => nav.inScores(s, track, "button", "New score")[0] !== undefined);
	},

	// The `role` nodes (labelled `label`, when given) inside `track`'s
	// scores column, top to bottom.
	inScores(snapshot, track, role, label) {
		const card = snapshot.find({ role: "card", label: `Scores of ${track}` })?.bounds;
		if (card === undefined) return [];
		return snapshot.findAll(label === undefined ? { role } : { role, label })
			.filter((n) => n.bounds.y >= card.y - 0.5 && n.bounds.y + n.bounds.height <= card.y + card.height + 0.5)
			.sort((a, b) => a.bounds.y - b.bounds.y);
	},

	// `track`'s score rows, top to bottom.
	scoreRows(snapshot, track) {
		return nav.inScores(snapshot, track, "row").filter((n) => n.label.startsWith("#"));
	},

	// A song's timeline: its scores, then the newest one (the listing leads
	// with it) or a new one when it has none. Waits for the timeline and for
	// the song list to have closed over it.
	// A score of `name` already open stays: there is nothing to walk to.
	track(name) {
		const already = app.snapshot();
		if (already.find({ role: "card", label: "Song list" }) === undefined
			&& nav.openScore(already, name) !== undefined) {
			until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }));
			return;
		}
		nav.scores(name);
		const level = app.snapshot();
		app.click(nav.scoreRows(level, name)[0] ?? nav.inScores(level, name, "button", "New score")[0]);
		until("the timeline", (s) =>
			s.findAll({ role: "text" }).find((n) => n.label.startsWith("Score #")) !== undefined
				&& s.find({ role: "card", label: "Song list" }) === undefined);
		app.frames(2);
	},

	// Leave the open score of `track` and open it again, so what comes back
	// is what was stored. Arrange holds one score, so opening another one —
	// a sibling, or a new one when there is none — drops the open body.
	reopen(track) {
		const title = (s) => nav.openScore(s, track);
		const open = title(app.snapshot());
		if (open === undefined) throw new Error(`no score of ${track} is open`);
		const ordinal = open.slice(`${track} · `.length);
		const row = (s, mine) => nav.scoreRows(s, track).find((n) => n.label.startsWith(`${ordinal} `) === mine);
		nav.scores(track);
		const level = app.snapshot();
		app.click(row(level, false) ?? nav.inScores(level, track, "button", "New score")[0]);
		until("another score", (s) => title(s) !== undefined && title(s) !== open
			&& s.find({ role: "card", label: "Song list" }) === undefined);
		nav.scores(track);
		app.click(row(until(`the score ${ordinal}`, (s) => row(s, true) ? s : undefined), true));
		until(`the score ${ordinal} again`, (s) => title(s) === open
			&& s.find({ role: "card", label: "Song list" }) === undefined
			&& s.find({ role: "card", label: "Waveform" }) !== undefined);
		app.frames(2);
	},

	// The open score of `track`, as `Aurora · #2`, or undefined when none is.
	// Read off the stage's song / score trigger, or — with the stage put
	// away, which takes the trigger with it — off the timeline's own
	// readouts: its song's name and `Score #2`.
	openScore(snapshot, track) {
		const trigger = snapshot.findAll({ role: "button" }).find((n) => n.label.startsWith(`${track} · #`));
		if (trigger) return trigger.label;
		const score = snapshot.findAll({ role: "text" }).find((n) => n.label.startsWith("Score #"));
		if (score === undefined || snapshot.find({ role: "text", label: track }) === undefined) return undefined;
		return `${track} · ${score.label.slice("Score ".length)}`;
	},

	// Both halves of the commonest walk in the suite.
	trackEditor(venue, track) {
		nav.venue(venue);
		nav.track(track);
	},

	// The settings screen, through the title bar's venue menu. ⌘, is the
	// other door; this walk is the one that proves the door a person can see.
	settings() {
		nav.venueMenu();
		nav.step("the settings row", "row", "Settings…");
	},

	// The title bar's venue menu, open. Its trigger is the title bar's first
	// button, labelled with the venue's name.
	venueMenu() {
		const shot = until("the title bar", (s) => s.find({ role: "card", label: "Title bar" }) ? s : undefined);
		const bar = shot.find({ role: "card", label: "Title bar" }).bounds;
		const trigger = shot.findAll({ role: "button" })
			.filter((n) => n.bounds.width > 0
				&& n.bounds.x >= bar.x && n.bounds.x < bar.x + bar.width
				&& n.bounds.y >= bar.y && n.bounds.y < bar.y + bar.height
				&& !n.label.endsWith(" agent"))
			.sort((a, b) => a.bounds.x - b.bounds.x)[0];
		app.click(trigger);
		until("the venue menu", (s) => s.find({ role: "row", label: "New venue…" }));
	},

	// Rig: the venue page. The one body that names a room without naming a
	// score, which is what a test wants when it needs the stage over an
	// *unlit* rig.
	patch(venue) {
		nav.venue(venue);
		nav.venuePage(venue);
	},

	// Rig, from a window that is already on `venue`.
	venuePage(venue) {
		app.action("luma::ShowRig");
		until("the venue page", (s) =>
			s.find({ role: "card", label: `${venue} Venue` }) !== undefined,
		);
	},

	// The venue's builder, reached the same way. The one body that can write
	// a position, so every placement test starts here.
	stage(venue) {
		nav.patch(venue);
		until("the stage controls", s => s.find({role:"button",label:"Add element"}) !== undefined);
	},

	// Live: the venue's Perform set.
	live() {
		app.action("luma::ShowLive");
		until("the live surface", (s) => s.find({ role: "button", label: "Edit layout" })
			?? s.find({ role: "button", label: "Done" }));
	},

	// Give the editor the whole content column. The stage rides above every
	// mode, so a test about an *editor's* own geometry says this once rather
	// than budgeting around a viewport it does not care about. Idempotent
	// across one session.
	stageOff() {
		if (!globalThis.__stageOff) {
			app.action("luma::ToggleVisualizer");
			globalThis.__stageOff = true;
			app.frames(2);
		}
	},

	// Give the body everything right of the window's edge: the agent panel
	// hidden. Idempotent across one session.
	agentOff() {
		if (app.snapshot().find({ role: "button", label: "Hide agent" })) {
			app.action("luma::ToggleAgent");
			until("the agent panel gone", (s) => s.find({ role: "button", label: "Show agent" }));
			app.frames(2);
		}
	},

	// Switch the window to the venue `name`, through the title bar's venue
	// menu, and wait for its song list or body to arrive.
	switchVenue(name) {
		nav.venueMenu();
		nav.step(`the venue ${name} in the menu`, "row", name);
		until(`the venue ${name}`, (s) => s.find({ role: "button", label: name })?.bounds.y < 44
			&& s.find({ role: "row", label: "New venue…" }) === undefined);
	},

	// Leave `venue` for `elsewhere` and come back, so the venue is opened
	// again from the library. A test that needs this seeds the second venue
	// in its fixture SQL.
	leaveVenue(venue, elsewhere = "Spare Venue") {
		nav.switchVenue(elsewhere);
		nav.switchVenue(venue);
	},

	// The clip card labelled `clip` on the timeline, double-clicked the way a
	// person opens it: its graph where the timeline was.
	clipGraph(clip) {
		nav.step(`the clip ${clip}`, "card", clip, { count: 2 });
		nav.onGraphPage();
	},

	// The selected clip's graph, where the timeline was: what a double-click
	// on the clip opens, for a test whose selection is already made.
	graphPage() {
		app.action("luma::OpenClipGraph");
		nav.onGraphPage();
	},

	// Wait for the graph page to be up and drawn.
	onGraphPage() {
		until("the graph page", (s) =>
			s.find({ role: "button", label: "Back to timeline" })
				&& s.find({ role: "card", label: "Pattern graph" }));
		app.frames(4);
	},

	// Pan the clip graph canvas until what `find(snapshot)` returns is in
	// view, under the canvas toolbar's band; `vertical: false` asks only
	// for its width. A node out of view reads as an empty box on the edge it
	// is past, so each step heads toward it.
	inGraph(find, what, { vertical = true } = {}) {
		for (let i = 0; i < 24; i++) {
			const snap = app.snapshot();
			const c = snap.find({ role: "card", label: "Graph canvas" }).bounds;
			const target = find(snap);
			if (!target) throw new Error(`no ${what} in the graph`);
			const b = target.bounds;
			const inX = b.width > 0 && b.x >= c.x + 4 && b.x + b.width <= c.x + c.width - 4;
			const inY = !vertical
				|| (b.height > 0 && b.y >= c.y + 44 && b.y + b.height <= c.y + c.height - 4);
			if (inX && inY) return target;
			const clamp = (v, r) => Math.max(-r, Math.min(r, v));
			const dx = inX ? 0 : clamp(c.x + c.width / 2 - (b.x + b.width / 2), c.width / 3);
			const dy = inY ? 0 : clamp(c.y + c.height / 2 - (b.y + b.height / 2), c.height / 3);
			app.scroll({ x: c.x + c.width / 2, y: c.y + c.height - 8 }, { dx, dy, steps: 2 });
			app.frames(1);
		}
		throw new Error(`could not bring ${what} into view`);
	},

	// The `role` node labelled `label` on the graph card `card`, in view.
	// Cards can be taller than the canvas, so the card is brought in by its
	// width and the node by its box.
	inCard(card, role, label) {
		until(`the ${card} card`, (s) => s.find({ role: "card", label: card }));
		const on = (s) => {
			const k = s.find({ role: "card", label: card })?.bounds;
			return k && s.findAll({ role, label })
				.find((n) => n.bounds.x >= k.x - 0.5 && n.bounds.x + n.bounds.width <= k.x + k.width + 0.5);
		};
		nav.inGraph((s) => s.find({ role: "card", label: card }), card, { vertical: false });
		until(`${label} on ${card}`, (s) => on(s));
		return nav.inGraph(on, `${label} on ${card}`);
	},

	// Dismiss the overlay that is up — what Escape means, minus the keyboard
	// (a focused text field keeps Escape for itself; the action does not care
	// where focus is).
	dismiss() {
		app.action("luma::DismissOverlay");
		app.frames(4);
	},
};
