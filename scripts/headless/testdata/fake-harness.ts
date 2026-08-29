#!/usr/bin/env bun
/**
 * A stand-in for `agent_harness` that exercises the pipe and nothing else.
 *
 * It holds the harness's half of the framing contract exactly: one JSON
 * request per line in, one JSON response per line out, `id` echoed back, and
 * `{"id": null, ...}` when a line does not parse — which is the failure the
 * shim must surface rather than hang on. Replies are deliberately reordered so
 * a test proves ids are matched rather than positions.
 *
 * `FAKE_HARNESS_POISON=<cmd>` answers that command with an unattributable
 * null-id frame, standing in for a request the real harness could not read.
 */
import { createInterface } from "node:readline";

const poison = process.env.FAKE_HARNESS_POISON ?? null;

let writes: Promise<void> = Promise.resolve();
const reply = (frame: unknown) => {
	const payload = `${JSON.stringify(frame)}\n`;
	writes = writes.then(
		() => new Promise<void>((res) => void process.stdout.write(payload, () => res())),
	);
};

const rl = createInterface({ input: process.stdin });
rl.on("line", (line) => {
	if (!line.trim()) return;
	let request: { id?: unknown; cmd?: unknown; args?: unknown };
	try {
		request = JSON.parse(line);
	} catch (error) {
		reply({ id: null, err: `malformed request JSON: ${String(error)}; line was ${line}` });
		return;
	}
	if (poison !== null && request.cmd === poison) {
		reply({ id: null, err: "poisoned frame" });
		return;
	}
	setTimeout(
		() => reply({ id: request.id, ok: { cmd: request.cmd, args: request.args } }),
		Math.random() * 4,
	);
});
