// biome-ignore-all lint/suspicious/noExportsInTest: the fixture library and
// case builders below are exported so the (throwaway) golden generator can
// import them instead of duplicating the hand-authored fixtures. See the
// "HOW THE GOLDENS WERE PRODUCED" block.

import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import type {
	Capability,
	Channel,
	FixtureDefinition,
	Mode,
} from "../../../../bindings/fixtures";
import {
	type DmxMapping,
	type FixtureState,
	getDmxMapping,
	getHeadState,
} from "../fixture-utils";

/**
 * GOLDEN-VECTOR CHARACTERIZATION TEST — fixture-utils (DMX -> visual state decode).
 *
 * This test REGENERATES NOTHING at runtime. It loads
 * `harness/goldens/fixture-utils.json` and asserts that the current
 * implementation still produces byte-for-byte (modulo float rounding) the
 * recorded outputs. It exists to pin the behaviour of `getDmxMapping` /
 * `getHeadState` ahead of a port to Rust — including the behaviour that is
 * arguably wrong (see "KNOWN QUIRKS" below). If a change here fails, that is a
 * behaviour change, not a flaky test: decide deliberately, then re-cut goldens.
 *
 * HOW THE GOLDENS WERE PRODUCED
 * -----------------------------
 * A throwaway generator imported `FIXTURES` and `buildCaseInputs()` from this
 * file, ran the real module over every case input, rounded the outputs with the
 * same `roundState()` used below, and wrote the JSON. To re-cut them, run a
 * script equivalent to:
 *
 *   // scratch/gen.ts
 *   import { FIXTURES, buildCaseInputs, roundState, applyDmx } from
 *     "./src/features/visualizer/lib/__tests__/fixture-utils.golden.test";
 *   import { getDmxMapping, getHeadState } from
 *     "./src/features/visualizer/lib/fixture-utils";
 *   // for each case input: mapping cases -> getDmxMapping(...);
 *   // state cases -> samples.map(s => roundState(getHeadState(
 *   //   input.mapping, applyDmx(input.universeLength, s.dmx), input.startAddress)))
 *   // then write [{case, input, output}] to harness/goldens/fixture-utils.json
 *
 *   bun run scratch/gen.ts
 *
 * FLOAT TOLERANCE
 * ---------------
 * There is no epsilon-compare. Both the recorded and the actual outputs pass
 * through `roundState()`, which quantises the colour components / intensity to
 * 1e-6 and strobe Hz to 1e-4, then the objects are compared with `toEqual`.
 * That is deliberate: it is tight enough to catch a wrong interpolation
 * constant, loose enough to survive last-bit differences in the double math of
 * a re-implementation.
 *
 * CASE NAMING
 * -----------
 * `mapping/...` cases feed `{fixture, mode, headIndex}` to `getDmxMapping`.
 * `state/...` cases carry a literal `DmxMapping`, a `startAddress`, a universe
 * length, and a list of DMX samples (absolute-address -> byte); each sample is
 * decoded by `getHeadState`. Fixture definitions themselves are NOT in the
 * golden — they are the hand-authored `FIXTURES` library below, referenced by
 * key, so the goldens stay readable.
 *
 * KNOWN QUIRKS PINNED BY THESE GOLDENS (bugs, not fixed here — see notes)
 * ----------------------------------------------------------------------
 *  1. `getHeadState`'s no-capability fallback reads `universeData[mapping.strobe]`
 *     as an ABSOLUTE index rather than `startAddress + mapping.strobe`, and then
 *     only checks `>= 0`, which is true for every byte. The read is dead unless
 *     the index is past the end of the universe, in which case it is `undefined`
 *     and `undefined >= 0` is false — silently disabling the whole strobe branch.
 *     Pinned by `state/no_caps_strobe/*`.
 *  2. The implicit-master-dimmer guard is `!mapping.masterDimmer`, which is true
 *     when the master dimmer was already found at channel index 0. A later
 *     "dimmer"-named channel outside every head therefore steals the master
 *     slot. Pinned by `mapping/quirky_master/*`.
 *  3. A channel whose preset is `IntensityDimmer` in a headless mode is assigned
 *     to BOTH `masterDimmer` and `dimmer`, so intensity is squared. Pinned by
 *     `state/cmy_mover/*`.
 *  4. A DMX value falling in a gap between declared strobe capabilities takes
 *     neither the capability branch nor (because the capability list is
 *     non-empty) the fallback branch: shutter stays open, strobe stays 0.
 *     Pinned by the 240-255 tail of `state/cmy_mover/strobe-sweep`.
 */

