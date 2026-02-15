import { describe, expect, it } from "vitest";
import type {
	BeatGrid,
	BlendMode,
	PatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import type { TimelineAnnotation } from "@/features/track-editor/stores/use-track-editor-store";
import {
	annotationsToDsl,
	buildRegistry,
	dslToAnnotations,
	hexToRgb,
	parseTagExprString,
} from "../convert";
import { parse } from "../parser";

// ── Test helpers ────────────────────────────────────────────────

function makeBeatGrid(numBars: number, bpm = 120, beatsPerBar = 4): BeatGrid {
	const beatDuration = 60 / bpm;
	const barDuration = beatDuration * beatsPerBar;
	const beats: number[] = [];
	const downbeats: number[] = [];

	for (let bar = 0; bar < numBars; bar++) {
		const barStart = bar * barDuration;
		downbeats.push(barStart);
		for (let beat = 0; beat < beatsPerBar; beat++) {
			beats.push(barStart + beat * beatDuration);
		}
	}

	return { beats, downbeats, bpm, downbeatOffset: 0, beatsPerBar };
}

function makeAnnotation(
	overrides: Partial<TimelineAnnotation> & {
		patternId: number;
		startTime: number;
		endTime: number;
	},
): TimelineAnnotation {
	return {
		id: 1,
		remoteId: null,
		uid: null,
		scoreId: 1,
		zIndex: 0,
		blendMode: "replace" as BlendMode,
		args: {},
		createdAt: "",
		updatedAt: "",
		...overrides,
	};
}

const PATTERNS: PatternSummary[] = [
	{
		id: 1,
		remoteId: null,
		uid: null,
		name: "solid_color",
		description: null,
		categoryId: null,
		categoryName: null,
		createdAt: "",
		updatedAt: "",
		isPublished: false,
		authorName: null,
		forkedFromRemoteId: null,
	},
	{
		id: 2,
		remoteId: null,
		uid: null,
		name: "intensity_spikes",
		description: null,
		categoryId: null,
		categoryName: null,
		createdAt: "",
		updatedAt: "",
		isPublished: false,
		authorName: null,
		forkedFromRemoteId: null,
	},
];

const PATTERN_ARGS: Record<number, PatternArgDef[]> = {
	1: [
		{
			id: "color",
			name: "color",
			argType: "Color",
			defaultValue: { r: 255, g: 255, b: 255 } as unknown as Record<
				string,
				unknown
			>,
		},
	],
	2: [
		{
			id: "subdivision",
			name: "subdivision",
			argType: "Scalar",
			defaultValue: 1 as unknown as Record<string, unknown>,
		},
		{
			id: "color",
			name: "color",
			argType: "Color",
			defaultValue: { r: 255, g: 255, b: 255 } as unknown as Record<
				string,
				unknown
			>,
		},
		{
			id: "selection",
			name: "selection",
			argType: "Selection",
			defaultValue: null as unknown as Record<string, unknown>,
		},
	],
};

// ── Tests ───────────────────────────────────────────────────────

describe("annotationsToDsl", () => {
	it("returns empty string for no annotations", () => {
		const result = annotationsToDsl(
			[],
			makeBeatGrid(4),
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toBe("");
	});

	it("returns empty string for empty beat grid", () => {
		const ann = makeAnnotation({
			patternId: 1,
			startTime: 0,
			endTime: 2,
			args: { color: { r: 255, g: 0, b: 0 } },
		});
		const result = annotationsToDsl(
			[ann],
			{ beats: [], downbeats: [], bpm: 120, downbeatOffset: 0, beatsPerBar: 4 },
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toBe("");
	});

	it("converts a single annotation spanning one bar", () => {
		const beatGrid = makeBeatGrid(4); // 4 bars at 120bpm, each bar = 2s
		const ann = makeAnnotation({
			patternId: 1,
			startTime: 0,
			endTime: 2, // exactly bar 1
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("@1\nsolid_color(all) color=#ff0000");
	});

	it("converts an annotation spanning multiple bars", () => {
		const beatGrid = makeBeatGrid(8);
		const ann = makeAnnotation({
			patternId: 1,
			startTime: 0,
			endTime: 8, // bars 1-4
			args: { color: { r: 0, g: 0, b: 68 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("@1-4\nsolid_color(all) color=#000044");
	});

	it("omits default arg values", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: 1,
			startTime: 0,
			endTime: 2,
			args: { color: { r: 255, g: 255, b: 255 } }, // default
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("@1\nsolid_color(all)");
	});

	it("uses selection expression from args", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: 2,
			startTime: 0,
			endTime: 2,
			args: {
				subdivision: 2,
				color: { r: 255, g: 255, b: 255 },
				selection: { expression: "hit", spatialReference: "global" },
			},
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("@1\nintensity_spikes(hit) subdivision=2");
	});

	it("uses complex selection expressions", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: 2,
			startTime: 0,
			endTime: 2,
			args: {
				subdivision: 1,
				color: { r: 255, g: 255, b: 255 },
				selection: {
					expression: "left & wash",
					spatialReference: "global",
				},
			},
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toContain("intensity_spikes(left & wash)");
	});

	it("emits hold for identical bars after a gap", () => {
		const beatGrid = makeBeatGrid(8);
		// Bars 1-2: red, bars 3-4: blue, bars 5-6: empty, bars 7-8: blue → hold
		const ann1 = makeAnnotation({
			id: 1,
			patternId: 1,
			startTime: 0,
			endTime: 4, // bars 1-2
			args: { color: { r: 255, g: 0, b: 0 } },
		});
		const ann2 = makeAnnotation({
			id: 2,
			patternId: 1,
			startTime: 4,
			endTime: 8, // bars 3-4
			args: { color: { r: 0, g: 0, b: 255 } },
		});
		const ann3 = makeAnnotation({
			id: 3,
			patternId: 1,
			startTime: 12,
			endTime: 16, // bars 7-8
			args: { color: { r: 0, g: 0, b: 255 } }, // same as ann2
		});

		const result = annotationsToDsl(
			[ann1, ann2, ann3],
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toContain("@1-2\nsolid_color(all) color=#ff0000");
		expect(result).toContain("@3-4\nsolid_color(all) color=#0000ff");
		expect(result).toContain("@7-8\nhold");
	});

	it("merges consecutive identical bars into a range", () => {
		const beatGrid = makeBeatGrid(8);
		// Two contiguous annotations with same config → merged into one range
		const ann1 = makeAnnotation({
			id: 1,
			patternId: 1,
			startTime: 0,
			endTime: 4, // bars 1-2
			args: { color: { r: 255, g: 0, b: 0 } },
		});
		const ann2 = makeAnnotation({
			id: 2,
			patternId: 1,
			startTime: 4,
			endTime: 8, // bars 3-4
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl(
			[ann1, ann2],
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toBe("@1-4\nsolid_color(all) color=#ff0000");
	});

	it("orders layers by zIndex", () => {
		const beatGrid = makeBeatGrid(4);
		const ann1 = makeAnnotation({
			id: 1,
			patternId: 1,
			startTime: 0,
			endTime: 2,
			zIndex: 1,
			args: { color: { r: 255, g: 0, b: 0 } },
		});
		const ann2 = makeAnnotation({
			id: 2,
			patternId: 2,
			startTime: 0,
			endTime: 2,
			zIndex: 0,
			blendMode: "add",
			args: {
				subdivision: 2,
				color: { r: 255, g: 255, b: 255 },
				selection: { expression: "hit", spatialReference: "global" },
			},
		});

		const result = annotationsToDsl(
			[ann1, ann2],
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		const lines = result.split("\n");
		// zIndex 0 first (intensity_spikes), then zIndex 1 (solid_color)
		expect(lines[1]).toContain("intensity_spikes");
		expect(lines[2]).toContain("solid_color");
	});

	it("includes blend mode when non-default", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: 1,
			startTime: 0,
			endTime: 2,
			blendMode: "add",
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toContain("blend=add");
	});

	it("skips bars with no annotations", () => {
		const beatGrid = makeBeatGrid(8);
		// Only bars 3-4 have an annotation
		const ann = makeAnnotation({
			patternId: 1,
			startTime: 4,
			endTime: 8,
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("@3-4\nsolid_color(all) color=#ff0000");
		expect(result).not.toContain("@1");
	});

	it("handles multiple non-overlapping annotations in different bar ranges", () => {
		const beatGrid = makeBeatGrid(8);
		const ann1 = makeAnnotation({
			id: 1,
			patternId: 1,
			startTime: 0,
			endTime: 4,
			args: { color: { r: 0, g: 0, b: 68 } },
		});
		const ann2 = makeAnnotation({
			id: 2,
			patternId: 1,
			startTime: 8,
			endTime: 12,
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl(
			[ann1, ann2],
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toContain("@1-2\nsolid_color(all) color=#000044");
		expect(result).toContain("@5-6\nsolid_color(all) color=#ff0000");
	});
});

describe("parseTagExprString", () => {
	it("parses simple tag", () => {
		expect(parseTagExprString("all")).toEqual({
			type: "tag",
			name: "all",
		});
	});

	it("parses AND expression", () => {
		expect(parseTagExprString("left & wash")).toEqual({
			type: "and",
			left: { type: "tag", name: "left" },
			right: { type: "tag", name: "wash" },
		});
	});

	it("parses OR expression", () => {
		expect(parseTagExprString("hit | accent")).toEqual({
			type: "or",
			left: { type: "tag", name: "hit" },
			right: { type: "tag", name: "accent" },
		});
	});

	it("parses NOT expression", () => {
		expect(parseTagExprString("~wash")).toEqual({
			type: "not",
			operand: { type: "tag", name: "wash" },
		});
	});

	it("parses grouped expression", () => {
		const result = parseTagExprString("(left | right) & wash");
		expect(result).toEqual({
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

	it("parses fallback expression", () => {
		expect(parseTagExprString("wash > all")).toEqual({
			type: "fallback",
			left: { type: "tag", name: "wash" },
			right: { type: "tag", name: "all" },
		});
	});
});

// ── Import (DSL → annotations) ─────────────────────────────────

function parseDsl(source: string) {
	const registry = buildRegistry(PATTERNS, PATTERN_ARGS);
	const result = parse(source, registry);
	if (!result.ok) throw new Error(`Parse failed: ${result.errors[0].message}`);
	return result.document;
}

describe("dslToAnnotations", () => {
	it("returns empty array for empty document", () => {
		const doc = { bars: [] };
		const result = dslToAnnotations(
			doc,
			makeBeatGrid(4),
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toEqual([]);
	});

	it("converts a single bar block with one layer", () => {
		const doc = parseDsl("@1\nsolid_color(all) color=#ff0000");
		const beatGrid = makeBeatGrid(4); // bar = 2s at 120bpm
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		expect(result[0].patternId).toBe(1);
		expect(result[0].startTime).toBe(0);
		expect(result[0].endTime).toBe(2);
		expect(result[0].blendMode).toBe("replace");
		expect(result[0].args.color).toEqual({ r: 255, g: 0, b: 0 });
	});

	it("converts a multi-bar range", () => {
		const doc = parseDsl("@1-4\nsolid_color(all) color=#000044");
		const beatGrid = makeBeatGrid(8);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		expect(result[0].startTime).toBe(0);
		expect(result[0].endTime).toBe(8); // 4 bars * 2s
	});

	it("fills default args when not specified in DSL", () => {
		const doc = parseDsl("@1\nsolid_color(all)");
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		// Default for solid_color color is {r:255,g:255,b:255}
		expect(result[0].args.color).toEqual({ r: 255, g: 255, b: 255 });
	});

	it("resolves hold blocks by replaying previous layers", () => {
		const doc = parseDsl("@1-2\nsolid_color(all) color=#ff0000\n\n@3-4\nhold");
		const beatGrid = makeBeatGrid(8);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(2);
		// First block
		expect(result[0].startTime).toBe(0);
		expect(result[0].endTime).toBe(4);
		expect(result[0].args.color).toEqual({ r: 255, g: 0, b: 0 });
		// Hold block — same pattern, different time range
		expect(result[1].patternId).toBe(1);
		expect(result[1].startTime).toBe(4);
		expect(result[1].endTime).toBe(8);
		expect(result[1].args.color).toEqual({ r: 255, g: 0, b: 0 });
	});

	it("assigns incrementing z-indices across all layers", () => {
		const doc = parseDsl(
			"@1\nintensity_spikes(hit) subdivision=2\nsolid_color(all) color=#ff0000",
		);
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(2);
		expect(result[0].zIndex).toBe(0);
		expect(result[1].zIndex).toBe(1);
	});

	it("preserves blend mode", () => {
		const doc = parseDsl("@1\nsolid_color(all) color=#ff0000 blend=add");
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result[0].blendMode).toBe("add");
	});

	it("converts selection expression to annotation arg", () => {
		const doc = parseDsl("@1\nintensity_spikes(left & wash) subdivision=2");
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result[0].args.selection).toEqual({
			expression: "left & wash",
			spatialReference: "global",
		});
	});

	it("round-trips export → import producing equivalent annotations", () => {
		const beatGrid = makeBeatGrid(8);
		const original = [
			makeAnnotation({
				id: 1,
				patternId: 1,
				startTime: 0,
				endTime: 4,
				zIndex: 0,
				args: { color: { r: 255, g: 0, b: 0 } },
			}),
			makeAnnotation({
				id: 2,
				patternId: 2,
				startTime: 4,
				endTime: 8,
				zIndex: 1,
				blendMode: "add",
				args: {
					subdivision: 2,
					color: { r: 0, g: 255, b: 0 },
					selection: { expression: "hit", spatialReference: "global" },
				},
			}),
		];

		const dslText = annotationsToDsl(
			original,
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		const doc = parseDsl(dslText);
		const imported = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(imported).toHaveLength(2);

		// First annotation
		expect(imported[0].patternId).toBe(1);
		expect(imported[0].startTime).toBe(0);
		expect(imported[0].endTime).toBe(4);
		expect(imported[0].args.color).toEqual({ r: 255, g: 0, b: 0 });

		// Second annotation
		expect(imported[1].patternId).toBe(2);
		expect(imported[1].startTime).toBe(4);
		expect(imported[1].endTime).toBe(8);
		expect(imported[1].blendMode).toBe("add");
		expect(imported[1].args.subdivision).toBe(2);
		expect(imported[1].args.color).toEqual({ r: 0, g: 255, b: 0 });
		expect(imported[1].args.selection).toEqual({
			expression: "hit",
			spatialReference: "global",
		});
	});
});

describe("hexToRgb", () => {
	it("converts hex to RGB", () => {
		expect(hexToRgb("#ff0000")).toEqual({ r: 255, g: 0, b: 0 });
		expect(hexToRgb("#00ff00")).toEqual({ r: 0, g: 255, b: 0 });
		expect(hexToRgb("#000044")).toEqual({ r: 0, g: 0, b: 68 });
	});
});
