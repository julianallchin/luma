// `nav.*`: the suite's one description of how to reach a view.
//
// Every test that drives Luma used to spell its own walk, and the shell swap
// is why that mattered: the venue grid became a picker overlay, the track list
// became the sidebar, screens became workspace tabs, and Back stopped
// existing. This file changed once; the tests said where they wanted to be and
// kept saying it.
//
// The shell's vocabulary, as the walks see it:
// - the venue picker overlay auto-opens while no venue is selected, and its
//   cards are the old welcome grid's;
// - a sidebar row click pushes the column to that track's scores; opening a
//   timeline is choosing one of them (`nav.track` walks both halves);
// - the pattern picker is an overlay on `luma::OpenPatterns`, and picking a
//   row opens that pattern's graph tab;
// - `luma::CloseTab` closes the visible tab, dropping its state — the
//   close-then-reopen idiom that used to be "Back then re-enter";
// - `luma::DismissOverlay` is what Escape means.
//
// Requires `until` (support/until.js) — splice `nav::SCRIPT` or
// `support::script(...)`, which carry both, rather than including this alone.
//
// Assigned onto the global for the same reason `until` is: one interpreter
// context per session, so a `const` would be a redeclaration on the second
// paste.
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
	// is selected — the app opens there — so this is the first line of almost
	// every walk. Selecting the venue closes the picker and fills the sidebar.
	//
	// Closing is not instant: a dismissed dialog stays mounted, and occluding,
	// while its out-animation plays. Waiting for it to actually go is part of
	// "go to this venue" — otherwise the next click in the walk lands on a
	// dying dialog's scrim instead of the shell behind it, which is exactly
	// the flake this file exists to remove.
	venue(name) {
		nav.step(`the venue ${name}`, "card", name, { restale: "match" });
		until("the venue picker to finish leaving", (s) =>
			s.find({ role: "card", label: "Venue dialog" }) === undefined ? s : undefined);
	},

	// The sidebar's second level: one track's scores.
	//
	// The row itself — a track has as many scores as there are people who
	// annotated it, so the row is the door to the *list* and nothing in the
	// sidebar guesses which one you meant. Waits for the push to finish,
	// because a level still in flight occludes every gesture aimed at it.
	scores(track) {
		nav.step(`the track ${track}`, "row", track);
		// The level having *arrived* is the track list being gone: while the
		// push plays both levels are mounted, and a row on the one still
		// flying is clipped out of the column — a click on it throws.
		until("the scores level", (s) =>
			s.find({ role: "card", label: "Scores level" }) !== undefined
				&& s.find({ role: "button", label: "New score" }) !== undefined
				&& s.find({ role: "input", label: "Search tracks…" }) === undefined);
	},

	// A track's timeline, which is now two gestures: into the track's scores,
	// then onto one of them. The level leads with the most recently written
	// score, which is the one the row used to open by itself.
	//
	// Pops back to the track list afterwards, so this still means what it
	// meant to every caller: the editor is up and the sidebar is at rest on
	// the list. A test that wants to *stay* on the scores level says
	// `nav.scores`.
	track(name) {
		nav.scores(name);
		const level = app.snapshot();
		const score = level.findAll({ role: "row" }).find((n) => n.label.startsWith("#"));
		// No score in this venue yet — minting one is the same door, and it
		// opens what it mints.
		app.click(score ?? level.find({ role: "button", label: "New score" }));
		// The timeline being up is what "opened the track" means, and waiting
		// for it here rather than in every caller keeps the walk's failure on
		// the gesture that missed instead of on the assertion three lines on.
		until("the timeline", (s) =>
			s.findAll({ role: "text" }).find((n) => n.label.startsWith("SCORE #")) !== undefined);
		nav.step("the way back to the track list", "button", "Back to tracks");
		until("the track list again", (s) =>
			s.find({ role: "card", label: "Scores level" }) === undefined
				&& s.find({ role: "input", label: "Search tracks…" }) !== undefined
				&& s.find({ role: "row", label: name }) !== undefined);
	},

	// Both halves of the commonest walk in the suite. Named so a test that
	// only wants a timeline says so in one line.
	trackEditor(venue, track) {
		nav.venue(venue);
		nav.track(track);
	},

	// The settings screen, through the sidebar's account foot.
	//
	// Two steps rather than one button in a corner: settings is an account
	// gesture now, so the foot opens a menu and "Settings" is a row in it.
	// ⌘, is the other door and does not need the sidebar — this walk is the
	// one that proves the door a person can see.
	settings() {
		nav.step("the account foot", "button", "Account");
		nav.step("the settings row", "row", "Settings");
	},

	// The pattern picker overlay. An action rather than a button: the picker
	// opens from anywhere, and the action is the same door ⌘P is.
	patterns() {
		app.action("luma::OpenPatterns");
		until("the pattern picker", (s) =>
			s.find((n) => n.role === "text" && n.label.endsWith("PATTERNS")) !== undefined,
		);
	},

	// A pattern's graph tab, via the picker.
	//
	// Waits for the picker to be *gone*, not merely for the row to be clicked.
	// An overlay's close is deferred so it can animate out, and until it is
	// reaped its scrim is still a full-window hit target — so a gesture aimed
	// at the shell on the next line (the `+` control, a node on the canvas)
	// lands on the dying dialog instead, silently doing nothing.
	pattern(name) {
		nav.patterns();
		nav.step(`the pattern ${name}`, "row", name);
		until("the dismissed pattern picker", (s) =>
			s.find((n) => n.role === "text" && n.label.endsWith("PATTERNS")) === undefined,
		);
	},

	// The venue's patch tab, via the `+` menu. The one tab that names a room
	// without naming a score, which is what a test wants when it needs the
	// stage pane up over an *unlit* rig — a track editor would composite one.
	universe(venue) {
		nav.venue(venue);
		// The `+` lives only in the workspace panel's band now, so it is not on
		// screen until something is open there. `luma::NewTab` is the path
		// chrome.rs documents as surviving a closed panel.
		app.action("luma::NewTab");
		nav.step("the universe choice", "button", "Patch");
		until("the universe tab", (s) =>
			s.find({ role: "card", label: `${venue} Patch` }) !== undefined,
		);
	},

	// Close the visible tab, dropping its state. What "leave and come back"
	// means now: a reopened tab reloads, which is what the persistence tests
	// lean on.
	closeTab() {
		app.action("luma::CloseTab");
		app.frames(4);
	},

	// Put the workspace into takeover — the tab body gets everything right
	// of the sidebar. Idempotent across one session: the flag survives tab
	// closes, so a test calls this once after its first open and every later
	// tab inherits the room.
	expand() {
		if (!globalThis.__expanded) {
			app.action("luma::ToggleExpand");
			globalThis.__expanded = true;
			app.frames(2);
		}
	},

	// Give the editor the whole workspace column. The stage rides above every
	// tab that names a room, so a test about an *editor's* own geometry says
	// this once rather than budgeting around a viewport it does not care
	// about. Idempotent across one session, like `expand`.
	stageOff() {
		if (!globalThis.__stageOff) {
			app.action("luma::ToggleVisualizer");
			globalThis.__stageOff = true;
			app.frames(2);
		}
	},

	// Dismiss the overlay that is up — what Escape means, minus the keyboard
	// (a focused text field keeps Escape for itself; the action does not care
	// where focus is).
	dismiss() {
		app.action("luma::DismissOverlay");
		app.frames(4);
	},
};