const GOLDEN_PATH = path.resolve(
	__dirname,
	"../../../../../harness/goldens/fixture-utils.json",
);

// ---------------------------------------------------------------------------
// Hand-authored fixture library (plain data, deliberately NOT loaded from
// resources/fixtures so the goldens can never drift with the bundled library).
// ---------------------------------------------------------------------------

function cap(
	min: number,
	max: number,
	preset: string,
	label = preset,
): Capability {
	return {
		"@Min": min,
		"@Max": max,
		"@Preset": preset,
		"@Res1": null,
		"@Res2": null,
		"@Res": null,
		"@Color": null,
		"@Color2": null,
		$value: label,
	};
}

function ch(
	name: string,
	preset: string | null,
	group: string | null = null,
	capability: Capability[] = [],
): Channel {
	return {
		"@Name": name,
		"@Preset": preset,
		Group: group === null ? null : { "@Byte": 0, $value: group },
		Capability: capability,
	};
}

function mode(name: string, channels: string[], heads: number[][] = []): Mode {
	return {
		"@Name": name,
		Channel: channels.map((c, i) => ({ "@Number": i, $value: c })),
		Head: heads.map((Channel) => ({ Channel })),
	};
}

function def(
	model: string,
	Channel: Channel[],
	Mode: Mode[],
): FixtureDefinition {
	return {
		Manufacturer: "Golden",
		Model: model,
		Type: "Test",
		Channel,
		Mode,
		Physical: null,
	};
}

/** Six preset families plus a deliberate uncovered 240-255 tail. */
const STROBE_CAPS: Capability[] = [
	cap(0, 7, "ShutterOpen"),
	cap(8, 15, "ShutterClose"),
	cap(16, 63, "StrobeSlowToFast"),
	cap(64, 111, "StrobeFastToSlow"),
	cap(112, 159, "StrobeRandom"),
	cap(160, 207, "StrobePulseSlowFast"),
	cap(208, 231, "StrobeFreqRange"),
	cap(232, 239, "LampOn"),
	// 240-255 intentionally uncovered -> gap branch
];

