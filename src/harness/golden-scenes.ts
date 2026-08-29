import type { FixtureDefinition, PatchedFixture } from "@/bindings/fixtures";
import type { PrimitiveState } from "@/bindings/universe";
import type { ResolvedNode } from "@/bindings/venue-graph";
import type { RenderSettings } from "@/features/visualizer/stores/use-render-settings-store";

// ---------------------------------------------------------------------------
// Golden scenes for the three.js -> wgpu port (docs/specs/wgpu-renderer.md §7).
//
// A scene is a complete, self-contained description of one frame's *inputs*:
// venue geometry, fixture definitions, primitive state, camera pose and render
// settings. Nothing here reads the database, the eval engine, or Tauri — the
// whole point is that the same inputs produce the same pixels on both
// renderers, and that a golden can be re-derived from this file alone.
//
// Fixture positions are Z-up data space (posZ is height); the renderer swaps
// Y<->Z on the way into three.js. Camera poses are three.js Y-up, because that
// is what `useCameraStore` holds.
// ---------------------------------------------------------------------------

type Vec3 = [number, number, number];

export interface GoldenScene {
	id: string;
	/** What a diff on this scene is evidence of. Recorded in the manifest. */
	isolates: string;
	camera: { position: Vec3; target: Vec3 };
	/** Overrides on top of {@link BASE_RENDER_SETTINGS}. */
	render?: Partial<RenderSettings>;
	/** Mounts the editor affordances: gizmo, grid, lit stage. */
	editing?: boolean;
	fixtures: PatchedFixture[];
	/** Structure nodes of the solved venue, already posed in world space. */
	pieces?: ResolvedNode[];
	/** Drives the gizmo and the selection outline (scene 5). */
	selectedFixtureIds?: string[];
	/** Fixed `PrimitiveState` per `"<fixtureId>:<head>"` key. */
	state: Record<string, PrimitiveState>;
	/**
	 * Clock values (seconds) to capture at. Irrational-ish by default so no
	 * strobe or noise phase lands on a special value; scenes that gate on a
	 * specific phase pin their own.
	 */
	times?: number[];
}

/** Default capture timestamps (spec §7.2). */
export const DEFAULT_TIMES = [0.0, 1.37, 4.2];

/**
 * Every dial the goldens depend on, pinned. `maxDpr: 2` pairs with the
 * capture script's `deviceScaleFactor: 2` so the drawing buffer is exactly
 * 2x the CSS viewport regardless of the host display.
 */
export const BASE_RENDER_SETTINGS: RenderSettings = {
	darkStage: true,
	volumetricHaze: true,
	hazeSteps: 8,
	hazeResolution: 1,
	hazeDensity: 0.8,
	hazeDenoise: true,
	fixtureSpotlights: true,
	bloom: false,
	maxDpr: 2,
	fov: 50,
};

// --- fixture definitions ---------------------------------------------------
// Transcribed from the bundled QLC+ library so cone angles come from real
// `Physical.Lens` data (the path `luminaire.ts` takes in the app). `Channel` is
// empty because the visualizer never reads it — it patches DMX, not pixels.

function definition(
	model: string,
	type: string,
	physical: FixtureDefinition["Physical"],
	modes: FixtureDefinition["Mode"] = [
		{ "@Name": "Default", Channel: [], Head: [] },
	],
): FixtureDefinition {
	return {
		Manufacturer: "Golden",
		Model: model,
		Type: type,
		Channel: [],
		Mode: modes,
		Physical: physical,
	};
}

function physical(
	dims: [number, number, number],
	lens: [number, number],
	layout?: [number, number],
): FixtureDefinition["Physical"] {
	return {
		Dimensions: {
			"@Weight": 10,
			"@Width": dims[0],
			"@Height": dims[1],
			"@Depth": dims[2],
		},
		Layout: layout ? { "@Width": layout[0], "@Height": layout[1] } : null,
		Bulb: null,
		Lens: { "@Name": "Other", "@DegreesMin": lens[0], "@DegreesMax": lens[1] },
		Focus: null,
		Technical: null,
	};
}

export const MOVER_PATH = "golden/moving-head.qxf";
export const PAR_PATH = "golden/par.qxf";
export const STROBE_PATH = "golden/strobe.qxf";
export const BAR_PATH = "golden/led-bar.qxf";
export const HAZER_PATH = "golden/hazer.qxf";

const BAR_PIXELS = 8;

