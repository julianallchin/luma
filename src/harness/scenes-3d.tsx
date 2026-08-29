import { advance } from "@react-three/fiber";
import ReactDOM from "react-dom/client";
import type { BoxGeometry, Mesh } from "three";

import "@/App.css";
import type { PrimitiveState } from "@/bindings/universe";
import type { ResolvedNode } from "@/bindings/venue-graph";
import { getNodeGroup } from "@/features/stage/lib/node-refs";
import { useVenueStore } from "@/features/stage/stores/use-venue-store";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import { PrimitiveOverrideContext } from "@/features/visualizer/hooks/use-primitive-state";
import { getFixtureGroup } from "@/features/visualizer/lib/fixture-refs";
import { useCameraStore } from "@/features/visualizer/stores/use-camera-store";
import { useRenderSettingsStore } from "@/features/visualizer/stores/use-render-settings-store";
import {
	BASE_RENDER_SETTINGS,
	GOLDEN_DEFINITIONS,
	GOLDEN_SCENES,
	type GoldenScene,
	sceneById,
	timesFor,
} from "./golden-scenes";
import { installTauriStub } from "./tauri-stub";

// ---------------------------------------------------------------------------
// Golden-scene capture page for the three.js renderer (spec §7.1). Served at
// /harness-3d.html?scene=<id>; `harness/shot-visualizer.mjs` drives it.
//
// Determinism rests on four pins, all of which must hold together:
//   1. no backend — stores are seeded from `golden-scenes.ts`, fixture state
//      arrives through `PrimitiveOverrideContext`, Tauri is stubbed out;
//   2. `frameloop="never"` — the clock only moves when we call `advance(t)`,
//      which is what haze noise drift and strobe phase read;
//   3. one page load per frame — `uFrame` (the jitter walk) and the temporal
//      history both start from zero;
//   4. WARMUP_FRAMES advances at the *same* t, so the temporal EMA converges
//      before the shot. Mirrors the export subframe loop.
// ---------------------------------------------------------------------------

/** 1/alpha ~= frames averaged; 64 is comfortably past convergence at alpha 0.4. */
const WARMUP_FRAMES = 64;

/** CSS pixels. At the capture script's 2x scale this is the spec's 1600x1000. */
const VIEWPORT = { width: 800, height: 500 };

interface GoldenApi {
	scenes: { id: string; times: number[] }[];
	/** True once every fixture and stage piece has its real mesh mounted. */
	ready: () => boolean;
	/** Advance to `t` and settle the temporal accumulator. */
	frame: (t: number) => void;
	/** Everything the manifest records about the mounted scene. */
	describe: () => unknown;
}

declare global {
	interface Window {
		__GOLDEN__: GoldenApi;
	}
}

installTauriStub();

const sceneId = new URLSearchParams(window.location.search).get("scene");
const scene = sceneId ? sceneById(sceneId) : undefined;

/**
 * The fallback cube `FixtureObject` renders while a definition or GLB is in
 * flight. Its presence anywhere under a fixture group means the scene is not
 * finished loading.
 */
const FALLBACK_CUBE_SIZE = 0.2;

function hasFallbackCube(root: {
	traverse: (fn: (o: object) => void) => void;
}): boolean {
	let found = false;
	root.traverse((obj) => {
		const mesh = obj as Mesh;
		if (!mesh.isMesh || mesh.geometry?.type !== "BoxGeometry") return;
		const p = (mesh.geometry as BoxGeometry).parameters;
		if (
			p.width === FALLBACK_CUBE_SIZE &&
			p.height === FALLBACK_CUBE_SIZE &&
			p.depth === FALLBACK_CUBE_SIZE
		) {
			found = true;
		}
	});
	return found;
}

function sceneReady(s: GoldenScene): boolean {
	for (const f of s.fixtures) {
		const group = getFixtureGroup(f.id);
		if (!group || hasFallbackCube(group)) return false;
	}
	for (const p of s.pieces ?? []) {
		if (!getNodeGroup(p.id)) return false;
	}
	return true;
}

