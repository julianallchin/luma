import { describe, expect, it } from "vitest";
import type { FixtureDefinition, Lens } from "../../../../bindings/fixtures";
import { coneFromOpening, luminaireFor } from "../luminaire";

function def(lens: Lens | null): FixtureDefinition {
	return {
		Manufacturer: "Test",
		Model: "Test",
		Type: "Moving Head",
		Channel: [],
		Mode: [],
		Physical: lens
			? {
					Dimensions: null,
					Layout: null,
					Bulb: null,
					Lens: lens,
					Focus: null,
					Technical: null,
				}
			: null,
	};
}

describe("luminaireFor", () => {
	it("derives field = 2x the definition's beam angle", () => {
		const l = luminaireFor(
			def({ "@Name": "Other", "@DegreesMin": 14, "@DegreesMax": 14 }),
			"moving_head",
		);
		expect(l.fieldAngleDeg).toBe(28);
	});

	it("sits at mid-zoom for a zoom lens", () => {
		const l = luminaireFor(
			def({ "@Name": "Other", "@DegreesMin": 10, "@DegreesMax": 60 }),
			"moving_head",
		);
		expect(l.fieldAngleDeg).toBe(70);
	});

	it("treats QLC+'s 0/0 'unknown' lens as absent", () => {
		const l = luminaireFor(
			def({ "@Name": "Other", "@DegreesMin": 0, "@DegreesMax": 0 }),
			"par",
		);
		expect(l.fieldAngleDeg).toBe(50);
	});

	it("falls back per kind when the definition has no physical block", () => {
		expect(luminaireFor(def(null), "strobe").fieldAngleDeg).toBe(156);
		expect(luminaireFor(undefined, "scanner").fieldAngleDeg).toBe(32);
	});

	it("keeps the kind's lumen budget even when the lens supplies the angle", () => {
		const l = luminaireFor(
			def({ "@Name": "Other", "@DegreesMin": 90, "@DegreesMax": 90 }),
			"strobe",
		);
		expect(l).toEqual({ fieldAngleDeg: 160, lumens: 3 });
	});

	it("clamps physically impossible openings", () => {
		expect(
			luminaireFor(
				def({ "@Name": null, "@DegreesMin": 1, "@DegreesMax": 1 }),
				"moving_head",
			).fieldAngleDeg,
		).toBe(4);
		expect(
			luminaireFor(
				def({ "@Name": null, "@DegreesMin": 300, "@DegreesMax": 300 }),
				"par",
			).fieldAngleDeg,
		).toBe(160);
	});
});

describe("coneFromOpening", () => {
	it("puts the reference 30 degree spot at gain 1.5 and 12m throw", () => {
		const cone = coneFromOpening({ fieldAngleDeg: 30, lumens: 1 });
		expect(cone.gain).toBeCloseTo(1.5, 6);
		expect(cone.range).toBeCloseTo(12, 6);
	});

	it("narrower openings throw further and hit harder", () => {
		const narrow = coneFromOpening({ fieldAngleDeg: 14, lumens: 1 });
		const wide = coneFromOpening({ fieldAngleDeg: 60, lumens: 1 });
		expect(narrow.range).toBeGreaterThan(wide.range);
		expect(narrow.gain).toBeGreaterThan(wide.gain);
		expect(narrow.cosField).toBeGreaterThan(wide.cosField);
		expect(narrow.wash).toBeLessThan(wide.wash);
	});
});
