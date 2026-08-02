import { describe, expect, it } from "vitest";
import { LatestRequestGate } from "./latest-request-gate";

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}

describe("LatestRequestGate", () => {
	it("rejects a delayed initial load after an authoritative restore", async () => {
		const gate = new LatestRequestGate();
		const response = deferred<string>();
		const ticket = gate.issue();
		let graph = "loading";
		const load = response.promise.then((value) => {
			if (gate.owns(ticket)) graph = value;
		});

		gate.supersede();
		graph = "restored";
		response.resolve("stale initial load");
		await load;

		expect(graph).toBe("restored");
	});

	it("rejects a delayed manual-save response after an authoritative restore", async () => {
		const gate = new LatestRequestGate();
		const response = deferred<string>();
		const ticket = gate.issue();
		let savedRevision = "before save";
		const save = response.promise.then((revision) => {
			if (gate.owns(ticket)) savedRevision = revision;
		});

		gate.supersede();
		savedRevision = "restored revision";
		response.resolve("stale save revision");
		await save;

		expect(savedRevision).toBe("restored revision");
	});
});