export const FIXTURES: Record<string, FixtureDefinition> = {
	// (a) plain 3-channel RGB par, no dimmer, no heads
	rgb3: def(
		"RGB Par 3ch",
		[
			ch("Red", "IntensityRed", "Intensity"),
			ch("Green", "IntensityGreen", "Intensity"),
			ch("Blue", "IntensityBlue", "Intensity"),
		],
		[mode("3 Channel", ["Red", "Green", "Blue"])],
	),

	// (b) RGBW par with an explicit master dimmer; second mode drops the dimmer
	// so every channel index shifts (mode-vs-global channel resolution).
	rgbw_master: def(
		"RGBW Par",
		[
			ch("Dimmer", "IntensityMasterDimmer", "Intensity"),
			ch("Red", "IntensityRed", "Intensity"),
			ch("Green", "IntensityGreen", "Intensity"),
			ch("Blue", "IntensityBlue", "Intensity"),
			ch("White", "IntensityWhite", "Intensity"),
		],
		[
			mode("5 Channel", ["Dimmer", "Red", "Green", "Blue", "White"]),
			mode("4 Channel", ["Red", "Green", "Blue", "White"]),
		],
	),

	// (c) RGBA par -> amber's 0.75 green weight / no-blue rule
	rgba: def(
		"RGBA Par",
		[
			ch("Red", "IntensityRed", "Intensity"),
			ch("Green", "IntensityGreen", "Intensity"),
			ch("Blue", "IntensityBlue", "Intensity"),
			ch("Amber", "IntensityAmber", "Intensity"),
		],
		[mode("4 Channel", ["Red", "Green", "Blue", "Amber"])],
	),

	// (d) CMY moving head: subtractive mixing, pan/tilt/zoom, full strobe table
	cmy_mover: def(
		"CMY Mover",
		[
			ch("Pan", "PositionPan", "Pan"),
			ch("Tilt", "PositionTilt", "Tilt"),
			ch("Cyan", "IntensityCyan", "Intensity"),
			ch("Magenta", "IntensityMagenta", "Intensity"),
			ch("Yellow", "IntensityYellow", "Intensity"),
			ch("Dimmer", "IntensityDimmer", "Intensity"),
			ch("Strobe", "ShutterStrobeSlowFast", "Shutter", STROBE_CAPS),
			ch("Zoom", "BeamZoomSmallBig", "Beam"),
		],
		[
			mode("8 Channel", [
				"Pan",
				"Tilt",
				"Cyan",
				"Magenta",
				"Yellow",
				"Dimmer",
				"Strobe",
				"Zoom",
			]),
			// no strobe, no zoom -> indices shift, strobe mapping stays null
			mode("6 Channel", ["Pan", "Tilt", "Cyan", "Magenta", "Yellow", "Dimmer"]),
		],
	),

	// (e) 3-head RGBW bar with explicit Mode.Head arrays and a global strobe
	// channel that carries NO capabilities but the ShutterStrobeSlowFast preset,
	// so the auto-generated 0-9 open / 10-255 strobe table kicks in.
	bar3: def(
		"RGBW Bar 3-head",
		[
			ch("Master Dimmer", "IntensityMasterDimmer", "Intensity"),
			ch("Strobe", "ShutterStrobeSlowFast", "Shutter"),
			ch("Dimmer 1", "IntensityDimmer", "Intensity"),
			ch("Red 1", "IntensityRed", "Intensity"),
			ch("Green 1", "IntensityGreen", "Intensity"),
			ch("Blue 1", "IntensityBlue", "Intensity"),
			ch("Dimmer 2", "IntensityDimmer", "Intensity"),
			ch("Red 2", "IntensityRed", "Intensity"),
			ch("Green 2", "IntensityGreen", "Intensity"),
			ch("Blue 2", "IntensityBlue", "Intensity"),
			ch("Dimmer 3", "IntensityDimmer", "Intensity"),
			ch("Red 3", "IntensityRed", "Intensity"),
			ch("Green 3", "IntensityGreen", "Intensity"),
			ch("Blue 3", "IntensityBlue", "Intensity"),
		],
		[
			mode(
				"14 Channel",
				[
					"Master Dimmer",
					"Strobe",
					"Dimmer 1",
					"Red 1",
					"Green 1",
					"Blue 1",
					"Dimmer 2",
					"Red 2",
					"Green 2",
					"Blue 2",
					"Dimmer 3",
					"Red 3",
					"Green 3",
					"Blue 3",
				],
				[
					[2, 3, 4, 5],
					[6, 7, 8, 9],
					[10, 11, 12, 13],
				],
			),
		],
	),

	// (f) mode whose Channel list names a channel absent from the global list
	missing_channel: def(
		"Broken Mode Par",
		[
			ch("Red", "IntensityRed", "Intensity"),
			ch("Green", "IntensityGreen", "Intensity"),
			ch("Blue", "IntensityBlue", "Intensity"),
		],
		[mode("Broken", ["Red", "Ghost Channel", "Blue"])],
	),

	// Explicit master at index 0 plus a headless "dimmer"-named channel later:
	// pins the `!mapping.masterDimmer` falsy-zero quirk.
	quirky_master: def(
		"Quirky Dimmer Par",
		[
			ch("Master Dimmer", "IntensityMasterDimmer", "Intensity"),
			ch("Red", "IntensityRed", "Intensity"),
			ch("Green", "IntensityGreen", "Intensity"),
			ch("Blue", "IntensityBlue", "Intensity"),
			ch("Fine Dimmer", null, "Intensity"),
		],
		[
			mode("5 Channel", [
				"Master Dimmer",
				"Red",
				"Green",
				"Blue",
				"Fine Dimmer",
			]),
		],
	),

	// Strobe channel with neither capabilities nor the auto-generated preset ->
	// the `getHeadState` fallback branch.
	no_caps_strobe: def(
		"Fallback Strobe Par",
		[
			ch("Red", "IntensityRed", "Intensity"),
			ch("Green", "IntensityGreen", "Intensity"),
			ch("Blue", "IntensityBlue", "Intensity"),
			ch("Shutter", "ShutterStrobeFastSlow", "Shutter"),
		],
		[mode("4 Channel", ["Red", "Green", "Blue", "Shutter"])],
	),
};

