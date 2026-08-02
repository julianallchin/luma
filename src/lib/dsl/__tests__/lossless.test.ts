import { describe, expect, it } from "vitest";
import type {
	BeatGrid,
	BlendMode,
	PatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import {
	type AnnotationInput,
	annotationsToDsl,
	buildRegistry,
	dslToAnnotations,
} from "../convert";
import { parse } from "../parser";

const BEAT_GRID: BeatGrid = {
	beats: [0, 0.5, 1, 1.5, 2, 2.5, 3, 3.5],
	downbeats: [0, 2, 4],
	bpm: 120,
	downbeatOffset: 0,
	beatsPerBar: 4,
};

function pattern(id: string, name: string): PatternSummary {
	return {
		id,
		uid: null,
		name,
		description: null,
		categoryName: null,
		createdAt: "",
		updatedAt: "",
		isVerified: false,
		authorName: null,
		forkedFromId: null,
	};
}

const PATTERNS = [pattern("pattern-a", "all_values")];

const ARGS: Record<string, PatternArgDef[]> = {
	"pattern-a": [
		{
			id: "selection",
			name: "Target",
			argType: "Selection",
			defaultValue: {
				expression: "all",
				spatialReference: "global",
			},
		},
		{
			id: "selection_2",
			name: "Secondary target",
			argType: "Selection",
			defaultValue: {
				expression: "all",
				spatialReference: "global",
			},
		},
		{
			id: "amount",
			name: "Amount",
			argType: "Scalar",
			defaultValue: 0 as unknown as Record<string, unknown>,
		},
		{
			id: "color",
			name: "Color",
			argType: "Color",
			defaultValue: { r: 255, g: 255, b: 255, a: 1 },
		},
		{
			id: "palette",
			name: "Palette",
			argType: "Palette",
			defaultValue: { colors: [] },
		},
		{
			id: "gradient",
			name: "Gradient",
			argType: "Gradient",
			defaultValue: { stops: [] },
		},
	],
};

function compile(source: string, beatGrid = BEAT_GRID) {
	const result = parse(source, buildRegistry(PATTERNS, ARGS), {
		beatsPerBar: beatGrid.beatsPerBar,
	});
	if (!result.ok) {
		throw new Error(result.errors.map((error) => error.message).join("\n"));
	}
	return dslToAnnotations(result.document, beatGrid, PATTERNS, ARGS);
}

describe("lossless score DSL", () => {
	it("preserves identity, exact time, z-index, blend, and every JSON arg", () => {
		const original: AnnotationInput = {
			id: "67b6b29f-6863-4889-91d7-058b590d91e4",
			patternId: "pattern-a",
			startTime: 0.3333333333333333,
			endTime: 1.23456789012345,
			zIndex: -7,
			blendMode: "subtract" as BlendMode,
			args: {
				selection: {
					expression: "left & wash",
					spatialReference: "group_local",
				},
				selection_2: {
					expression: "right",
					spatialReference: "global",
				},
				amount: 0.30000000000000004,
				color: { r: 12.5, g: 34, b: 56, a: 0.5 },
				palette: { colors: ["#ff0080", "#00ffc8"] },
				gradient: {
					stops: [
						{ color: "#000000", t: 0 },
						{ color: "#ffffff80", t: 0.3333333333333333 },
					],
				},
				orphaned_arg: {
					nested: [true, false, null, "unchanged"],
				},
				blend: "this is an arg, not the clip blend mode",
			},
		};

		const source = annotationsToDsl([original], BEAT_GRID, PATTERNS, ARGS);

		expect(source).toContain("layer -7:");
		expect(source).toContain(`"${original.id}":`);
		expect(source).toContain('all_values["pattern-a"]');
		expect(source).toContain("@0.3333333333333333s-1.23456789012345s");
		expect(source).toContain("(group_local: left & wash)");
		expect(source).toContain("amount=0.30000000000000004");
		expect(source).toContain('"blend"=');
		expect(source).toContain("blend=subtract");

		expect(compile(source)).toEqual([original]);

		const canonicalAgain = annotationsToDsl(
			compile(source),
			BEAT_GRID,
			PATTERNS,
			ARGS,
		);
		expect(canonicalAgain).toBe(source);
	});

	it("uses musical notation only when both boundaries round-trip exactly", () => {
		const aligned: AnnotationInput = {
			id: "aligned",
			patternId: "pattern-a",
			startTime: 1,
			endTime: 4,
			zIndex: 0,
			blendMode: "replace",
			args: {},
		};
		const source = annotationsToDsl([aligned], BEAT_GRID, PATTERNS, ARGS);
		expect(source).toContain("@1:3-3");
		expect(source).not.toContain("s-");
		expect(compile(source)).toEqual([aligned]);
	});

	it("distinguishes an absent Selection override from explicit all", () => {
		const original: AnnotationInput = {
			id: "default-selection",
			patternId: "pattern-a",
			startTime: 0,
			endTime: 2,
			zIndex: 0,
			blendMode: "replace",
			args: {},
		};
		const source = annotationsToDsl([original], BEAT_GRID, PATTERNS, ARGS);
		expect(source).toContain('all_values["pattern-a"]()');
		expect(compile(source)).toEqual([original]);
	});

	it("requires a stable id when a pattern name is ambiguous", () => {
		const patterns = [
			pattern("pattern-a", "duplicate"),
			pattern("pattern-b", "duplicate"),
		];
		const registry = buildRegistry(patterns, {
			"pattern-a": [],
			"pattern-b": [],
		});

		expect(parse("duplicate(all) @1", registry).ok).toBe(false);
		const qualified = parse('duplicate["pattern-b"](all) @1', registry);
		expect(qualified.ok).toBe(true);
		if (!qualified.ok) return;
		expect(
			dslToAnnotations(qualified.document, BEAT_GRID, patterns, {
				"pattern-a": [],
				"pattern-b": [],
			})[0].patternId,
		).toBe("pattern-b");
	});

	it("quotes non-identifier pattern names and preserves legacy selections verbatim", () => {
		const patterns = [pattern("numeric-pattern", "250_500hz_bass_pulse")];
		const args = {
			"numeric-pattern": ARGS["pattern-a"],
		};
		const original: AnnotationInput = {
			id: "legacy",
			patternId: "numeric-pattern",
			startTime: 0,
			endTime: 2,
			zIndex: 0,
			blendMode: "replace",
			args: {
				selection: {
					expression: "front_led_bars side_pars ",
					spatialReference: "global",
				},
			},
		};
		const source = annotationsToDsl([original], BEAT_GRID, patterns, args);
		expect(source).toContain('"250_500hz_bass_pulse"["numeric-pattern"]()');
		expect(source).toContain(
			'selection={"expression":"front_led_bars side_pars ","spatialReference":"global"}',
		);

		const parsed = parse(source, buildRegistry(patterns, args));
		expect(parsed.ok).toBe(true);
		if (!parsed.ok) return;
		expect(
			dslToAnnotations(parsed.document, BEAT_GRID, patterns, args),
		).toEqual([original]);
	});

	it("compiles exact-second scores without a beat grid and rejects bar timing", () => {
		const noGrid: BeatGrid = {
			beats: [],
			downbeats: [],
			bpm: 120,
			downbeatOffset: 0,
			beatsPerBar: 4,
		};
		const exact = compile(
			'"clip": all_values["pattern-a"]() @0.125s-1.75s',
			noGrid,
		);
		expect(exact[0].startTime).toBe(0.125);
		expect(exact[0].endTime).toBe(1.75);

		const parsedBars = parse(
			'all_values["pattern-a"]() @1-2',
			buildRegistry(PATTERNS, ARGS),
		);
		expect(parsedBars.ok).toBe(true);
		if (!parsedBars.ok) return;
		expect(() =>
			dslToAnnotations(parsedBars.document, noGrid, PATTERNS, ARGS),
		).toThrow("without a beat grid");
	});

	it("fails loudly instead of dropping unavailable patterns or syntax", () => {
		const missingPattern: AnnotationInput = {
			id: "missing",
			patternId: "not-installed",
			startTime: 0,
			endTime: 1,
			zIndex: 0,
			blendMode: "replace",
			args: {},
		};
		expect(() =>
			annotationsToDsl([missingPattern], BEAT_GRID, PATTERNS, ARGS),
		).toThrow("unavailable");
		expect(
			parse(
				'all_values["pattern-a"]() @1 amount=1;',
				buildRegistry(PATTERNS, ARGS),
			).ok,
		).toBe(false);
	});

	it("keeps stable arg IDs distinct when display names duplicate and collide", () => {
		const patterns = [pattern("colliding-pattern", "colliding_args")];
		const args: Record<string, PatternArgDef[]> = {
			"colliding-pattern": [
				{
					id: "amount",
					name: "shared",
					argType: "Scalar",
					defaultValue: 0 as unknown as Record<string, unknown>,
				},
				{
					id: "shared",
					name: "shared",
					argType: "Color",
					defaultValue: { r: 255, g: 255, b: 255, a: 1 },
				},
			],
		};
		const original: AnnotationInput = {
			id: "colliding-clip",
			patternId: "colliding-pattern",
			startTime: 0,
			endTime: 2,
			zIndex: 0,
			blendMode: "replace",
			// Put the colliding stable ID first to exercise the old first-match loss.
			args: {
				shared: { r: 12, g: 34, b: 56, a: 1 },
				amount: 0.75,
			},
		};

		const source = annotationsToDsl([original], BEAT_GRID, patterns, args);
		expect(source.match(/\bamount=/g)).toHaveLength(1);
		expect(source.match(/\bshared=/g)).toHaveLength(1);

		const parsed = parse(source, buildRegistry(patterns, args), {
			beatsPerBar: BEAT_GRID.beatsPerBar,
		});
		expect(parsed.ok).toBe(true);
		if (!parsed.ok) return;
		expect(
			dslToAnnotations(parsed.document, BEAT_GRID, patterns, args),
		).toEqual([original]);
	});

	it("rejects a display-name alias shared by multiple stable args", () => {
		const patterns = [pattern("ambiguous-pattern", "ambiguous_args")];
		const args: Record<string, PatternArgDef[]> = {
			"ambiguous-pattern": [
				{
					id: "left_gain",
					name: "gain",
					argType: "Scalar",
					defaultValue: 0 as unknown as Record<string, unknown>,
				},
				{
					id: "right_gain",
					name: "gain",
					argType: "Scalar",
					defaultValue: 0 as unknown as Record<string, unknown>,
				},
			],
		};
		const parsed = parse(
			'ambiguous_args["ambiguous-pattern"]() @0s-1s gain=0.5',
			buildRegistry(patterns, args),
		);
		expect(parsed.ok).toBe(true);
		if (!parsed.ok) return;
		expect(() =>
			dslToAnnotations(parsed.document, BEAT_GRID, patterns, args),
		).toThrow('arg name "gain" is ambiguous');
	});
});
