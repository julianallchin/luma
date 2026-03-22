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
	parseGroupExprString,
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
		patternId: string;
		startTime: number;
		endTime: number;
	},
): TimelineAnnotation {
	return {
		id: "test-1",
		uid: null,
		scoreId: "test-score-1",
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
		id: "pat-1",
		uid: null,
		name: "solid_color",
		description: null,
		categoryId: null,
		categoryName: null,
		createdAt: "",
		updatedAt: "",
		isPublished: false,
		authorName: null,
		forkedFromId: null,
	},
	{
		id: "pat-2",
		uid: null,
		name: "intensity_spikes",
		description: null,
		categoryId: null,
		categoryName: null,
		createdAt: "",
		updatedAt: "",
		isPublished: false,
		authorName: null,
		forkedFromId: null,
	},
];

const PATTERN_ARGS: Record<string, PatternArgDef[]> = {
	"pat-1": [
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
	"pat-2": [
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
			patternId: "pat-1",
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
			patternId: "pat-1",
			startTime: 0,
			endTime: 2, // exactly bar 1
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("solid_color(all) @1 color=#ff0000");
	});

	it("converts an annotation spanning multiple bars", () => {
		const beatGrid = makeBeatGrid(8);
		const ann = makeAnnotation({
			patternId: "pat-1",
			startTime: 0,
			endTime: 8, // bars 1-4
			args: { color: { r: 0, g: 0, b: 68 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("solid_color(all) @1-5 color=#000044");
	});

	it("includes default arg values", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: "pat-1",
			startTime: 0,
			endTime: 2,
			args: { color: { r: 255, g: 255, b: 255 } }, // default
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("solid_color(all) @1 color=#ffffff");
	});

	it("uses selection expression from args", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: "pat-2",
			startTime: 0,
			endTime: 2,
			args: {
				subdivision: 2,
				color: { r: 255, g: 255, b: 255 },
				selection: { expression: "hit", spatialReference: "global" },
			},
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toBe("intensity_spikes(hit) @1 subdivision=2 color=#ffffff");
	});

	it("uses complex selection expressions", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: "pat-2",
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

	it("groups annotations by z-index into layers", () => {
		const beatGrid = makeBeatGrid(4);
		const ann1 = makeAnnotation({
			id: "1",
			patternId: "pat-1",
			startTime: 0,
			endTime: 2,
			zIndex: 0,
			args: { color: { r: 0, g: 0, b: 68 } },
		});
		const ann2 = makeAnnotation({
			id: "2",
			patternId: "pat-2",
			startTime: 0,
			endTime: 2,
			zIndex: 1,
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
		// Layer 0, blank line, Layer 1
		expect(lines).toHaveLength(3);
		expect(lines[0]).toContain("solid_color");
		expect(lines[1]).toBe("");
		expect(lines[2]).toContain("intensity_spikes");
	});

	it("includes blend mode when non-default", () => {
		const beatGrid = makeBeatGrid(4);
		const ann = makeAnnotation({
			patternId: "pat-1",
			startTime: 0,
			endTime: 2,
			blendMode: "add",
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toContain("blend=add");
	});

	it("handles multiple non-overlapping annotations in the same layer", () => {
		const beatGrid = makeBeatGrid(8);
		const ann1 = makeAnnotation({
			id: "1",
			patternId: "pat-1",
			startTime: 0,
			endTime: 4,
			args: { color: { r: 0, g: 0, b: 68 } },
		});
		const ann2 = makeAnnotation({
			id: "2",
			patternId: "pat-1",
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
		// Both in same layer (zIndex 0), time ordered
		expect(result).toContain("@1-3 color=#000044");
		expect(result).toContain("@5-7 color=#ff0000");
		expect(result.split("\n").filter((l) => l === "")).toHaveLength(0); // no blank line separating same layer
	});

	it("exports sub-bar precision using bar:beat notation", () => {
		const beatGrid = makeBeatGrid(8); // each bar = 2s at 120bpm
		// Annotation starting at beat 3 of bar 1 (= 1s into bar 1 = 50% through bar)
		const ann = makeAnnotation({
			patternId: "pat-1",
			startTime: 1, // halfway through bar 1
			endTime: 4, // end of bar 2
			args: { color: { r: 255, g: 0, b: 0 } },
		});

		const result = annotationsToDsl([ann], beatGrid, PATTERNS, PATTERN_ARGS);
		expect(result).toContain("@1:3-3");
	});
});

describe("parseGroupExprString", () => {
	it("parses simple group", () => {
		expect(parseGroupExprString("all")).toEqual({
			type: "group",
			name: "all",
		});
	});

	it("parses AND expression", () => {
		expect(parseGroupExprString("left & wash")).toEqual({
			type: "and",
			left: { type: "group", name: "left" },
			right: { type: "group", name: "wash" },
		});
	});

	it("parses OR expression", () => {
		expect(parseGroupExprString("hit | accent")).toEqual({
			type: "or",
			left: { type: "group", name: "hit" },
			right: { type: "group", name: "accent" },
		});
	});

	it("parses NOT expression", () => {
		expect(parseGroupExprString("~wash")).toEqual({
			type: "not",
			operand: { type: "group", name: "wash" },
		});
	});

	it("parses grouped expression", () => {
		const result = parseGroupExprString("(left | right) & wash");
		expect(result).toEqual({
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

	it("parses fallback expression", () => {
		expect(parseGroupExprString("wash > all")).toEqual({
			type: "fallback",
			left: { type: "group", name: "wash" },
			right: { type: "group", name: "all" },
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
		const doc = { layers: [] };
		const result = dslToAnnotations(
			doc,
			makeBeatGrid(4),
			PATTERNS,
			PATTERN_ARGS,
		);
		expect(result).toEqual([]);
	});

	it("converts a single annotation", () => {
		const doc = parseDsl("solid_color(all) @1 color=#ff0000");
		const beatGrid = makeBeatGrid(4); // bar = 2s at 120bpm
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		expect(result[0].patternId).toBe(1);
		expect(result[0].startTime).toBeCloseTo(0, 5);
		expect(result[0].endTime).toBeCloseTo(2, 5);
		expect(result[0].blendMode).toBe("replace");
		expect(result[0].args.color).toEqual({ r: 255, g: 0, b: 0, a: 1 });
	});

	it("converts a multi-bar range", () => {
		const doc = parseDsl("solid_color(all) @1-5 color=#000044");
		const beatGrid = makeBeatGrid(8);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		expect(result[0].startTime).toBeCloseTo(0, 5);
		expect(result[0].endTime).toBeCloseTo(8, 5); // 4 bars * 2s
	});

	it("does not fill default args when not specified in DSL", () => {
		const doc = parseDsl("solid_color(all) @1");
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		// Only explicitly-present args are imported; defaults are left to the engine
		expect(result[0].args.color).toBeUndefined();
	});

	it("assigns z-indices from layer groups", () => {
		const doc = parseDsl(
			"solid_color(all) @1 color=#ff0000\n\nintensity_spikes(hit) @1 subdivision=2",
		);
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(2);
		expect(result[0].zIndex).toBe(0);
		expect(result[1].zIndex).toBe(1);
	});

	it("preserves blend mode", () => {
		const doc = parseDsl("solid_color(all) @1 color=#ff0000 blend=add");
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result[0].blendMode).toBe("add");
	});

	it("converts selection expression to annotation arg", () => {
		const doc = parseDsl("intensity_spikes(left & wash) @1 subdivision=2");
		const beatGrid = makeBeatGrid(4);
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result[0].args.selection).toEqual({
			expression: "left & wash",
			spatialReference: "global",
		});
	});

	it("handles bar:beat ranges", () => {
		const doc = parseDsl("solid_color(all) @1:3-3 color=#ff0000");
		const beatGrid = makeBeatGrid(4); // each bar = 2s
		const result = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(result).toHaveLength(1);
		expect(result[0].startTime).toBeCloseTo(1, 5); // bar 1 beat 3 = halfway through bar 1 = 1s
		expect(result[0].endTime).toBeCloseTo(4, 5); // start of bar 3 = 4s
	});

	it("round-trips export → import producing equivalent annotations", () => {
		const beatGrid = makeBeatGrid(8);
		const original = [
			makeAnnotation({
				id: "1",
				patternId: "pat-1",
				startTime: 0,
				endTime: 4,
				zIndex: 0,
				args: { color: { r: 255, g: 0, b: 0 } },
			}),
			makeAnnotation({
				id: "2",
				patternId: "pat-2",
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
		expect(imported[0].startTime).toBeCloseTo(0, 5);
		expect(imported[0].endTime).toBeCloseTo(4, 5);
		expect(imported[0].zIndex).toBe(0);
		expect(imported[0].args.color).toEqual({ r: 255, g: 0, b: 0, a: 1 });

		// Second annotation
		expect(imported[1].patternId).toBe(2);
		expect(imported[1].startTime).toBeCloseTo(4, 5);
		expect(imported[1].endTime).toBeCloseTo(8, 5);
		expect(imported[1].zIndex).toBe(1);
		expect(imported[1].blendMode).toBe("add");
		expect(imported[1].args.subdivision).toBe(2);
		expect(imported[1].args.color).toEqual({ r: 0, g: 255, b: 0, a: 1 });
		expect(imported[1].args.selection).toEqual({
			expression: "hit",
			spatialReference: "global",
		});
	});

	it("round-trips sub-bar precision with bar:beat notation", () => {
		const beatGrid = makeBeatGrid(8); // each bar = 2s at 120bpm

		// Annotation starting at beat 3 of bar 5 (halfway through bar 5)
		const original = [
			makeAnnotation({
				id: "1",
				patternId: "pat-1",
				startTime: 9, // bar 5 starts at 8s, 9s = halfway through
				endTime: 14, // bar 7 starts at 12s, 14s = end of bar 7
				zIndex: 0,
				args: { color: { r: 255, g: 0, b: 0 } },
			}),
		];

		const dslText = annotationsToDsl(
			original,
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		// Should have bar:beat notation
		expect(dslText).toContain("@5:3-8");

		const doc = parseDsl(dslText);
		const imported = dslToAnnotations(doc, beatGrid, PATTERNS, PATTERN_ARGS);

		expect(imported).toHaveLength(1);
		expect(imported[0].startTime).toBeCloseTo(9, 1);
		expect(imported[0].endTime).toBeCloseTo(14, 1);
	});

	it("string round-trip: serialize → parse → serialize is stable", () => {
		const beatGrid = makeBeatGrid(8);
		const annotations = [
			makeAnnotation({
				id: "1",
				patternId: "pat-1",
				startTime: 0,
				endTime: 8,
				zIndex: 0,
				args: { color: { r: 0, g: 0, b: 68 } },
			}),
			makeAnnotation({
				id: "2",
				patternId: "pat-2",
				startTime: 4,
				endTime: 8,
				zIndex: 1,
				blendMode: "add",
				args: {
					subdivision: 2,
					color: { r: 255, g: 255, b: 255 },
					selection: { expression: "hit", spatialReference: "global" },
				},
			}),
		];

		const dsl1 = annotationsToDsl(
			annotations,
			beatGrid,
			PATTERNS,
			PATTERN_ARGS,
		);
		const doc1 = parseDsl(dsl1);
		const imported = dslToAnnotations(doc1, beatGrid, PATTERNS, PATTERN_ARGS);

		// Re-export from imported annotations
		const reimported = imported.map((a, i) =>
			makeAnnotation({
				id: String(i + 1),
				patternId: a.patternId,
				startTime: a.startTime,
				endTime: a.endTime,
				zIndex: a.zIndex,
				blendMode: a.blendMode as BlendMode,
				args: a.args as Record<string, unknown>,
			}),
		);

		const dsl2 = annotationsToDsl(reimported, beatGrid, PATTERNS, PATTERN_ARGS);
		expect(dsl2).toBe(dsl1);
	});
});

describe("hexToRgb", () => {
	it("converts hex to RGB", () => {
		expect(hexToRgb("#ff0000")).toEqual({ r: 255, g: 0, b: 0 });
		expect(hexToRgb("#00ff00")).toEqual({ r: 0, g: 255, b: 0 });
		expect(hexToRgb("#000044")).toEqual({ r: 0, g: 0, b: 68 });
	});
});
