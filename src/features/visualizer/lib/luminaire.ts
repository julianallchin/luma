import type { FixtureDefinition } from "@/bindings/fixtures";
import type { FixtureModelKind } from "../components/fixture-models";

// ---------------------------------------------------------------------------
// One continuous luminaire model, one source of cone geometry.
//
// A fixture type is just an opening angle (and a lumen budget) on a single zoom
// axis — beam -> spot -> par -> wash is one smooth sweep of `fieldAngleDeg`
// with no per-type special cases. Concentration (lumens per solid angle)
// derives brightness, throw, edge hardness, and scatter anisotropy.
//
// The angle comes from the fixture definition's `Physical.Lens` (parsed from
// the QLC+ `.qxf`). The per-kind table below is a *fallback only*, for
// definitions that omit lens data — it is the only such table in the codebase
// and no consumer may keep its own. A live zoom channel plugs in by animating
// `fieldAngleDeg` before calling `coneFromOpening`.
// ---------------------------------------------------------------------------

export interface Luminaire {
	/** Full field angle (deg) — the fixture's opening. */
	fieldAngleDeg: number;
	/** Relative lumen output; 1 = a stock moving-head lamp. */
	lumens: number;
}

/**
 * Used only when `Physical.Lens` is missing or blank. Angles are the median
 * lens angle of the bundled QLC+ definitions of that kind, so a definition
 * without lens data lands where its peers do.
 */
const FALLBACK_LUMINAIRES: Partial<Record<FixtureModelKind, Luminaire>> = {
	moving_head: { fieldAngleDeg: 18, lumens: 1 },
	scanner: { fieldAngleDeg: 16, lumens: 1 },
	par: { fieldAngleDeg: 25, lumens: 1 },
	strobe: { fieldAngleDeg: 78, lumens: 3 },
};

const DEFAULT_LUMINAIRE: Luminaire = { fieldAngleDeg: 25, lumens: 1 };

/** Per-pixel emitter of a procedural LED bar / matrix — not a lensed fixture. */
export const PIXEL_LUMINAIRE: Luminaire = { fieldAngleDeg: 60, lumens: 0.6 };

/** Openings outside this range are not physical and break the cone math. */
export function clampFieldAngle(deg: number): number {
	return Math.min(160, Math.max(4, deg));
}

/**
 * Opening angle from the definition's lens, or null when it has none.
 *
 * QLC+ writes `DegreesMin="0" DegreesMax="0"` for "unknown" (a third of the
 * bundled library does), so zero means absent, not a zero-degree beam. A fixed
 * lens repeats one value; a zoom lens gives a range, and with no zoom channel
 * in the state model we sit at mid-zoom.
 */
function lensFieldAngle(
	definition: FixtureDefinition | undefined,
): number | null {
	const lens = definition?.Physical?.Lens;
	if (!lens) return null;
	const min = lens["@DegreesMin"] ?? 0;
	const max = lens["@DegreesMax"] ?? 0;
	const lo = min > 0 ? min : max;
	const hi = max > 0 ? max : min;
	if (lo <= 0) return null;
	return (lo + hi) / 2;
}

/** The one answer to "how wide is this fixture's cone". */
export function luminaireFor(
	definition: FixtureDefinition | undefined,
	kind: FixtureModelKind | null,
): Luminaire {
	const fallback = (kind && FALLBACK_LUMINAIRES[kind]) || DEFAULT_LUMINAIRE;
	const lens = lensFieldAngle(definition);
	if (lens === null) return fallback;
	return { fieldAngleDeg: clampFieldAngle(lens), lumens: fallback.lumens };
}

export interface ConeParams {
	cosBeam: number;
	cosField: number;
	range: number;
	wash: number;
	/** Intensity multiplier: lumens x solid-angle concentration. */
	gain: number;
}

function coneSolidAngle(fullAngleDeg: number): number {
	return 2 * Math.PI * (1 - Math.cos(((fullAngleDeg / 2) * Math.PI) / 180));
}

function smoothstep01(edge0: number, edge1: number, x: number): number {
	const t = Math.min(1, Math.max(0, (x - edge0) / (edge1 - edge0)));
	return t * t * (3 - 2 * t);
}

/** Concentration reference: a 30 degree spot has gain 1.5 and 12m of throw. */
const REF_SOLID_ANGLE = coneSolidAngle(30);

export function coneFromOpening(luminaire: Luminaire): ConeParams {
	const fieldDeg = clampFieldAngle(luminaire.fieldAngleDeg);
	// Same energy through a smaller solid angle = hotter, whiter, longer throw.
	const concentration = REF_SOLID_ANGLE / coneSolidAngle(fieldDeg);
	// Wide openings scatter near-isotropically and develop a soft shoulder;
	// the beam:field ratio (the 50%-intensity contour of the peaked profile)
	// narrows continuously as the cone opens.
	const wash = smoothstep01(20, 80, fieldDeg);
	const beamRatio = 0.6 - 0.25 * wash;
	const halfRad = ((fieldDeg / 2) * Math.PI) / 180;
	return {
		cosField: Math.cos(halfRad),
		cosBeam: Math.cos(halfRad * beamRatio),
		range: Math.min(18, Math.max(3, 12 * Math.sqrt(concentration))),
		wash,
		gain: 1.5 * luminaire.lumens * Math.min(6, Math.max(0.1, concentration)),
	};
}
