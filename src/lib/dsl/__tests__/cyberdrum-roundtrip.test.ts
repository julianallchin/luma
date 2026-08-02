import { describe, expect, it } from "vitest";
import type {
	BeatGrid,
	BlendMode,
	PatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import type { AnnotationInput } from "../convert";
import { annotationsToDsl, buildRegistry, dslToAnnotations } from "../convert";
import { parse } from "../parser";
import fixture from "./cyberdrum-fixture.json";

// Load the real cyberdrum data from the fixture
const beatGrid: BeatGrid = fixture.beatGrid as BeatGrid;
// Fixture has numeric IDs; coerce to string to match the new schema
const patterns: PatternSummary[] = (
	fixture.patterns as unknown as Array<Record<string, unknown>>
).map((p) => ({
	...p,
	id: String(p.id),
	categoryName: p.categoryName ?? null,
	forkedFromId: p.forkedFromId != null ? String(p.forkedFromId) : null,
})) as unknown as PatternSummary[];
const patternArgs: Record<string, PatternArgDef[]> = Object.fromEntries(
	Object.entries(fixture.patternArgs).map(([k, v]) => [
		k,
		v as PatternArgDef[],
	]),
);

type FixtureAnnotation = (typeof fixture.annotations)[number];

// Convert fixture annotations to the AnnotationInput shape that annotationsToDsl expects
function toTimelineAnnotations(anns: FixtureAnnotation[]): AnnotationInput[] {
	return anns.map((a) => ({
		id: String(a.id),
		patternId: String(a.patternId),
		startTime: a.startTime,
		endTime: a.endTime,
		zIndex: a.zIndex,
		blendMode: a.blendMode as BlendMode,
		args: a.args as Record<string, unknown>,
	}));
}

describe("cyberdrum roundtrip", () => {
	it("has correct fixture data", () => {
		expect(fixture.annotations.length).toBe(109);
		expect(fixture.patterns.length).toBe(10);
		expect(beatGrid.bpm).toBe(80);
		expect(beatGrid.beatsPerBar).toBe(4);
	});

	it("exports to DSL without errors", () => {
		const timeline = toTimelineAnnotations(fixture.annotations);
		const dsl = annotationsToDsl(timeline, beatGrid, patterns, patternArgs);
		expect(dsl.length).toBeGreaterThan(0);
		// Should have multiple lines (one per annotation, blank lines between layers)
		const lines = dsl.split("\n").filter((l) => l.trim().length > 0);
		expect(lines.length).toBeGreaterThan(0);
		expect(dsl).toContain(`"${fixture.annotations[0].id}":`);
		expect(dsl).toContain(`["${fixture.annotations[0].patternId}"]`);
	});

	it("parses exported DSL without errors", () => {
		const timeline = toTimelineAnnotations(fixture.annotations);
		const dsl = annotationsToDsl(timeline, beatGrid, patterns, patternArgs);
		const registry = buildRegistry(patterns, patternArgs);
		const result = parse(dsl, registry, { beatsPerBar: beatGrid.beatsPerBar });
		expect(result.ok).toBe(true);
		if (!result.ok) {
			console.error("Parse errors:", result.errors);
		}
	});

	it("roundtrips all annotations: export → parse → import produces same data", () => {
		const timeline = toTimelineAnnotations(fixture.annotations);
		const dsl = annotationsToDsl(timeline, beatGrid, patterns, patternArgs);
		const registry = buildRegistry(patterns, patternArgs);
		const parseResult = parse(dsl, registry, {
			beatsPerBar: beatGrid.beatsPerBar,
		});
		expect(parseResult.ok).toBe(true);
		if (!parseResult.ok) return;

		const reimported = dslToAnnotations(
			parseResult.document,
			beatGrid,
			patterns,
			patternArgs,
		);

		expect(reimported.length).toBe(timeline.length);
		const reimportedById = new Map(
			reimported.map((annotation) => [annotation.id, annotation]),
		);
		for (const original of timeline) {
			expect(reimportedById.get(original.id)).toEqual(original);
		}
	});

	it("DSL string is stable: serialize → parse → serialize", () => {
		const timeline = toTimelineAnnotations(fixture.annotations);
		const dsl1 = annotationsToDsl(timeline, beatGrid, patterns, patternArgs);
		const registry = buildRegistry(patterns, patternArgs);

		const result1 = parse(dsl1, registry, {
			beatsPerBar: beatGrid.beatsPerBar,
		});
		expect(result1.ok).toBe(true);
		if (!result1.ok) return;

		const dsl2 = annotationsToDsl(
			dslToAnnotations(result1.document, beatGrid, patterns, patternArgs),
			beatGrid,
			patterns,
			patternArgs,
		);

		if (dsl1 !== dsl2) {
			// Show first differing line
			const lines1 = dsl1.split("\n");
			const lines2 = dsl2.split("\n");
			for (let i = 0; i < Math.max(lines1.length, lines2.length); i++) {
				if (lines1[i] !== lines2[i]) {
					console.error(`First diff at line ${i + 1}:`);
					console.error(`  original:  ${lines1[i]}`);
					console.error(`  roundtrip: ${lines2[i]}`);
					break;
				}
			}
		}
		expect(dsl2).toBe(dsl1);
	});
});
