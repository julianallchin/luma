import { describe, expect, it } from "vitest";
import { formatError } from "../errors";
import { parse } from "../parser";
import { createTestRegistry } from "./fixtures";

const registry = createTestRegistry();

describe("error formatting", () => {
	it("formats unknown pattern error with caret", () => {
		const src = "@1\npulse(hit) color=#ffffff";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		const formatted = formatError(result.errors[0], src);
		expect(formatted).toContain("line 2");
		expect(formatted).toContain("unknown pattern");
		expect(formatted).toContain("^^^^^");
		expect(formatted).toContain("pulse(hit) color=#ffffff");
	});

	it("formats type mismatch error", () => {
		const src = "@1\nintensity_spikes(hit) subdivision=#ff0000";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		const formatted = formatError(result.errors[0], src);
		expect(formatted).toContain("subdivision");
		expect(formatted).toContain("^");
	});

	it("includes hint text when available", () => {
		const src = "@1\npulse(hit)";
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
			"@1-4",
			"solid_color(all) color=#ff0000",
			"",
			"@5-8",
			"unknown_thing(hit)",
			"",
			"@9-12",
			"solid_color(all) color=#0000ff",
		].join("\n");
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		expect(result.errors).toHaveLength(1);
		expect(result.errors[0].code).toBe("unknown_pattern");

		// Partial AST should have all three blocks
		expect(result.partial).not.toBeNull();
		expect(result.partial?.bars).toHaveLength(3);
		// Block 1 and 3 should have layers
		expect(result.partial?.bars[0].layers).toHaveLength(1);
		expect(result.partial?.bars[2].layers).toHaveLength(1);
	});

	it("accumulates multiple errors", () => {
		const src = ["@1", "bogus_a(all)", "", "@2", "bogus_b(all)"].join("\n");
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;

		expect(result.errors).toHaveLength(2);
		expect(result.errors[0].message).toContain("bogus_a");
		expect(result.errors[1].message).toContain("bogus_b");
	});

	it("handles missing selection parens", () => {
		const src = "@1\nsolid_color color=#ff0000";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;
		expect(result.errors[0].code).toBe("missing_selection");
	});

	it("handles empty document", () => {
		const result = parse("", registry);
		expect(result.ok).toBe(true);
		if (!result.ok) return;
		expect(result.document.bars).toHaveLength(0);
	});

	it("handles comment-only document", () => {
		const result = parse("# just a comment\n# another comment", registry);
		expect(result.ok).toBe(true);
		if (!result.ok) return;
		expect(result.document.bars).toHaveLength(0);
	});
});

describe("span accuracy", () => {
	it("reports correct line for errors on line 1", () => {
		const src = "bogus(all)";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;
		// The error is "expected bar header", since bogus isn't preceded by @
		expect(result.errors[0].span.start.line).toBe(1);
	});

	it("reports correct line for errors after blank lines", () => {
		const src = "@1\nsolid_color(all) color=#ff0000\n\n\n@5\nbogus(all)";
		const result = parse(src, registry);
		expect(result.ok).toBe(false);
		if (result.ok) return;
		const bogusError = result.errors.find((e) => e.message.includes("bogus"));
		expect(bogusError).toBeDefined();
		expect(bogusError?.span.start.line).toBe(6);
	});
});
