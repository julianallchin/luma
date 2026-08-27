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
// - a sidebar row click opens (or reveals — opening is idempotent) that
//   track's editor tab;
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

	// A track, from the sidebar: its row opens (or reveals) the editor tab.
	track(name) {
		nav.step(`the track ${name}`, "row", name);
	},

	// Both halves of the commonest walk in the suite. Named so a test that
	// only wants a timeline says so in one line.
	trackEditor(venue, track) {
		nav.venue(venue);
		nav.track(track);
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
		nav.step("the universe choice", "button", "Universe setup");
		until("the universe tab", (s) =>
			s.find({ role: "card", label: `${venue} Universe setup` }) !== undefined,
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
