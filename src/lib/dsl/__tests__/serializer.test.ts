import { describe, expect, it } from "vitest";
import { parse } from "../parser";
import { formatNumber, serialize } from "../serializer";
import type { Document } from "../types";
import { createTestRegistry } from "./fixtures";

const registry = createTestRegistry();

const ZERO_SPAN = {
	start: { line: 0, column: 0, offset: 0 },
	end: { line: 0, column: 0, offset: 0 },
};

describe("serializer", () => {
	describe("basic serialization", () => {
		it("serializes a single annotation", () => {
			const doc: Document = {
				layers: [
					[
						{
							type: "annotation",
							pattern: "solid_color",
							selection: { type: "group", name: "all" },
							range: { start: 1, end: 5 },
							args: [
								{
									key: "color",
									value: { type: "color", hex: "#ff0000" },
									span: ZERO_SPAN,
								},
							],
							blend: "replace",
							span: ZERO_SPAN,
						},
					],
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("solid_color(all) @1-5 color=#ff0000");
		});

		it("uses single-bar shorthand when range is one integer bar", () => {
			const doc: Document = {
				layers: [
					[
						{
							type: "annotation",
							pattern: "solid_color",
							selection: { type: "group", name: "all" },
							range: { start: 5, end: 6 },
							args: [],
							blend: "replace",
							span: ZERO_SPAN,
						},
					],
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("solid_color(all) @5");
		});

		it("serializes fractional bar ranges using bar:beat notation", () => {
			const doc: Document = {
				layers: [
					[
						{
							type: "annotation",
							pattern: "solid_color",
							selection: { type: "group", name: "all" },
							range: { start: 5.5, end: 7.25 },
							args: [
								{
									key: "color",
									value: { type: "color", hex: "#ff0000" },
									span: ZERO_SPAN,
								},
							],
							blend: "replace",
							span: ZERO_SPAN,
						},
					],
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("solid_color(all) @5:3-7:2 color=#ff0000");
		});

		it("serializes blend mode when non-default", () => {
			const doc: Document = {
				layers: [
					[
						{
							type: "annotation",
							pattern: "solid_color",
							selection: { type: "group", name: "all" },
							range: { start: 1, end: 2 },
							args: [
								{
									key: "color",
									value: { type: "color", hex: "#ff0000" },
									span: ZERO_SPAN,
								},
							],
							blend: "add",
							span: ZERO_SPAN,
						},
					],
				],
			};
			const out = serialize(doc, registry);
			expect(out).toContain("blend=add");
		});

		it("emits all args including defaults", () => {
			const doc: Document = {
				layers: [
					[
						{
							type: "annotation",
							pattern: "solid_color",
							selection: { type: "group", name: "all" },
							range: { start: 1, end: 2 },
							args: [
								{
									key: "color",
									value: { type: "color", hex: "#ffffff" },
									span: ZERO_SPAN,
								},
							],
							blend: "replace",
							span: ZERO_SPAN,
						},
					],
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("solid_color(all) @1 color=#ffffff");
		});

		it("separates layers with blank lines", () => {
			const doc: Document = {
				layers: [
					[
						{
							type: "annotation",
							pattern: "solid_color",
							selection: { type: "group", name: "all" },
							range: { start: 1, end: 9 },
							args: [
								{
									key: "color",
									value: { type: "color", hex: "#000044" },
									span: ZERO_SPAN,
								},
							],
							blend: "replace",
							span: ZERO_SPAN,
						},
					],
					[
						{
							type: "annotation",
							pattern: "intensity_spikes",
							selection: { type: "group", name: "hit" },
							range: { start: 5, end: 9 },
							args: [
								{
									key: "subdivision",
									value: { type: "number", value: 2 },
									span: ZERO_SPAN,
								},
							],
							blend: "add",
							span: ZERO_SPAN,
						},
					],
				],
			};
			const out = serialize(doc, registry);
			const lines = out.split("\n");
			expect(lines).toHaveLength(3); // ann1, blank, ann2
			expect(lines[0]).toContain("solid_color");
			expect(lines[1]).toBe("");
			expect(lines[2]).toContain("intensity_spikes");
		});
	});

	describe("group expression serialization", () => {
		it("serializes AND", () => {
			const result = parse(
				"solid_color(left & wash) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(left & wash)");
		});

		it("serializes OR", () => {
			const result = parse(
				"solid_color(hit | accent) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(hit | accent)");
		});

		it("serializes NOT", () => {
			const result = parse("solid_color(~wash) @1 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(~wash)");
		});

		it("serializes grouped expression with parens", () => {
			const result = parse(
				"solid_color((left | right) & wash) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color((left | right) & wash)");
		});

		it("serializes fallback", () => {
			const result = parse(
				"solid_color(wash > all) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(wash > all)");
		});
	});

	describe("roundtrip", () => {
		it("roundtrips a multi-layer score", () => {
			const src = [
				"solid_color(all) @1-5 color=#000044",
				"smooth_dimmer_noise(wash) @1-5",
				"solid_color(all) @5-9 color=#110022",
				"",
				"intensity_spikes(hit) @5-9 color=#ffffff subdivision=2 max_dimmer=0.8 blend=add",
				"",
				"major_axis_chase(wash) @9-13 color=#ff0000 subdivision=1",
				"bass_strobe(hit) @9-13 rate=0.9 blend=add",
				"random_dimmer_mask(accent) @9-13 subdivision=2 count=3 color=#ff4400 blend=add",
			].join("\n");

			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;

			// Serialize → parse → serialize should be stable
			const serialized1 = serialize(result.document, registry);
			const result2 = parse(serialized1, registry);
			expect(result2.ok).toBe(true);
			if (!result2.ok) return;

			const serialized2 = serialize(result2.document, registry);
			expect(serialized2).toBe(serialized1);
		});

		it("roundtrips complex group expressions", () => {
			const src = "solid_color((left | right) & ~high > wash) @1 color=#ff0000";
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;

			const serialized = serialize(result.document, registry);
			const result2 = parse(serialized, registry);
			expect(result2.ok).toBe(true);
			if (!result2.ok) return;

			const ann1 = result.document.layers[0][0];
			const ann2 = result2.document.layers[0][0];
			expect(ann2.selection).toEqual(ann1.selection);
		});

		it("roundtrips fractional bar ranges with bar:beat notation", () => {
			const src = "solid_color(all) @5:3-7:2 color=#ff0000";
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;

			const serialized = serialize(result.document, registry);
			expect(serialized).toBe(src);
		});
	});

	describe("formatNumber", () => {
		it("formats integers cleanly", () => {
			expect(formatNumber(5)).toBe("5");
			expect(formatNumber(100)).toBe("100");
		});

		it("formats simple fractions", () => {
			expect(formatNumber(5.5)).toBe("5.5");
			expect(formatNumber(7.25)).toBe("7.25");
		});

		it("avoids float artifacts", () => {
			expect(formatNumber(0.1 + 0.2)).toBe("0.3");
		});

		it("rounds to 4 decimal places", () => {
			expect(formatNumber(1.23456789)).toBe("1.2346");
		});
	});
});
