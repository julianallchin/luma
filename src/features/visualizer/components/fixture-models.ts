import type { FixtureDefinition } from "../../../bindings/fixtures";
import hazerGlb from "../../../../resources/meshes/qlc/hazer.glb?url";
import movingHeadGlb from "../../../../resources/meshes/qlc/moving_head.glb?url";
import parGlb from "../../../../resources/meshes/qlc/par.glb?url";
import scannerGlb from "../../../../resources/meshes/qlc/scanner.glb?url";
import smokeGlb from "../../../../resources/meshes/qlc/smoke.glb?url";
import strobeGlb from "../../../../resources/meshes/qlc/strobe.glb?url";

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
	return MODEL_BY_TYPE[type] ?? null;
}