export const GOLDEN_DEFINITIONS: Record<string, FixtureDefinition> = {
	// Martin MAC Aura: 11-58 deg zoom lens, mid-zoom is what a zoom-less state
	// model renders.
	[MOVER_PATH]: definition(
		"Mover",
		"Moving Head",
		physical([302, 302, 361], [11, 58]),
	),
	// Involight SlimPar 56 PRO.
	[PAR_PATH]: definition(
		"Par",
		"Color Changer",
		physical([265, 245, 115], [40, 40]),
	),
	// Martin Atomic 3000 LED: no lens data, so the kind fallback answers.
	[STROBE_PATH]: definition(
		"Strobe",
		"Strobe",
		physical([425, 240, 245], [0, 0]),
	),
	// XStatic X-240Bar RGB: 8 pixels on one row.
	[BAR_PATH]: definition(
		"Bar",
		"LED Bar (Pixels)",
		physical([1064, 65, 88], [30, 30], [BAR_PIXELS, 1]),
		[
			{
				"@Name": "Pixels",
				Channel: [],
				Head: Array.from({ length: BAR_PIXELS }, () => ({ Channel: [] })),
			},
		],
	),
	// Martin 24/7 Hazer. Emits no beam; its dimmer scales global haze density.
	[HAZER_PATH]: definition("Hazer", "Smoke", physical([246, 350, 419], [0, 0])),
};

// --- venue helpers ---------------------------------------------------------

const VENUE_ID = "golden-venue";

interface FixtureSpec {
	id: string;
	path: string;
	/** Z-up data space; z is height above the floor. */
	pos: Vec3;
	/** Radians, Z-up data space. */
	rot?: Vec3;
	mode?: string;
}

let nextAddress = 1;

function fixture({
	id,
	path,
	pos,
	rot = [0, 0, 0],
	mode = "Default",
}: FixtureSpec): PatchedFixture {
	const address = nextAddress;
	nextAddress += 16;
	const def = GOLDEN_DEFINITIONS[path];
	return {
		id,
		uid: null,
		venueId: VENUE_ID,
		universe: 0n,
		address: BigInt(address),
		numChannels: 16n,
		manufacturer: def.Manufacturer,
		model: def.Model,
		modeName: mode,
		addressPinned: false,
		fixturePath: path,
		label: id,
		posX: pos[0],
		posY: pos[1],
		posZ: pos[2],
		rotX: rot[0],
		rotY: rot[1],
		rotZ: rot[2],
	};
}

/**
 * One structure node, already solved: a golden scene is a description of a
 * frame's inputs, and the resolver's output is what the renderer takes.
 */
function piece(
	id: string,
	meshPath: string,
	pos: Vec3,
	rot: Vec3 = [0, 0, 0],
): ResolvedNode {
	return {
		id,
		kind: "piece",
		catalogRef: meshPath,
		label: id,
		parentId: null,
		position: pos,
		rotation: rot,
		facing: [0, 0, 1],
		arrayIndex: null,
		// A golden piece is one object with one mesh; the resolver's own
		// answer for a plain `piece` carrying a `catalogRef`.
		setPiece: true,
		params: {},
	};
}

function head(
	dimmer: number,
	color: Vec3,
	position: [number, number] = [0, 0],
	strobe = 0,
): PrimitiveState {
	return { dimmer, color, strobe, position, speed: 0 };
}

const WHITE: Vec3 = [1, 1, 1];
const RED: Vec3 = [1, 0.05, 0.05];
const GREEN: Vec3 = [0.05, 1, 0.05];
const BLUE: Vec3 = [0.1, 0.2, 1];
const MAGENTA: Vec3 = [1, 0.1, 0.8];
const FAN_COLORS: Vec3[] = [RED, GREEN, BLUE, MAGENTA];

/**
 * A hazer parked behind the camera. Global haze density is scaled by the
 * strongest hazer dimmer (0.3x floor with none present), so scenes that want
 * their nominal density must patch one.
 */
const HAZER = fixture({ id: "hazer", path: HAZER_PATH, pos: [0, 8, 0.4] });

// --- scenes ----------------------------------------------------------------

function singleMover(): GoldenScene {
	const mover = fixture({ id: "mover", path: MOVER_PATH, pos: [0, 0, 4] });
	return {
		id: "single-mover",
		isolates: "beam axis, cone half-angle, near-field core",
		camera: { position: [5.5, 2.2, 5.5], target: [0, 1.5, 0] },
		fixtures: [mover, HAZER],
		state: { "mover:0": head(1, WHITE, [0, 30]), "hazer:0": head(1, WHITE) },
	};
}

