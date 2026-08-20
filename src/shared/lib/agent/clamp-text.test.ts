import { describe, expect, it } from "vitest";
import { clampForModel } from "./clamp-text";

describe("clampForModel", () => {
	it("returns short text untouched", () => {
		expect(clampForModel("hello", 100)).toBe("hello");
	});

	it("keeps head and tail and names the omission", () => {
		const text = `${"a".repeat(500)}MIDDLE${"z".repeat(500)}`;
		const out = clampForModel(text, 100, { label: "stdout" });
		expect(out.startsWith("a".repeat(60))).toBe(true);
		expect(out.endsWith("z".repeat(40))).toBe(true);
		expect(out).toContain("chars of stdout omitted");
		expect(out).not.toContain("MIDDLE");
	});

	it("reports the exact omitted count", () => {
		const out = clampForModel("x".repeat(1_100), 100);
		expect(out).toContain("[1000 chars of output omitted");
	});

	it("biases toward the tail when asked", () => {
		const text = `${"h".repeat(500)}${"t".repeat(500)}`;
		const out = clampForModel(text, 100, { tailShare: 0.75 });
		expect(out.endsWith("t".repeat(75))).toBe(true);
	});
});
