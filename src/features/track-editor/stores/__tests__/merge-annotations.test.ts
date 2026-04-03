import { describe, expect, it } from "vitest";
import { argsEqual } from "../use-track-editor-store";

describe("argsEqual", () => {
	it("treats identical objects as equal", () => {
		expect(argsEqual({ color: "red" }, { color: "red" })).toBe(true);
	});

	it("treats different key order as equal", () => {
		expect(
			argsEqual(
				{ color: { r: 255, g: 0, b: 0, a: 1 }, subdivision: 2 },
				{ subdivision: 2, color: { a: 1, b: 0, g: 0, r: 255 } },
			),
		).toBe(true);
	});

	it("detects actual value differences", () => {
		expect(argsEqual({ color: "red" }, { color: "blue" })).toBe(false);
	});

	it("detects missing keys", () => {
		expect(argsEqual({ a: 1, b: 2 }, { a: 1 })).toBe(false);
	});

	it("handles nulls", () => {
		expect(argsEqual(null, null)).toBe(true);
		expect(argsEqual(null, {})).toBe(false);
		expect(argsEqual({}, null)).toBe(false);
	});

	it("handles primitives", () => {
		expect(argsEqual(42, 42)).toBe(true);
		expect(argsEqual(42, 43)).toBe(false);
		expect(argsEqual("a", "a")).toBe(true);
	});
});