function moverFan(): GoldenScene {
	const movers = Array.from({ length: 8 }, (_, i) =>
		fixture({ id: `mover-${i}`, path: MOVER_PATH, pos: [-3.5 + i, 0, 4] }),
	);
	const state: Record<string, PrimitiveState> = { "hazer:0": head(1, WHITE) };
	for (let i = 0; i < movers.length; i++) {
		// Fan pans outward, tilts alternate so the cones cross rather than run
		// parallel — overlap summation is the thing under test.
		const pan = -42 + i * 12;
		const tilt = i % 2 === 0 ? 28 : 44;
		state[`mover-${i}:0`] = head(1, FAN_COLORS[i % FAN_COLORS.length], [
			pan,
			tilt,
		]);
	}
	return {
		id: "mover-fan",
		isolates: "colour, overlap summation, per-light jitter decorrelation",
		camera: { position: [0, 3.4, 13] as Vec3, target: [0, 1.6, 0] },
		fixtures: [...movers, HAZER],
		state,
	};
}

function parOcclusion(): GoldenScene {
	const par = fixture({ id: "par", path: PAR_PATH, pos: [0, -1.2, 3.6] });
	return {
		id: "par-occlusion",
		isolates: "occlusion, bilateral upsample at silhouettes",
		camera: { position: [4.5, 1.8, 5.5], target: [0, 1.0, 0] },
		fixtures: [par, HAZER],
		// A stage deck sits across the beam at mid-throw. It has to be wide
		// enough to actually cut the cone: the haze must stop at its
		// silhouette and the floor pool must be notched.
		pieces: [
			piece("deck", "stage_lab/stage_praticavel_2x1x1.glb", [-1, -1.2, 1.4]),
		],
		state: { "par:0": head(1, WHITE, [0, 0]), "hazer:0": head(1, WHITE) },
	};
}

function ledBar(): GoldenScene {
	// Hung, so the row of pixel cones fires down at the floor: a fixture rests
	// along its mount normal, and an unrotated mount hangs.
	const bar = fixture({
		id: "bar",
		path: BAR_PATH,
		pos: [0, 0, 2.6],
		mode: "Pixels",
	});
	const state: Record<string, PrimitiveState> = { "hazer:0": head(1, WHITE) };
	for (let i = 0; i < BAR_PIXELS; i++) {
		// A hue ramp across the row: per-pixel colour and the sqrt(headCount)
		// normalisation are both visible in one frame.
		const phase = i / BAR_PIXELS;
		state[`bar:${i}`] = head(1, [
			0.5 + 0.5 * Math.cos(2 * Math.PI * phase),
			0.5 + 0.5 * Math.cos(2 * Math.PI * (phase + 1 / 3)),
			0.5 + 0.5 * Math.cos(2 * Math.PI * (phase + 2 / 3)),
		]);
	}
	return {
		id: "led-bar",
		isolates: "procedural pixel path, sqrt(headCount) normalisation",
		camera: { position: [3.2, 1.9, 5.0], target: [0, 1.2, 0] },
		fixtures: [bar, HAZER],
		state,
	};
}

function stageBuilder(): GoldenScene {
	const movers = [
		fixture({ id: "sb-mover-l", path: MOVER_PATH, pos: [-1.5, 0, 3.2] }),
		fixture({ id: "sb-mover-r", path: MOVER_PATH, pos: [1.5, 0, 3.2] }),
	];
	return {
		id: "stage-builder",
		isolates: "PBR, shadow map, grid shader, fixture selection outline",
		camera: { position: [5, 3.5, 6], target: [0, 0.8, 0] },
		// The lit stage has no haze pass at all (`volumetricHaze && darkStage`).
		render: { darkStage: false },
		editing: true,
		fixtures: movers,
		pieces: [
			piece("deck-l", "stage_lab/stage_praticavel_2x1x1.glb", [-1, 0, 0]),
			piece("deck-r", "stage_lab/stage_praticavel_2x1x1.glb", [1, 0, 0]),
			piece("truss-a", "stage_lab/truss_q40_1.83m.glb", [0, -1.6, 0]),
			piece("speaker", "stage_lab/speaker_dbr15.glb", [2.2, 0, 0]),
		],
		selectedFixtureIds: ["sb-mover-l"],
		state: {
			"sb-mover-l:0": head(1, WHITE, [-20, 35]),
			"sb-mover-r:0": head(1, WHITE, [20, 35]),
		},
	};
}

