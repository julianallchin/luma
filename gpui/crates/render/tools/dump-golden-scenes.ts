// Serialise the golden-scene catalogue for the Rust renderer.
//
//   bun gpui/crates/render/tools/dump-golden-scenes.ts
//
// `src/harness/golden-scenes.ts` is the one description of what the eight
// golden frames contain; `harness/goldens/scenes/manifest.json` is the record
// of a particular *capture* of it and omits the fixture definitions (lens
// angles, physical dimensions, head counts) that cone geometry needs. Rather
// than transcribe either into Rust, dump the module itself — regenerate this
// whenever the scenes change and the two renderers stay in step by
// construction.
import {
	BASE_RENDER_SETTINGS,
	GOLDEN_DEFINITIONS,
	GOLDEN_SCENES,
	timesFor,
} from "../../../../src/harness/golden-scenes";

const out = {
	// Mirrors `harness/shot-visualizer.mjs`: one page load per frame, 64
	// warmup advances at the same t. The Rust side accumulates the same
	// number of jitter subframes instead of running the temporal EMA.
	warmupFrames: 64,
	viewport: { width: 800, height: 500 },
	deviceScaleFactor: 2,
	definitions: GOLDEN_DEFINITIONS,
	scenes: GOLDEN_SCENES.map((s) => ({
		id: s.id,
		times: timesFor(s),
		camera: s.camera,
		editing: s.editing ?? false,
		render: { ...BASE_RENDER_SETTINGS, ...s.render },
		selectedFixtureIds: s.selectedFixtureIds ?? [],
		fixtures: s.fixtures.map((f) => ({
			id: f.id,
			fixturePath: f.fixturePath,
			modeName: f.modeName,
			pos: [f.posX, f.posY, f.posZ],
			rot: [f.rotX, f.rotY, f.rotZ],
		})),
		pieces: (s.pieces ?? []).map((p) => ({
			id: p.id,
			meshPath: p.meshPath,
			kind: p.kind,
			pos: [p.posX, p.posY, p.posZ],
			rot: [p.rotX, p.rotY, p.rotZ],
			scale: p.scale,
		})),
		state: s.state,
	})),
};

const dest = new URL("../goldens/scenes.json", import.meta.url);
await Bun.write(dest, `${JSON.stringify(out, null, "\t")}\n`);
console.log(dest.pathname);
