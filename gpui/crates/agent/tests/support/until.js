// `until(what, pred, { timeoutMs })`: poll until a snapshot satisfies `pred`,
// or fail saying what it waited for, how long, and what it last saw.
//
// Polling, not sleeping. The loads behind these screens run on a runtime gpui
// cannot see, so "how many frames until it has loaded" is a guess a busy
// machine falsifies.
//
// Bounded by time, not by a count of polls: a count is a different time on
// every machine, and a failing wait should say so in seconds, not after a
// thousand frames. Each poll after the first is `snapshot({ waitMs })`, which
// draws only when the app did something and returns as soon as it does.
//
// Assigned onto the global rather than declared: the interpreter keeps one
// context per session, so a `const` would be a redeclaration the second time a
// test pastes this in.
globalThis.until = (what, pred, options) => {
	const timeoutMs = options?.timeoutMs ?? 5000;
	const started = Date.now();
	let last = app.snapshot();
	for (;;) {
		if (pred(last)) return last;
		const left = timeoutMs - (Date.now() - started);
		if (left <= 0) break;
		last = app.snapshot({ waitMs: Math.min(50, left) });
	}
	const labels = last.nodes.map((n) => `${n.role}:${n.label}`);
	const shown = labels.slice(0, 40).join(", ");
	throw new Error(
		`never saw ${what} in ${Date.now() - started} ms; last frame (${labels.length} nodes): ` +
			shown + (labels.length > 40 ? ", …" : ""),
	);
};
