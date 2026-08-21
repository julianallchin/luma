// `nav.*`: the suite's one description of how to reach a view.
//
// Every test that drives Luma used to spell its own walk — click the venue
// card, click the track row, click Back — and there were six spellings of the
// first step alone. That is fine while the walk never changes and fatal the day
// it does: a shell redesign is then a ten-file edit to the suite, and ten
// chances to leave one test walking a path that no longer exists.
//
// So the walk lives here, once. A test says *where it wants to be*, not which
// pixels get it there, and the day the venue grid becomes an overlay or the
// track list becomes a sidebar this file changes and nothing else does.
//
// Requires `until` (support/until.js) — splice `nav::SCRIPT`, which carries
// both, rather than including this alone.
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
		const shot = until(what, (s) => s.find({ role, label }) !== undefined);
		app.click(shot.find({ role, label }), options);
	},

	// The venue grid's card for `name`. The app opens here, so this is the
	// first line of almost every walk.
	venue(name) {
		nav.step(`the venue ${name}`, "card", name, { restale: "match" });
	},

	// A track, from the venue it belongs to: its row opens the track editor.
	track(name) {
		nav.step(`the track ${name}`, "row", name);
	},

	// Both halves of the commonest walk in the suite. Named so a test that
	// only wants a timeline says so in one line.
	trackEditor(venue, track) {
		nav.venue(venue);
		nav.track(track);
	},

	// The library's pattern list.
	patterns() {
		nav.step("the Patterns button", "button", "Patterns");
	},

	// A pattern's node graph, from the pattern list.
	pattern(name) {
		nav.patterns();
		nav.step(`the pattern ${name}`, "row", name);
	},

	// Leave the current view for the one it was opened from.
	back() {
		nav.step("the Back button", "button", "Back");
	},
};
