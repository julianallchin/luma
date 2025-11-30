import hazerGlb from "../../../../resources/meshes/qlc/hazer.glb?url";
import movingHeadGlb from "../../../../resources/meshes/qlc/moving_head.glb?url";
import parGlb from "../../../../resources/meshes/qlc/par.glb?url";
import scannerGlb from "../../../../resources/meshes/qlc/scanner.glb?url";
import smokeGlb from "../../../../resources/meshes/qlc/smoke.glb?url";
import strobeGlb from "../../../../resources/meshes/qlc/strobe.glb?url";
import type { FixtureDefinition } from "../../../bindings/fixtures";

export type FixtureModelKind =
	| "par"
	| "moving_head"
	| "scanner"
	| "strobe"
	| "hazer"
	| "smoke";

export interface FixtureModelInfo {
	kind: FixtureModelKind;
	url: string;
}

const MODEL_BY_TYPE: Record<string, FixtureModelInfo> = {
	"Color Changer": { kind: "par", url: parGlb },
	Dimmer: { kind: "par", url: parGlb },
	"Moving Head": { kind: "moving_head", url: movingHeadGlb },
	Scanner: { kind: "scanner", url: scannerGlb },
	Strobe: { kind: "strobe", url: strobeGlb },
	Hazer: { kind: "hazer", url: hazerGlb },
	Smoke: { kind: "smoke", url: smokeGlb },
};

/** Returns true if the fixture should be rendered procedurally (LED bars / matrices). */
export function isProcedural(definition: FixtureDefinition): boolean {
	const type = definition.Type;
	return type === "LED Bar (Pixels)" || type === "LED Bar (Beams)";
}

/** Matches QLC+ mesh selection for non-LED fixtures. */
export function getModelForFixture(
	definition: FixtureDefinition,
): FixtureModelInfo | null {
	const type = definition.Type;

	// 1. Try exact match
	if (MODEL_BY_TYPE[type]) {
		return MODEL_BY_TYPE[type];
	}

	// 2. Fuzzy match
	const lower = type.toLowerCase();

	if (lower.includes("moving") || lower.includes("head")) {
		return { kind: "moving_head", url: movingHeadGlb };
	}
	if (
		lower.includes("par") ||
		lower.includes("color") ||
		lower.includes("dimmer")
	) {
		return { kind: "par", url: parGlb };
	}
	if (lower.includes("scanner")) {
		return { kind: "scanner", url: scannerGlb };
	}
	if (lower.includes("strobe")) {
		return { kind: "strobe", url: strobeGlb };
	}
	if (lower.includes("hazer")) {
		return { kind: "hazer", url: hazerGlb };
	}
	if (lower.includes("smoke") || lower.includes("fog")) {
		return { kind: "smoke", url: smokeGlb };
	}

	return null;
}
