import { resolve } from "node:path";
import { afterEach, expect, test } from "vitest";

import { type Harness, startHarness } from "./shim";

const FAKE = resolve(import.meta.dirname, "testdata/fake-harness.ts");

let harness: Harness | null = null;
afterEach(async () => {
	await harness?.close();
	harness = null;
	delete process.env.FAKE_HARNESS_POISON;
});

/**
 * Concurrent `invoke`s share one stdin pipe. Without a serialized writer their
 * chunks splice into each other, the harness reads a line built out of two
 * requests, and the caller whose request was eaten waits forever.
 */
test("concurrent invokes each get their own answer", async () => {
	harness = await startHarness({ binary: FAKE, verbose: false });
	const calls = Array.from({ length: 400 }, (_, i) =>
		harness?.invoke<{ cmd: string; args: { n: number } }>(`cmd_${i}`, { n: i }),
	);
	const answers = await Promise.all(calls);
	for (const [i, answer] of answers.entries()) {
		expect(answer).toEqual({ cmd: `cmd_${i}`, args: { n: i } });
	}
});

/**
 * A null id names no request, so it cannot be delivered to one caller. Failing
 * everyone in flight is loud; the old silence was a hang.
 */
test("an unattributable error frame fails every in-flight call", async () => {
	process.env.FAKE_HARNESS_POISON = "poison";
	harness = await startHarness({ binary: FAKE, verbose: false });

	const slow = harness.invoke("slow", {});
	const poisoned = harness.invoke("poison", {});

	await expect(slow).rejects.toThrow(/request pipe is corrupt/);
	await expect(poisoned).rejects.toThrow(/request pipe is corrupt/);
});
