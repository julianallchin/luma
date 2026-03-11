import { describe, expect, it } from "vitest";
import { parse } from "../parser";
import { createTestRegistry } from "./fixtures";

const registry = createTestRegistry();

describe("parser", () => {
	describe("annotations", () => {
		it("parses a single annotation with integer bar range", () => {
			const result = parse("solid_color(all) @1-5 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers).toHaveLength(1);
			expect(result.document.layers[0]).toHaveLength(1);
			const ann = result.document.layers[0][0];
			expect(ann.range).toEqual({ start: 1, end: 5 });
		});

		it("parses single bar shorthand (@5 → [5, 6))", () => {
			const result = parse("solid_color(all) @5 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].range).toEqual({
				start: 5,
				end: 6,
			});
		});

		it("parses bar:beat notation", () => {
			const result = parse("solid_color(all) @5:3-7:2 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].range).toEqual({
				start: 5.5,
				end: 7.25,
			});
		});

		it("parses bar:beat:sub notation", () => {
			const result = parse("solid_color(all) @5:3:2 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			// bar 5, beat 3, sub 2 → 5 + (3-1)/4 + (2-1)/(4*4) = 5 + 0.5 + 0.0625 = 5.5625
			expect(result.document.layers[0][0].range.start).toBeCloseTo(5.5625, 9);
		});

		it("parses pattern with color arg", () => {
			const result = parse("solid_color(all) @1 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const ann = result.document.layers[0][0];
			expect(ann.pattern).toBe("solid_color");
			expect(ann.args).toHaveLength(1);
			expect(ann.args[0].key).toBe("color");
			expect(ann.args[0].value).toEqual({ type: "color", hex: "#ff0000" });
		});

		it("parses pattern with multiple args", () => {
			const result = parse(
				"intensity_spikes(hit) @1 color=#ffffff subdivision=2 max_dimmer=0.8",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const ann = result.document.layers[0][0];
			expect(ann.pattern).toBe("intensity_spikes");
			expect(ann.args).toHaveLength(3);
			expect(ann.args[0]).toMatchObject({
				key: "color",
				value: { type: "color", hex: "#ffffff" },
			});
			expect(ann.args[1]).toMatchObject({
				key: "subdivision",
				value: { type: "number", value: 2 },
			});
			expect(ann.args[2]).toMatchObject({
				key: "max_dimmer",
				value: { type: "number", value: 0.8 },
			});
		});

		it("parses pattern with no args", () => {
			const result = parse("smooth_dimmer_noise(wash) @1", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const ann = result.document.layers[0][0];
			expect(ann.pattern).toBe("smooth_dimmer_noise");
			expect(ann.args).toHaveLength(0);
		});
	});

	describe("layers", () => {
		it("groups annotations into layers separated by blank lines", () => {
			const src = [
				"solid_color(all) @1-9 color=#000044",
				"",
				"intensity_spikes(hit) @5-9 subdivision=2 blend=add",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers).toHaveLength(2);
			expect(result.document.layers[0]).toHaveLength(1);
			expect(result.document.layers[1]).toHaveLength(1);
		});

		it("keeps multiple annotations in the same layer when not separated by blank line", () => {
			const src = [
				"solid_color(all) @1-5 color=#000044",
				"solid_color(all) @5-9 color=#110022",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers).toHaveLength(1);
			expect(result.document.layers[0]).toHaveLength(2);
		});

		it("warns on overlapping annotations within a layer", () => {
			const src = [
				"solid_color(all) @1-5 color=#000044",
				"solid_color(all) @3-9 color=#110022",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.warnings).toHaveLength(1);
			expect(result.warnings[0].code).toBe("overlap");
		});

		it("does not warn on adjacent non-overlapping annotations", () => {
			const src = [
				"solid_color(all) @1-5 color=#000044",
				"solid_color(all) @5-9 color=#110022",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.warnings).toHaveLength(0);
		});

		it("handles multiple layers", () => {
			const src = [
				"# Layer 0 — base",
				"solid_color(all) @1-17 color=#000044",
				"",
				"# Layer 1 — rhythmic",
				"intensity_spikes(hit) @5-9 subdivision=2 blend=add",
				"bass_strobe(hit) @9-17 rate=0.9 blend=add",
				"",
				"# Layer 2 — accents",
				"random_dimmer_mask(accent) @9-17 subdivision=2 count=3 color=#ff4400 blend=add",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers).toHaveLength(3);
			expect(result.document.layers[0]).toHaveLength(1);
			expect(result.document.layers[1]).toHaveLength(2);
			expect(result.document.layers[2]).toHaveLength(1);
		});
	});

	describe("blend mode", () => {
		it("parses blend=add", () => {
			const result = parse(
				"intensity_spikes(hit) @1 color=#ffffff subdivision=2 max_dimmer=0.8 blend=add",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			const ann = result.document.layers[0][0];
			expect(ann.blend).toBe("add");
			expect(ann.args.find((a) => a.key === "blend")).toBeUndefined();
		});

		it("defaults to replace when no blend specified", () => {
			const result = parse("solid_color(all) @1 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].blend).toBe("replace");
		});

		it("errors on invalid blend mode", () => {
			const result = parse(
				"solid_color(all) @1 color=#ff0000 blend=bogus",
				registry,
			);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("invalid_blend_mode");
		});
	});

	describe("group expressions", () => {
		it("parses simple group", () => {
			const result = parse("solid_color(all) @1 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "group",
				name: "all",
			});
		});

		it("parses AND expression", () => {
			const result = parse(
				"solid_color(left & wash) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "and",
				left: { type: "group", name: "left" },
				right: { type: "group", name: "wash" },
			});
		});

		it("parses OR expression", () => {
			const result = parse(
				"solid_color(hit | accent) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "or",
				left: { type: "group", name: "hit" },
				right: { type: "group", name: "accent" },
			});
		});

		it("parses NOT expression", () => {
			const result = parse("solid_color(~wash) @1 color=#ff0000", registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "not",
				operand: { type: "group", name: "wash" },
			});
		});

		it("parses XOR expression", () => {
			const result = parse(
				"solid_color(left ^ right) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "xor",
				left: { type: "group", name: "left" },
				right: { type: "group", name: "right" },
			});
		});

		it("parses fallback expression", () => {
			const result = parse(
				"solid_color(wash > all) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "fallback",
				left: { type: "group", name: "wash" },
				right: { type: "group", name: "all" },
			});
		});

		it("parses grouped expression", () => {
			const result = parse(
				"solid_color((left | right) & wash) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "and",
				left: {
					type: "paren",
					inner: {
						type: "or",
						left: { type: "group", name: "left" },
						right: { type: "group", name: "right" },
					},
				},
				right: { type: "group", name: "wash" },
			});
		});

		it("respects operator precedence: & binds tighter than |", () => {
			const result = parse(
				"solid_color(hit | left & wash) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "or",
				left: { type: "group", name: "hit" },
				right: {
					type: "and",
					left: { type: "group", name: "left" },
					right: { type: "group", name: "wash" },
				},
			});
		});

		it("respects operator precedence: | binds tighter than >", () => {
			const result = parse(
				"solid_color(wash | hit > all) @1 color=#ff0000",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers[0][0].selection).toEqual({
				type: "fallback",
				left: {
					type: "or",
					left: { type: "group", name: "wash" },
					right: { type: "group", name: "hit" },
				},
				right: { type: "group", name: "all" },
			});
		});
	});

	describe("comments", () => {
		it("ignores comment lines", () => {
			const src = [
				"# Intro — ambient wash",
				"solid_color(all) @1-5 color=#000044",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers).toHaveLength(1);
			expect(result.document.layers[0]).toHaveLength(1);
		});

		it("ignores comments between layers", () => {
			const src = [
				"solid_color(all) @1-5 color=#000044",
				"",
				"# Build section",
				"intensity_spikes(hit) @5-9 blend=add",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.document.layers).toHaveLength(2);
		});
	});

	describe("full example", () => {
		it("parses a multi-layer annotation-centric score", () => {
			const src = [
				"# Layer 0 — base wash",
				"solid_color(all) @1-5 color=#000044",
				"solid_color(all) @5-9 color=#110022",
				"",
				"# Layer 1 — rhythmic",
				"intensity_spikes(hit) @5-9 color=#ffffff subdivision=2 max_dimmer=0.8 blend=add",
				"",
				"# Layer 2 — drop",
				"major_axis_chase(wash) @9-13 color=#ff0000 subdivision=1",
				"bass_strobe(hit) @9-13 rate=0.9 blend=add",
				"random_dimmer_mask(accent) @9-13 subdivision=2 count=3 color=#ff4400 blend=add",
				"",
				"# Layer 3 — breakdown",
				"solid_color(all) @17-21 color=#000000",
				"linear_dimmer_fade(wash) @17-21 start_value=0 end_value=0.3",
			].join("\n");

			const result = parse(src, registry);
			expect(result.ok).toBe(true);
			if (!result.ok) {
				console.log(result.errors);
				return;
			}

			const { layers } = result.document;
			expect(layers).toHaveLength(4);

			// Layer 0: 2 annotations
			expect(layers[0]).toHaveLength(2);
			expect(layers[0][0].range).toEqual({ start: 1, end: 5 });
			expect(layers[0][1].range).toEqual({ start: 5, end: 9 });

			// Layer 1: 1 annotation
			expect(layers[1]).toHaveLength(1);
			expect(layers[1][0].blend).toBe("add");

			// Layer 2: 3 annotations
			expect(layers[2]).toHaveLength(3);

			// Layer 3: 2 annotations
			expect(layers[3]).toHaveLength(2);
		});
	});

	describe("validation", () => {
		it("errors on unknown pattern", () => {
			const result = parse("pulse(hit) @1 color=#ffffff", registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("unknown_pattern");
			expect(result.errors[0].message).toContain("pulse");
			expect(result.errors[0].hint).toContain("solid_color");
		});

		it("warns on unknown arg", () => {
			const result = parse(
				"solid_color(all) @1 color=#ff0000 bogus=42",
				registry,
			);
			expect(result.ok).toBe(true);
			if (!result.ok) return;
			expect(result.warnings).toHaveLength(1);
			expect(result.warnings[0].code).toBe("unknown_arg");
		});

		it("errors on type mismatch: color arg gets number", () => {
			const result = parse("solid_color(all) @1 color=42", registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("type_mismatch");
		});

		it("errors on type mismatch: scalar arg gets color", () => {
			const result = parse(
				"intensity_spikes(hit) @1 subdivision=#ff0000",
				registry,
			);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("type_mismatch");
			expect(result.errors[0].message).toContain("subdivision");
		});

		it("errors on invalid bar range (end <= start)", () => {
			const result = parse("solid_color(all) @8-3 color=#ff0000", registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.errors[0].code).toBe("invalid_bar_range");
		});

		it("returns partial AST on errors", () => {
			const src = [
				"solid_color(all) @1-5 color=#ff0000",
				"",
				"bogus_pattern(hit) @5-9",
				"",
				"solid_color(all) @9-13 color=#0000ff",
			].join("\n");
			const result = parse(src, registry);
			expect(result.ok).toBe(false);
			if (result.ok) return;
			expect(result.partial).not.toBeNull();
			// Should still have three layers (the middle one empty after error)
			expect(result.partial?.layers.length).toBeGreaterThanOrEqual(2);
		});
	});
});
