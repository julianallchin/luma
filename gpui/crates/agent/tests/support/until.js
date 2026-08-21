// `until(what, pred)`: settle and draw until a snapshot satisfies `pred`, or
// fail saying what it last saw.
//
// Polling, not sleeping. The loads behind these screens run on a runtime gpui
// cannot see, so "how many frames until it has loaded" is a guess a busy
// machine falsifies. One file, included by every fixture that drives a screen,
// because a second copy would be a second answer to how long is long enough.
//
// Assigned onto the global rather than declared: the interpreter keeps one
// context per session, so a `const` would be a redeclaration the second time a
// test pastes this in.
globalThis.until = (what, pred) => {
	let last = null;
	for (let i = 0; i < 300; i++) {
		last = app.snapshot();
		if (pred(last)) return last;
		app.frames(1, { waitMs: 10 });
	}
	throw new Error(
		`never saw ${what}: ` +
			JSON.stringify(last.nodes.map((n) => `${n.role}:${n.label}`)),
	);
};
