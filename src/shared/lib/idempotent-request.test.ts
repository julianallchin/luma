import { describe, expect, it } from "vitest";

import {
	IdempotentRequestGate,
	idempotentRequestFor,
} from "./idempotent-request";

describe("idempotentRequestFor", () => {
	it("reuses an operation ID only for an identical retry", () => {
		let next = 0;
		const createId = () => `request-${++next}`;

		const first = idempotentRequestFor(null, "same payload", createId);
		const retry = idempotentRequestFor(first, "same payload", createId);
		const edited = idempotentRequestFor(retry, "edited payload", createId);
		const reopened = idempotentRequestFor(null, "same payload", createId);

		expect(retry.requestId).toBe(first.requestId);
		expect(edited.requestId).not.toBe(first.requestId);
		expect(reopened.requestId).not.toBe(first.requestId);
	});
});

describe("IdempotentRequestGate", () => {
	it("suppresses a duplicate submission while the first attempt is pending", () => {
		const gate = new IdempotentRequestGate(() => "request-1");

		const first = gate.begin("same payload");

		expect(first?.requestId).toBe("request-1");
		expect(gate.begin("same payload")).toBeNull();
		expect(gate.begin("different payload")).toBeNull();
	});

	it("reuses the request ID after failure and consumes it after success", () => {
		let next = 0;
		const gate = new IdempotentRequestGate(() => `request-${++next}`);
		const first = gate.begin("same payload");
		expect(first).not.toBeNull();
		if (!first) return;

		expect(gate.fail(first)).toBe(true);
		const retry = gate.begin("same payload");
		expect(retry).toEqual(first);
		if (!retry) return;

		expect(gate.succeed(retry)).toBe(true);
		expect(gate.begin("same payload")?.requestId).toBe("request-2");
	});

	it("issues a new request for changed input after a failed attempt", () => {
		let next = 0;
		const gate = new IdempotentRequestGate(() => `request-${++next}`);
		const first = gate.begin("first payload");
		expect(first).not.toBeNull();
		if (!first) return;

		gate.fail(first);

		expect(gate.begin("changed payload")?.requestId).toBe("request-2");
	});

	it("invalidates an in-flight attempt when its subject is reset", () => {
		let next = 0;
		const gate = new IdempotentRequestGate(() => `request-${++next}`);
		const stale = gate.begin("old subject");
		expect(stale).not.toBeNull();
		if (!stale) return;

		gate.reset();
		const current = gate.begin("new subject");

		expect(current?.requestId).toBe("request-2");
		expect(gate.succeed(stale)).toBe(false);
		expect(gate.begin("new subject")).toBeNull();
	});

	it("forgets a failed retry identity when its UI scope closes", () => {
		let next = 0;
		const gate = new IdempotentRequestGate(() => `request-${++next}`);
		const failed = gate.begin("dialog scope");
		expect(failed).not.toBeNull();
		if (!failed) return;

		gate.fail(failed);
		gate.reset();

		expect(gate.begin("dialog scope")?.requestId).toBe("request-2");
	});
});