function strobeDuty(): GoldenScene {
	const strobe = fixture({ id: "strobe", path: STROBE_PATH, pos: [0, 0, 3.2] });
	return {
		id: "strobe-duty",
		isolates: "strobe phase gating",
		camera: { position: [3.6, 1.8, 4.6], target: [0, 1.4, 0] },
		fixtures: [strobe, HAZER],
		state: {
			"strobe:0": head(1, WHITE, [0, 0], 0.5),
			"hazer:0": head(1, WHITE),
		},
		// strobe 0.5 -> 10 Hz -> 100 ms period, lit for the first half. 0.02 is
		// mid-duty, 0.07 mid-gap; 1.37 is a third phase far from both edges.
		times: [0.02, 0.07, 1.37],
	};
}

/** Movers + pars + a bar: the shared body of scenes 7 and 8. */
function venueFixtures(): {
	fixtures: PatchedFixture[];
	state: Record<string, PrimitiveState>;
} {
	const fixtures: PatchedFixture[] = [];
	const state: Record<string, PrimitiveState> = {};
	for (let i = 0; i < 6; i++) {
		const f = fixture({
			id: `v-mover-${i}`,
			path: MOVER_PATH,
			pos: [-4 + i * 1.6, 1.5, 4.2],
		});
		fixtures.push(f);
		state[`${f.id}:0`] = head(1, FAN_COLORS[i % FAN_COLORS.length], [
			-30 + i * 12,
			34,
		]);
	}
	for (let i = 0; i < 6; i++) {
		const f = fixture({
			id: `v-par-${i}`,
			path: PAR_PATH,
			pos: [-4 + i * 1.6, -1.5, 3.4],
		});
		fixtures.push(f);
		state[`${f.id}:0`] = head(0.8, WHITE);
	}
	const bar = fixture({
		id: "v-bar",
		path: BAR_PATH,
		pos: [0, 0.5, 0.15],
		mode: "Pixels",
	});
	fixtures.push(bar);
	for (let i = 0; i < BAR_PIXELS; i++)
		state[`v-bar:${i}`] = head(1, FAN_COLORS[i % FAN_COLORS.length]);
	return { fixtures, state };
}

function venueNoHaze(): GoldenScene {
	const { fixtures, state } = venueFixtures();
	return {
		id: "venue-no-haze",
		isolates:
			"geometry, materials, lighting and tonemap with zero stochastic content",
		camera: { position: [0, 3.2, 10] as Vec3, target: [0, 1.4, 0] },
		// The lit stage is the only mode with ambient + directional light, so
		// it is the only one where a geometry/material baseline is visible at
		// all. `hazeDensity: 0` is belt and braces: `darkStage: false` already
		// unmounts the haze pass.
		render: { darkStage: false, hazeDensity: 0 },
		fixtures,
		state,
	};
}

function denseVenue(): GoldenScene {
	const fixtures: PatchedFixture[] = [HAZER];
	const state: Record<string, PrimitiveState> = { "hazer:0": head(1, WHITE) };
	// 120 fixtures on a 20x6 grid of movers — the stress case for the haze
	// pass and the top-N spotlight selection.
	for (let row = 0; row < 6; row++) {
		for (let col = 0; col < 20; col++) {
			const id = `d-${row}-${col}`;
			fixtures.push(
				fixture({ id, path: MOVER_PATH, pos: [-9.5 + col, -2.5 + row, 4.5] }),
			);
			state[`${id}:0`] = head(1, FAN_COLORS[(row + col) % FAN_COLORS.length], [
				-30 + col * 3,
				20 + row * 6,
			]);
		}
	}
	return {
		id: "dense-venue",
		isolates: "perf and the many-light path",
		camera: { position: [0, 7.5, 19] as Vec3, target: [0, 1.0, 0] },
		render: { hazeDensity: 1 },
		fixtures,
		state,
	};
}

export const GOLDEN_SCENES: GoldenScene[] = [
	singleMover(),
	moverFan(),
	parOcclusion(),
	ledBar(),
	stageBuilder(),
	strobeDuty(),
	venueNoHaze(),
	denseVenue(),
];

export function sceneById(id: string): GoldenScene | undefined {
	return GOLDEN_SCENES.find((s) => s.id === id);
}

export function timesFor(scene: GoldenScene): number[] {
	return scene.times ?? DEFAULT_TIMES;
}