// ---------------------------------------------------------------------------
// Case input construction (shared with the generator)
// ---------------------------------------------------------------------------

export interface MappingCaseInput {
	fixture: string;
	mode: string;
	headIndex: number;
}

export interface DmxSample {
	dmx: Record<string, number>;
}

export interface StateCaseInput {
	mapping: DmxMapping;
	startAddress: number;
	universeLength: number;
	samples: DmxSample[];
}

export type CaseInput = MappingCaseInput | StateCaseInput;

export const UNIVERSE_LENGTH = 40;
const LADDER = [0, 1, 9, 10, 127, 128, 254, 255];

/** mulberry32 — fixed seed, so the sampled cases are reproducible. */
function prng(seed: number): () => number {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = a;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

/** Offsets the mapping actually reads, in a stable order. */
function mappedOffsets(mapping: DmxMapping): number[] {
	const keys: (keyof DmxMapping)[] = [
		"red",
		"green",
		"blue",
		"white",
		"amber",
		"cyan",
		"magenta",
		"yellow",
		"dimmer",
		"masterDimmer",
		"strobe",
		"pan",
		"tilt",
		"zoom",
	];
	const seen = new Set<number>();
	for (const k of keys) {
		const v = mapping[k];
		if (typeof v === "number") seen.add(v);
	}
	return [...seen].sort((a, b) => a - b);
}

function sample(
	offsets: number[],
	values: number[],
	startAddress: number,
	universeLength: number,
): DmxSample {
	const dmx: Record<string, number> = {};
	offsets.forEach((off, i) => {
		const addr = startAddress + off;
		if (addr < universeLength) dmx[String(addr)] = values[i];
	});
	return { dmx };
}

function crossProduct(mapping: DmxMapping, startAddress: number): DmxSample[] {
	const offsets = mappedOffsets(mapping);
	const out: DmxSample[] = [];
	const total = LADDER.length ** offsets.length;
	for (let n = 0; n < total; n++) {
		let rest = n;
		const values = offsets.map(() => {
			const v = LADDER[rest % LADDER.length];
			rest = Math.floor(rest / LADDER.length);
			return v;
		});
		out.push(sample(offsets, values, startAddress, UNIVERSE_LENGTH));
	}
	return out;
}

function sampled(
	mapping: DmxMapping,
	startAddress: number,
	seed: number,
	count: number,
): DmxSample[] {
	const offsets = mappedOffsets(mapping);
	const rand = prng(seed);
	const out: DmxSample[] = [];
	for (let n = 0; n < count; n++) {
		const values = offsets.map(
			() => LADDER[Math.floor(rand() * LADDER.length)],
		);
		out.push(sample(offsets, values, startAddress, UNIVERSE_LENGTH));
	}
	return out;
}

/** Sweeps the strobe channel across all 256 values, everything else pinned. */
function strobeSweep(mapping: DmxMapping, startAddress: number): DmxSample[] {
	const offsets = mappedOffsets(mapping);
	const out: DmxSample[] = [];
	for (let v = 0; v < 256; v++) {
		const values = offsets.map((off) => (off === mapping.strobe ? v : 200));
		out.push(sample(offsets, values, startAddress, UNIVERSE_LENGTH));
	}
	return out;
}

const NULL_MAPPING: DmxMapping = {
	red: null,
	green: null,
	blue: null,
	white: null,
	amber: null,
	cyan: null,
	magenta: null,
	yellow: null,
	dimmer: null,
	masterDimmer: null,
	strobe: null,
	strobeCapabilities: [],
	pan: null,
	tilt: null,
	zoom: null,
};

/**
 * The full case list. The generator and the test agree on this ordering, but
 * the test only reads inputs back out of the golden file — this function exists
 * so the goldens can be re-cut without hand-writing 5000 DMX frames.
 */
export function buildCaseInputs(): { case: string; input: CaseInput }[] {
	const cases: { case: string; input: CaseInput }[] = [];

	// --- getDmxMapping ------------------------------------------------------
	const mappingCases: MappingCaseInput[] = [
		{ fixture: "rgb3", mode: "3 Channel", headIndex: 0 },
		// headless mode ignores headIndex entirely
		{ fixture: "rgb3", mode: "3 Channel", headIndex: 1 },
		{ fixture: "rgb3", mode: "No Such Mode", headIndex: 0 },
		{ fixture: "rgbw_master", mode: "5 Channel", headIndex: 0 },
		{ fixture: "rgbw_master", mode: "4 Channel", headIndex: 0 },
		{ fixture: "rgba", mode: "4 Channel", headIndex: 0 },
		{ fixture: "cmy_mover", mode: "8 Channel", headIndex: 0 },
		{ fixture: "cmy_mover", mode: "8 Channel", headIndex: 2 },
		{ fixture: "cmy_mover", mode: "6 Channel", headIndex: 0 },
		{ fixture: "bar3", mode: "14 Channel", headIndex: 0 },
		{ fixture: "bar3", mode: "14 Channel", headIndex: 1 },
		{ fixture: "bar3", mode: "14 Channel", headIndex: 2 },
		// out of range head -> no head channels at all
		{ fixture: "bar3", mode: "14 Channel", headIndex: 3 },
		{ fixture: "missing_channel", mode: "Broken", headIndex: 0 },
		{ fixture: "quirky_master", mode: "5 Channel", headIndex: 0 },
		{ fixture: "no_caps_strobe", mode: "4 Channel", headIndex: 0 },
	];
	for (const input of mappingCases) {
		cases.push({
			case: `mapping/${input.fixture}/${input.mode}/head${input.headIndex}`,
			input,
		});
	}

	// --- getHeadState -------------------------------------------------------
	const m = (fixture: string, modeName: string, head: number) =>
		getDmxMapping(FIXTURES[fixture], modeName, head);

	const rgb3 = m("rgb3", "3 Channel", 0);
	const rgbw = m("rgbw_master", "5 Channel", 0);
	const rgba = m("rgba", "4 Channel", 0);
	const cmy = m("cmy_mover", "8 Channel", 0);
	const bar0 = m("bar3", "14 Channel", 0);
	const bar1 = m("bar3", "14 Channel", 1);
	const bar2 = m("bar3", "14 Channel", 2);
	const bar3 = m("bar3", "14 Channel", 3);
	const broken = m("missing_channel", "Broken", 0);
	const quirky = m("quirky_master", "5 Channel", 0);
	const fallback = m("no_caps_strobe", "4 Channel", 0);

	const st = (
		name: string,
		mapping: DmxMapping,
		startAddress: number,
		samples: DmxSample[],
	) => {
		cases.push({
			case: name,
			input: {
				mapping,
				startAddress,
				universeLength: UNIVERSE_LENGTH,
				samples,
			},
		});
	};

	// full 8^3 value ladder cross-product, base address 0
	st("state/rgb3/cross-product@0", rgb3, 0, crossProduct(rgb3, 0));
	// base address 38 of a 40-byte universe: the blue offset is truncated to 0
	st("state/rgb3/cross-product@38-truncated", rgb3, 38, crossProduct(rgb3, 38));
	st("state/rgbw_master/sampled@0", rgbw, 0, sampled(rgbw, 0, 0x5eed0001, 240));
	st(
		"state/rgbw_master/sampled@12",
		rgbw,
		12,
		sampled(rgbw, 12, 0x5eed0002, 160),
	);
	st("state/rgba/sampled@0", rgba, 0, sampled(rgba, 0, 0x5eed0003, 320));
	st("state/rgba/sampled@12", rgba, 12, sampled(rgba, 12, 0x5eed0004, 160));
	st("state/cmy_mover/sampled@0", cmy, 0, sampled(cmy, 0, 0x5eed0005, 500));
	st("state/cmy_mover/sampled@12", cmy, 12, sampled(cmy, 12, 0x5eed0006, 240));
	// base address 36: pan/tilt/cyan/magenta fit, the rest truncate to 0
	st(
		"state/cmy_mover/sampled@36-truncated",
		cmy,
		36,
		sampled(cmy, 36, 0x5eed0007, 160),
	);
	// start address entirely past the end of the universe
	st(
		"state/cmy_mover/sampled@100-out-of-universe",
		cmy,
		100,
		sampled(cmy, 100, 0x5eed0008, 32),
	);
	// every capability family, plus the uncovered 240-255 gap
	st("state/cmy_mover/strobe-sweep@0", cmy, 0, strobeSweep(cmy, 0));
	st("state/cmy_mover/strobe-sweep@12", cmy, 12, strobeSweep(cmy, 12));
	// auto-generated 0-9 open / 10-255 StrobeSlowToFast table
	st("state/bar3/head0-strobe-sweep@0", bar0, 0, strobeSweep(bar0, 0));
	st("state/bar3/head0-sampled@0", bar0, 0, sampled(bar0, 0, 0x5eed0009, 300));
	st(
		"state/bar3/head1-sampled@12",
		bar1,
		12,
		sampled(bar1, 12, 0x5eed000a, 200),
	);
	st("state/bar3/head2-sampled@0", bar2, 0, sampled(bar2, 0, 0x5eed000b, 200));
	st(
		"state/bar3/head2-sampled@22-truncated",
		bar2,
		22,
		sampled(bar2, 22, 0x5eed000c, 160),
	);
	// out-of-range head: only the global master/strobe survive
	st(
		"state/bar3/head3-out-of-range@0",
		bar3,
		0,
		sampled(bar3, 0, 0x5eed000d, 128),
	);
	st(
		"state/missing_channel/cross-product@0",
		broken,
		0,
		crossProduct(broken, 0),
	);
	st(
		"state/quirky_master/sampled@0",
		quirky,
		0,
		sampled(quirky, 0, 0x5eed000e, 200),
	);
	// fallback branch: no capabilities, generic <10 open / else 1..15Hz ramp
	st(
		"state/no_caps_strobe/fallback-sweep@0",
		fallback,
		0,
		strobeSweep(fallback, 0),
	);
	st(
		"state/no_caps_strobe/fallback-sweep@12",
		fallback,
		12,
		strobeSweep(fallback, 12),
	);
	// degenerate: nothing mapped at all -> full intensity, black, open shutter
	st("state/degenerate/null-mapping", NULL_MAPPING, 0, [
		{ dmx: {} },
		{ dmx: { "0": 255, "5": 255 } },
	]);

	return cases;
}

// ---------------------------------------------------------------------------
// Shared helpers (also used by the generator)
// ---------------------------------------------------------------------------

export function applyDmx(
	universeLength: number,
	dmx: Record<string, number>,
): Uint8Array {
	const data = new Uint8Array(universeLength);
	for (const [addr, value] of Object.entries(dmx)) {
		const i = Number(addr);
		if (i >= 0 && i < universeLength) data[i] = value;
	}
	return data;
}

const round = (v: number, dp: number) => {
	const f = 10 ** dp;
	const r = Math.round(v * f) / f;
	// normalise -0 so JSON round-trips identically
	return r === 0 ? 0 : r;
};

/** Colour + intensity quantised to 1e-6, strobe Hz to 1e-4. */
export function roundState(s: FixtureState) {
	return {
		color: {
			r: round(s.color.r, 6),
			g: round(s.color.g, 6),
			b: round(s.color.b, 6),
		},
		intensity: round(s.intensity, 6),
		strobe: round(s.strobe, 4),
		shutter: s.shutter,
		zoom: s.zoom,
		pan: s.pan,
		tilt: s.tilt,
	};
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

interface GoldenEntry {
	case: string;
	input: CaseInput;
	output: unknown;
}

const golden: GoldenEntry[] = JSON.parse(readFileSync(GOLDEN_PATH, "utf8"));

describe("fixture-utils golden vectors", () => {
	it("has a golden file with a sane number of cases", () => {
		expect(golden.length).toBeGreaterThanOrEqual(15);
		expect(golden.length).toBeLessThanOrEqual(60);
	});

	it("golden case names are unique", () => {
		expect(new Set(golden.map((g) => g.case)).size).toBe(golden.length);
	});

	for (const entry of golden) {
		it(entry.case, () => {
			if (entry.case.startsWith("mapping/")) {
				const input = entry.input as MappingCaseInput;
				const fixture = FIXTURES[input.fixture];
				expect(fixture, `unknown fixture ${input.fixture}`).toBeDefined();
				expect(getDmxMapping(fixture, input.mode, input.headIndex)).toEqual(
					entry.output,
				);
				return;
			}

			const input = entry.input as StateCaseInput;
			const actual = input.samples.map((s) =>
				roundState(
					getHeadState(
						input.mapping,
						applyDmx(input.universeLength, s.dmx),
						input.startAddress,
					),
				),
			);
			expect(actual).toEqual(entry.output);
		});
	}
});