function seedStores(s: GoldenScene): void {
	useRenderSettingsStore.setState({ ...BASE_RENDER_SETTINGS, ...s.render });
	useCameraStore.setState({
		position: s.camera.position,
		target: s.camera.target,
	});
	useFixtureStore.setState({
		patchedFixtures: s.fixtures,
		definitionsCache: new Map(Object.entries(GOLDEN_DEFINITIONS)),
		selectedPatchedIds: new Set(s.selectedFixtureIds ?? []),
		lastSelectedPatchedId: s.selectedFixtureIds?.[0] ?? null,
		previewFixtureIds: [],
		// Left null so the visualizer's venue effect never calls
		// `initialize`, which would try to load pieces over IPC.
		venueId: null,
	});
	// Fixtures get a venue node each, from the same pose the patch row was
	// authored with: a fixture is drawn only where the solve places it, and the
	// harness is the solver here.
	const fixtureNodes: ResolvedNode[] = s.fixtures.map((f) => ({
		id: f.id,
		kind: "fixture",
		catalogRef: f.id,
		label: f.label,
		parentId: null,
		position: [f.posX, f.posY, f.posZ],
		rotation: [f.rotX, f.rotY, f.rotZ],
		facing: [0, 0, 1],
		arrayIndex: null,
		// A fixture is drawn from the patch, never from the set-piece list.
		setPiece: false,
		params: {},
	}));
	const nodes = [...fixtureNodes, ...(s.pieces ?? [])];
	useVenueStore.setState({
		// Left null so the visualizer's venue effect never calls `initialize`,
		// which would try to solve the venue over IPC.
		venueId: null,
		nodes,
		byId: new Map(nodes.map((n) => [n.id, n])),
		warnings: [],
		unplaced: [],
	});
}

function GoldenPage({ s }: { s: GoldenScene }) {
	const lookup = (id: string): PrimitiveState | undefined => s.state[id];
	return (
		<PrimitiveOverrideContext.Provider value={() => lookup}>
			<div style={{ position: "relative", ...VIEWPORT }}>
				<StageVisualizer frameloop="never" enableEditing={s.editing ?? false} />
			</div>
		</PrimitiveOverrideContext.Provider>
	);
}

const root = ReactDOM.createRoot(
	document.getElementById("root") as HTMLElement,
);

if (!scene) {
	root.render(
		<div style={{ color: "#ccc", font: "12px monospace", padding: 24 }}>
			{sceneId ? `Unknown scene: ${sceneId}` : "Pass ?scene=<id>"}. Available:{" "}
			{GOLDEN_SCENES.map((s) => s.id).join(", ")}
		</div>,
	);
} else {
	seedStores(scene);
	root.render(<GoldenPage s={scene} />);
}

window.__GOLDEN__ = {
	scenes: GOLDEN_SCENES.map((s) => ({ id: s.id, times: timesFor(s) })),
	ready: () => (scene ? sceneReady(scene) : false),
	frame: (t: number) => {
		// Repeated advances at one t leave delta at 0 after the first, so only
		// the frame counter and the temporal accumulator move — exactly the
		// convergence the spec asks for, with no clock drift.
		for (let i = 0; i <= WARMUP_FRAMES; i++) advance(t);
	},
	describe: () =>
		scene && {
			id: scene.id,
			isolates: scene.isolates,
			camera: scene.camera,
			renderSettings: { ...BASE_RENDER_SETTINGS, ...scene.render },
			editing: scene.editing ?? false,
			viewport: VIEWPORT,
			warmupFrames: WARMUP_FRAMES,
			fixtures: scene.fixtures.map((f) => ({
				id: f.id,
				fixturePath: f.fixturePath,
				modeName: f.modeName,
				pos: [f.posX, f.posY, f.posZ],
				rot: [f.rotX, f.rotY, f.rotZ],
			})),
			pieces: (scene.pieces ?? []).map((p) => ({
				id: p.id,
				meshPath: p.catalogRef,
				pos: p.position,
				rot: p.rotation,
			})),
			selectedFixtureIds: scene.selectedFixtureIds ?? [],
			state: scene.state,
			tauriCalls: window.__TAURI_CALLS__ ?? [],
		},
};
