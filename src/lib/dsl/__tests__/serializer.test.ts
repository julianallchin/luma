import { describe, expect, it } from "vitest";
import { parse } from "../parser";
import { serialize } from "../serializer";
import type { Document, PatternLayer } from "../types";
import { createTestRegistry } from "./fixtures";

const registry = createTestRegistry();

describe("serializer", () => {
	describe("basic serialization", () => {
		it("serializes a simple bar block", () => {
			const doc: Document = {
				bars: [
					{
						range: { start: 1, end: 4 },
						layers: [
							{
								type: "pattern",
								pattern: "solid_color",
								selection: { type: "tag", name: "all" },
								args: [
									{
										key: "color",
										value: { type: "color", hex: "#ff0000" },
										span: {
											start: { line: 1, column: 0, offset: 0 },
											end: { line: 1, column: 0, offset: 0 },
										},
									},
								],
								blend: "replace",
								span: {
									start: { line: 1, column: 0, offset: 0 },
									end: { line: 1, column: 0, offset: 0 },
								},
							},
						],
						span: {
							start: { line: 1, column: 0, offset: 0 },
							end: { line: 1, column: 0, offset: 0 },
						},
					},
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("@1-4\nsolid_color(all) color=#ff0000");
		});

		it("serializes single bar number without dash", () => {
			const doc: Document = {
				bars: [
					{
						range: { start: 5, end: 5 },
						layers: [
							{
								type: "hold",
								span: {
									start: { line: 1, column: 0, offset: 0 },
									end: { line: 1, column: 0, offset: 0 },
								},
							},
						],
						span: {
							start: { line: 1, column: 0, offset: 0 },
							end: { line: 1, column: 0, offset: 0 },
						},
					},
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("@5\nhold");
		});

		it("serializes blend mode when non-default", () => {
			const doc: Document = {
				bars: [
					{
						range: { start: 1, end: 1 },
						layers: [
							{
								type: "pattern",
								pattern: "solid_color",
								selection: { type: "tag", name: "all" },
								args: [
									{
										key: "color",
										value: { type: "color", hex: "#ff0000" },
										span: {
											start: { line: 1, column: 0, offset: 0 },
											end: { line: 1, column: 0, offset: 0 },
										},
									},
								],
								blend: "add",
								span: {
									start: { line: 1, column: 0, offset: 0 },
									end: { line: 1, column: 0, offset: 0 },
								},
							},
						],
						span: {
							start: { line: 1, column: 0, offset: 0 },
							end: { line: 1, column: 0, offset: 0 },
						},
					},
				],
			};
			const out = serialize(doc, registry);
			expect(out).toContain("blend=add");
		});

		it("omits default args", () => {
			// solid_color default color is #ffffff — if we pass that, it should be omitted
			const doc: Document = {
				bars: [
					{
						range: { start: 1, end: 1 },
						layers: [
							{
								type: "pattern",
								pattern: "solid_color",
								selection: { type: "tag", name: "all" },
								args: [
									{
										key: "color",
										value: { type: "color", hex: "#ffffff" },
										span: {
											start: { line: 1, column: 0, offset: 0 },
											end: { line: 1, column: 0, offset: 0 },
										},
									},
								],
								blend: "replace",
								span: {
									start: { line: 1, column: 0, offset: 0 },
									end: { line: 1, column: 0, offset: 0 },
								},
							},
						],
						span: {
							start: { line: 1, column: 0, offset: 0 },
							end: { line: 1, column: 0, offset: 0 },
						},
					},
				],
			};
			const out = serialize(doc, registry);
			expect(out).toBe("@1\nsolid_color(all)");
		});
	});

	describe("tag expression serialization", () => {
		it("serializes AND", () => {
			const result = parse(
				"@1\nsolid_color(left & wash) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(left & wash)");
		});

		it("serializes OR", () => {
			const result = parse(
				"@1\nsolid_color(hit | accent) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(hit | accent)");
		});

		it("serializes NOT", () => {
			const result = parse("@1\nsolid_color(~wash) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(~wash)");
		});

		it("serializes grouped expression with parens", () => {
			const result = parse(
				"@1\nsolid_color((left | right) & wash) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color((left | right) & wash)");
		});

		it("serializes fallback", () => {
			const result = parse(
				"@1\nsolid_color(wash > all) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const out = serialize(result.document, registry);
			expect(out).toContain("solid_color(wash > all)");
		});
	});

	describe("roundtrip", () => {
		it("roundtrips the full example", () => {
			const src = [
				"@1-4",
				"solid_color(all) color=#000044",
				"smooth_dimmer_noise(wash)",
				"",
				"@5-8",
				"solid_color(all) color=#110022",
				"intensity_spikes(hit) color=#ffffff subdivision=2 max_dimmer=0.8 blend=add",
				"",
				"@9-12",
				"major_axis_chase(wash) color=#ff0000 subdivision=1",
				"bass_strobe(hit) rate=0.9 blend=add",
				"random_dimmer_mask(accent) subdivision=2 count=3 color=#ff4400 blend=add",
				"",
				"@13-16",
				"hold",
				"",
				"@17-20",
				"solid_color(all) color=#000000",
				"linear_dimmer_fade(wash) start_value=0 end_value=0.3",
			].join("\n");

			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;

			// Serialize → parse → serialize should be stable (canonical form)
			const serialized1 = serialize(result.document, registry);
			const result2 = parse(serialized1, registry);
			expect(result2.ok).toBe(true);
			if (!result2.ok) return;

			const serialized2 = serialize(result2.document, registry);
			expect(serialized2).toBe(serialized1);
		});

		it("roundtrips complex tag expressions", () => {
			const src =
				"@1\nsolid_color((left | right) & ~high > wash) color=#ff0000";
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;

			const serialized = serialize(result.document, registry);
			const result2 = parse(serialized, registry);
			expect(result2.ok).toBe(true);
			if (!result2.ok) return;

			const layer1 = result.document.bars[0].layers[0] as PatternLayer;
			const layer2 = result2.document.bars[0].layers[0] as PatternLayer;
			expect(layer2.selection).toEqual(layer1.selection);
		});
	});
});
