import { describe, expect, it } from "vitest";
import { formatError } from "../errors";
import { parse } from "../parser";
import { createTestRegistry } from "./fixtures";

const registry = createTestRegistry();

describe("error formatting", () => {
	it("formats unknown pattern error with caret", () => {
		const src = "pulse(hit) @1 color=#ffffff";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		const formatted = formatError(result.errors[0], src);
		expect(formatted).toContain("line 1");
		expect(formatted).toContain("unknown pattern");
		expect(formatted).toContain("^^^^^");
		expect(formatted).toContain("pulse(hit) @1 color=#ffffff");
	});

	it("formats type mismatch error", () => {
		const src = "intensity_spikes(hit) @1 subdivision=#ff0000";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		const formatted = formatError(result.errors[0], src);
		expect(formatted).toContain("subdivision");
		expect(formatted).toContain("^");
	});

	it("includes hint text when available", () => {
		const src = "pulse(hit) @1";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		const formatted = formatError(result.errors[0], src);
		expect(formatted).toContain("Available patterns:");
	});
});

describe("error recovery", () => {
	it("recovers from unknown pattern and continues parsing", () => {
		const src = [
			"solid_color(all) @1-5 color=#ff0000",
			"",
			"unknown_thing(hit) @5-9",
			"",
			"solid_color(all) @9-13 color=#0000ff",
		].join("\n");
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		expect(result.errors).toHaveLength(1);
		expect(result.errors[0].code).toBe("unknown_pattern");

		// Partial AST should have layers with valid annotations
		expect(result.partial).not.toBeNull();
		const allAnnotations = result.partial?.layers.flat() ?? [];
		expect(allAnnotations.length).toBeGreaterThanOrEqual(2);
	});

	it("accumulates multiple errors", () => {
		const src = ["bogus_a(all) @1", "", "bogus_b(all) @2"].join("\n");
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		expect(result.errors).toHaveLength(2);
		expect(result.errors[0].message).toContain("bogus_a");
		expect(result.errors[1].message).toContain("bogus_b");
	});

	it("handles missing selection parens", () => {
		const src = "solid_color @1 color=#ff0000";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;
		expect(result.errors[0].code).toBe("missing_selection");
	});

	it("handles empty document", () => {
		const result = parse("", registry);
		expect(result.ok).toBe(true);
		if (!result.ok) return;
		expect(result.document.layers).toHaveLength(0);
	});

	it("handles comment-only document", () => {
		const result = parse("# just a comment\n# another comment", registry);
		expect(result.ok).toBe(true);
		if (!result.ok) return;
		expect(result.document.layers).toHaveLength(0);
	});
});

describe("span accuracy", () => {
	it("reports correct line for errors on line 1", () => {
		const src = "bogus(all) @1";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;
		expect(result.errors[0].span.start.line).toBe(1);
	});

	it("reports correct line for errors after blank lines", () => {
		const src = "solid_color(all) @1 color=#ff0000\n\n\nbogus(all) @5";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;
		const bogusError = result.errors.find((e) => e.message.includes("bogus"));
		expect(bogusError).toBeDefined();
		expect(bogusError?.span.start.line).toBeGreaterThanOrEqual(4);
	});
});
