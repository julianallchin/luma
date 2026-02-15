import { describe, expect, it } from "vitest";
import { parse } from "../parser";
import type { PatternLayer } from "../types";
import { createTestRegistry } from "./fixtures";

const registry = createTestRegistry();

describe("parser", () => {
	describe("bar blocks", () => {
		it("parses a single bar", () => {
			const result = parse("@1\nsolid_color(all) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars).toHaveLength(1);
			expect(result.document.bars[0].range).toEqual({ start: 1, end: 1 });
		});

		it("parses a bar range", () => {
			const result = parse("@5-8\nsolid_color(all) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars[0].range).toEqual({ start: 5, end: 8 });
		});

		it("parses multiple bar blocks", () => {
			const src = [
				"@1-4",
				"solid_color(all) color=#000044",
				"",
				"@5-8",
				"solid_color(all) color=#110022",
			].join("\n");

			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars).toHaveLength(2);
			expect(result.document.bars[0].range).toEqual({ start: 1, end: 4 });
			expect(result.document.bars[1].range).toEqual({ start: 5, end: 8 });
		});
	});

	describe("pattern layers", () => {
		it("parses solid_color with color arg", () => {
			const result = parse("@1\nsolid_color(all) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.type).toBe("pattern");
			expect(layer.pattern).toBe("solid_color");
			expect(layer.args).toHaveLength(1);
			expect(layer.args[0].key).toBe("color");
			expect(layer.args[0].value).toEqual({ type: "color", hex: "#ff0000" });
		});

		it("parses intensity_spikes with multiple args", () => {
			const result = parse(
				"@1\nintensity_spikes(hit) color=#ffffff subdivision=2 max_dimmer=0.8",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.pattern).toBe("intensity_spikes");
			expect(layer.args).toHaveLength(3);
			expect(layer.args[0]).toMatchObject({
				key: "color",
				value: { type: "color", hex: "#ffffff" },
			});
			expect(layer.args[1]).toMatchObject({
				key: "subdivision",
				value: { type: "number", value: 2 },
			});
			expect(layer.args[2]).toMatchObject({
				key: "max_dimmer",
				value: { type: "number", value: 0.8 },
			});
		});

		it("parses pattern with no args (smooth_dimmer_noise)", () => {
			const result = parse("@1\nsmooth_dimmer_noise(wash)", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.pattern).toBe("smooth_dimmer_noise");
			expect(layer.args).toHaveLength(0);
		});

		it("parses multiple layers in a bar block", () => {
			const src = [
				"@1-4",
				"solid_color(all) color=#000044",
				"smooth_dimmer_noise(wash)",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars[0].layers).toHaveLength(2);
		});
	});

	describe("blend mode", () => {
		it("parses blend=add as layer meta-param", () => {
			const result = parse(
				"@1\nintensity_spikes(hit) color=#ffffff subdivision=2 max_dimmer=0.8 blend=add",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.blend).toBe("add");
			// blend should NOT appear in args
			expect(layer.args.find((a) => a.key === "blend")).toBeUndefined();
		});

		it("defaults to replace when no blend specified", () => {
			const result = parse("@1\nsolid_color(all) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.blend).toBe("replace");
		});

		it("errors on invalid blend mode", () => {
			const result = parse(
				"@1\nsolid_color(all) color=#ff0000 blend=bogus",
				registry,
			);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("invalid_blend_mode");
		});
	});

	describe("hold", () => {
		it("parses hold layer", () => {
			const src = [
				"@1-4",
				"solid_color(all) color=#ff0000",
				"",
				"@5-8",
				"hold",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars[1].layers[0].type).toBe("hold");
		});
	});

	describe("tag expressions", () => {
		it("parses simple tag", () => {
			const result = parse("@1\nsolid_color(all) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({ type: "tag", name: "all" });
		});

		it("parses AND expression", () => {
			const result = parse(
				"@1\nsolid_color(left & wash) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({
				type: "and",
				left: { type: "tag", name: "left" },
				right: { type: "tag", name: "wash" },
			});
		});

		it("parses OR expression", () => {
			const result = parse(
				"@1\nsolid_color(hit | accent) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({
				type: "or",
				left: { type: "tag", name: "hit" },
				right: { type: "tag", name: "accent" },
			});
		});

		it("parses NOT expression", () => {
			const result = parse("@1\nsolid_color(~wash) color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({
				type: "not",
				operand: { type: "tag", name: "wash" },
			});
		});

		it("parses XOR expression", () => {
			const result = parse(
				"@1\nsolid_color(left ^ right) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({
				type: "xor",
				left: { type: "tag", name: "left" },
				right: { type: "tag", name: "right" },
			});
		});

		it("parses fallback expression", () => {
			const result = parse(
				"@1\nsolid_color(wash > all) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({
				type: "fallback",
				left: { type: "tag", name: "wash" },
				right: { type: "tag", name: "all" },
			});
		});

		it("parses grouped expression", () => {
			const result = parse(
				"@1\nsolid_color((left | right) & wash) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			expect(layer.selection).toEqual({
				type: "and",
				left: {
					type: "group",
					inner: {
						type: "or",
						left: { type: "tag", name: "left" },
						right: { type: "tag", name: "right" },
					},
				},
				right: { type: "tag", name: "wash" },
			});
		});

		it("respects operator precedence: & binds tighter than |", () => {
			const result = parse(
				"@1\nsolid_color(hit | left & wash) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			// Should parse as: hit | (left & wash)
			expect(layer.selection).toEqual({
				type: "or",
				left: { type: "tag", name: "hit" },
				right: {
					type: "and",
					left: { type: "tag", name: "left" },
					right: { type: "tag", name: "wash" },
				},
			});
		});

		it("respects operator precedence: | binds tighter than >", () => {
			const result = parse(
				"@1\nsolid_color(wash | hit > all) color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const layer = result.document.bars[0].layers[0] as PatternLayer;
			// Should parse as: (wash | hit) > all
			expect(layer.selection).toEqual({
				type: "fallback",
				left: {
					type: "or",
					left: { type: "tag", name: "wash" },
					right: { type: "tag", name: "hit" },
				},
				right: { type: "tag", name: "all" },
			});
		});
	});

	describe("comments", () => {
		it("ignores top-level comments", () => {
			const src = [
				"# Intro — ambient wash",
				"@1-4",
				"solid_color(all) color=#000044",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars).toHaveLength(1);
		});

		it("ignores comments between bar blocks", () => {
			const src = [
				"@1-4",
				"solid_color(all) color=#000044",
				"",
				"# Build section",
				"@5-8",
				"solid_color(all) color=#110022",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.bars).toHaveLength(2);
		});
	});

	describe("full example", () => {
		it("parses the example from the spec", () => {
			const src = [
				"# Intro — ambient wash",
				"@1-4",
				"solid_color(all) color=#000044",
				"smooth_dimmer_noise(wash)",
				"",
				"# Build — add rhythmic elements",
				"@5-8",
				"solid_color(all) color=#110022",
				"intensity_spikes(hit) color=#ffffff subdivision=2 max_dimmer=0.8 blend=add",
				"",
				"# Drop",
				"@9-12",
				"major_axis_chase(wash) color=#ff0000 subdivision=1",
				"bass_strobe(hit) color=#ffffff rate=0.9 blend=add",
				"random_dimmer_mask(accent) subdivision=2 count=3 color=#ff4400 blend=add",
				"",
				"# Sustain",
				"@13-16",
				"hold",
				"",
				"# Breakdown",
				"@17-20",
				"solid_color(all) color=#000000",
				"linear_dimmer_fade(wash) start_value=0 end_value=0.3",
			].join("\n");

			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) {
				console.log(result.errors);
				return;
			}

			const { bars } = result.document;
			expect(bars).toHaveLength(5);

			// Block 1: @1-4
			expect(bars[0].range).toEqual({ start: 1, end: 4 });
			expect(bars[0].layers).toHaveLength(2);

			// Block 2: @5-8
			expect(bars[1].range).toEqual({ start: 5, end: 8 });
			expect(bars[1].layers).toHaveLength(2);
			const spikes = bars[1].layers[1] as PatternLayer;
			expect(spikes.blend).toBe("add");

			// Block 3: @9-12
			expect(bars[2].range).toEqual({ start: 9, end: 12 });
			expect(bars[2].layers).toHaveLength(3);

			// Block 4: @13-16 hold
			expect(bars[3].range).toEqual({ start: 13, end: 16 });
			expect(bars[3].layers[0].type).toBe("hold");

			// Block 5: @17-20
			expect(bars[4].range).toEqual({ start: 17, end: 20 });
			expect(bars[4].layers).toHaveLength(2);
		});
	});

	describe("validation", () => {
		it("errors on unknown pattern", () => {
			const result = parse("@1\npulse(hit) color=#ffffff", registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("unknown_pattern");
			expect(result.errors[0].message).toContain("pulse");
			expect(result.errors[0].hint).toContain("solid_color");
		});

		it("warns on unknown arg", () => {
			const result = parse(
				"@1\nsolid_color(all) color=#ff0000 bogus=42",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.warnings).toHaveLength(1);
			expect(result.warnings[0].code).toBe("unknown_arg");
		});

		it("errors on type mismatch: color arg gets number", () => {
			const result = parse("@1\nsolid_color(all) color=42", registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("type_mismatch");
		});

		it("errors on type mismatch: scalar arg gets color", () => {
			const result = parse(
				"@1\nintensity_spikes(hit) subdivision=#ff0000",
				registry,
			);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("type_mismatch");
			expect(result.errors[0].message).toContain("subdivision");
		});

		it("errors on invalid bar range (end < start)", () => {
			const result = parse("@8-3\nsolid_color(all) color=#ff0000", registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("invalid_bar_range");
		});

		it("returns partial AST on errors", () => {
			const src = [
				"@1-4",
				"solid_color(all) color=#ff0000",
				"",
				"@5-8",
				"bogus_pattern(hit)",
				"",
				"@9-12",
				"solid_color(all) color=#0000ff",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.partial).not.toBeNull();
			// Should still have the valid blocks
			expect(result.partial?.bars).toHaveLength(3);
		});
	});
});
