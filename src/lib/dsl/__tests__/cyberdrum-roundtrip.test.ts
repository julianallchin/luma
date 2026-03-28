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
		// Debug: print first 30 lines of DSL
		console.log("DSL output (first 30 lines):");
		for (const [i, l] of dsl.split("\n").slice(0, 30).entries())
			console.log(`  ${i + 1}: ${l}`);
		console.log(`  ... total ${dsl.split("\n").length} lines`);
		// Debug: first annotation info
		const first = fixture.annotations[0];
		console.log(
			`First annotation: time=${first.startTime}-${first.endTime} pattern=${first.patternName} z=${first.zIndex}`,
		);
		console.log(`First downbeat: ${beatGrid.downbeats[0]}`);
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

		// Normalize z-indices: original may use -1, 0, 2, 3, 4 etc.
		// DSL normalizes to 0, 1, 2, 3, 4
		const origZValues = [...new Set(timeline.map((a) => a.zIndex))].sort(
			(a, b) => a - b,
		);
		const zMap = new Map(origZValues.map((z, i) => [z, i]));

		// Group by normalized layer for proper comparison
		const origByLayer = new Map<number, typeof timeline>();
		for (const a of timeline) {
			const norm = zMap.get(a.zIndex) as number;
			if (!origByLayer.has(norm)) origByLayer.set(norm, []);
			origByLayer.get(norm)?.push(a);
		}
		const reimByLayer = new Map<number, typeof reimported>();
		for (const a of reimported) {
			if (!reimByLayer.has(a.zIndex)) reimByLayer.set(a.zIndex, []);
			reimByLayer.get(a.zIndex)?.push(a);
		}

		// Should have same total count
		expect(reimported.length).toBe(timeline.length);

		const patternNameMap = new Map(patterns.map((p) => [p.id, p.name]));
		const diffs: string[] = [];

		for (const [layer, origAnns] of origByLayer) {
			const reimAnns = reimByLayer.get(layer) ?? [];
			origAnns.sort((a, b) => a.startTime - b.startTime);
			reimAnns.sort((a, b) => a.startTime - b.startTime);

			if (origAnns.length !== reimAnns.length) {
				diffs.push(
					`Layer ${layer}: count mismatch ${origAnns.length} vs ${reimAnns.length}`,
				);
				continue;
			}

			for (let i = 0; i < origAnns.length; i++) {
				const orig = origAnns[i];
				const reim = reimAnns[i];
				if (!orig || !reim) continue;

				const name =
					patternNameMap.get(orig.patternId) ?? String(orig.patternId);
				const label = `L${layer}[${i}] ${name}`;

				// Pattern ID
				if (orig.patternId !== reim.patternId) {
					diffs.push(
						`${label}: patternId ${orig.patternId} → ${reim.patternId}`,
					);
				}

				// Times (with tolerance for bar quantization)
				const TIME_TOL = 0.02; // 20ms
				if (Math.abs(orig.startTime - reim.startTime) > TIME_TOL) {
					diffs.push(
						`${label}: startTime ${orig.startTime.toFixed(3)} → ${reim.startTime.toFixed(3)} (Δ${(reim.startTime - orig.startTime).toFixed(3)}s)`,
					);
				}
				if (Math.abs(orig.endTime - reim.endTime) > TIME_TOL) {
					diffs.push(
						`${label}: endTime ${orig.endTime.toFixed(3)} → ${reim.endTime.toFixed(3)} (Δ${(reim.endTime - orig.endTime).toFixed(3)}s)`,
					);
				}

				// Blend mode
				if (orig.blendMode !== reim.blendMode) {
					diffs.push(`${label}: blend ${orig.blendMode} → ${reim.blendMode}`);
				}

				// Args comparison - compare the args that exist in the pattern's arg defs
				// (orphaned args from old pattern versions are expected to be lost)
				const argDefs = patternArgs[orig.patternId] ?? [];
				const origArgs = (orig.args ?? {}) as Record<string, unknown>;
				const reimArgs = reim.args;

				for (const def of argDefs) {
					if (def.argType === "Selection") {
						const origSel = origArgs[def.id] as
							| { expression?: string }
							| undefined;
						const reimSel = reimArgs[def.id] as
							| { expression?: string }
							| undefined;
						const origExpr = origSel?.expression ?? "all";
						const reimExpr = reimSel?.expression ?? "all";
						if (origExpr !== reimExpr) {
							diffs.push(`${label}: selection "${origExpr}" → "${reimExpr}"`);
						}
						continue;
					}

					const ov = origArgs[def.id];
					const rv = reimArgs[def.id];

					if (def.argType === "Color") {
						const origColor = normalizeColor(ov);
						const reimColor = normalizeColor(rv);
						if (origColor && reimColor) {
							if (
								Math.abs(origColor.r - reimColor.r) > 1 ||
								Math.abs(origColor.g - reimColor.g) > 1 ||
								Math.abs(origColor.b - reimColor.b) > 1 ||
								Math.abs(origColor.a - reimColor.a) > 0.01
							) {
								diffs.push(
									`${label}: arg "${def.name}" color(${origColor.r},${origColor.g},${origColor.b},${origColor.a}) → (${reimColor.r},${reimColor.g},${reimColor.b},${reimColor.a})`,
								);
							}
						} else if (origColor !== reimColor) {
							diffs.push(
								`${label}: arg "${def.name}" ${JSON.stringify(ov)} → ${JSON.stringify(rv)}`,
							);
						}
						continue;
					}

					// Scalar
					if (typeof ov === "number" && typeof rv === "number") {
						if (Math.abs(ov - rv) > 0.001) {
							diffs.push(`${label}: arg "${def.name}" ${ov} → ${rv}`);
						}
					} else if (ov != null && rv == null) {
						diffs.push(
							`${label}: arg "${def.name}" ${JSON.stringify(ov)} → undefined`,
						);
					} else if (ov == null && rv != null) {
						// Reimport filled a default — this is expected behavior
					}
				}
			}
		}

		if (diffs.length > 0) {
			console.error(
				`${diffs.length} roundtrip differences:\n${diffs.join("\n")}`,
			);
		}
		expect(diffs).toEqual([]);
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
			dslToAnnotations(result1.document, beatGrid, patterns, patternArgs).map(
				(a, i) => ({
					id: i + 1,
					patternId: a.patternId,
					patternName: patterns.find((p) => p.id === a.patternId)?.name ?? "",
					startTime: a.startTime,
					endTime: a.endTime,
					zIndex: a.zIndex,
					blendMode: a.blendMode,
					args: a.args,
				}),
			),
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

function normalizeColor(
	v: unknown,
): { r: number; g: number; b: number; a: number } | null {
	if (v == null) return null;
	if (typeof v === "object" && v !== null && "r" in v) {
		const obj = v as { r: number; g: number; b: number; a?: number };
		return { r: obj.r, g: obj.g, b: obj.b, a: obj.a ?? 1 };
	}
	return null;
}
