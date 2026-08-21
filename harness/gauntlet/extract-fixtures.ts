#!/usr/bin/env bun
// Extract the graph-editor gauntlet fixtures: the *real* saved graphs for the
// named patterns, plus the deterministic view-node signals and the two camera
// framings each capture uses.
//
//   bun harness/gauntlet/extract-fixtures.ts
//   bun harness/gauntlet/extract-fixtures.ts gradient circle_pill_step
//
// Node positions are normalized through the app's own `layoutGraph()` — the
// same left→right layered layout the editor's Layout button applies. Saved
// positions carry years of hand-dragging (in `gradient`, a stray disconnected
// `audio_input` sits at the origin, on top of two other cards), and a reference
// image with cards overlapping is not a quality bar. The layout is a pure
// function of topology, and its output is baked into the fixture, so both
// stacks render byte-identical positions.
//
// Output: harness/gauntlet/fixtures/<pattern>.json — the single file both the
// web capture page (`src/harness/graph-canvas.tsx`) and the GPUI port read, so
// the two stacks are provably rendering the same graph.
//
// The node *type* catalogue (ports, param defs) is not in the saved graph; it
// is compiled into the Rust core. Regenerate it with the `dump_node_types`
// binary, which is the same function the app serves over `get_node_types`:
//
//   cargo run --manifest-path src-tauri/Cargo.toml --bin dump_node_types \
//     > harness/gauntlet/node-types.json
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Graph, Signal } from "@/bindings/schema";
import { layoutGraph } from "@/features/patterns/agent/graph-layout";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const outDir = path.join(root, "harness/gauntlet/fixtures");
const db = path.join(
	homedir(),
	"Library/Application Support/com.luma.luma/luma.db",
);

// Per-pattern capture setup. `viewSignals` seeds the view nodes so they draw a
// real trace instead of the "waiting for signal data…" placeholder — a graph
// screenshot with an empty 720px box in it is not a quality bar. `closeup` is a
// fixed viewport (not a fitView) so the framing can't drift with measurement.
type Shape = { n: number; t: number; c: number };
type Setup = {
	views: Record<string, Shape>;
	closeup: { x: number; y: number; zoom: number };
};

const PATTERNS: Record<string, Setup> = {
	gradient: {
		views: { "node-30": { n: 1, t: 128, c: 1 } },
		// Frames the ramp → sample-palette → apply-color run plus the top of the
		// view node: every port hue in the graph, wires, and a text input.
		closeup: { x: -240, y: -30, zoom: 1 },
	},
	circle_pill_step: {
		views: { "node-70": { n: 4, t: 128, c: 1 } },
		// Frames the get-attribute selector, two math selectors and the round
		// node's text input — the param-control vocabulary at readable size.
		closeup: { x: -260, y: -255, zoom: 1 },
	},
};

/** Deterministic stand-in trace: pure function of (line, step), no RNG, no clock. */
function synthSignal({ n, t, c }: Shape): Signal {
	const data: number[] = [];
	for (let line = 0; line < n; line++) {
		for (let step = 0; step < t; step++) {
			const u = step / (t - 1);
			const phase = (line / Math.max(n, 1)) * Math.PI * 2;
			for (let ch = 0; ch < c; ch++) {
				data.push(
					0.5 + 0.45 * Math.sin(u * Math.PI * 4 + phase) * (1 - 0.4 * u),
				);
			}
		}
	}
	return { n, t, c, data: data.map((v) => Number(v.toFixed(6))) };
}

function graphFor(name: string): Graph {
	const json = execFileSync("sqlite3", [
		db,
		`SELECT i.graph_json FROM implementations i
		 JOIN patterns p ON p.id = i.pattern_id
		 WHERE p.name = '${name}' ORDER BY i.id LIMIT 1;`,
	])
		.toString()
		.trim();
	if (!json) throw new Error(`no implementation graph for pattern ${name}`);
	return JSON.parse(json);
}

const requested = process.argv.slice(2);
const names = requested.length > 0 ? requested : Object.keys(PATTERNS);

mkdirSync(outDir, { recursive: true });
for (const name of names) {
	const setup = PATTERNS[name];
	if (!setup) throw new Error(`unknown pattern: ${name}`);
	const viewSignals: Record<string, Signal> = {};
	for (const [nodeId, shape] of Object.entries(setup.views)) {
		viewSignals[nodeId] = synthSignal(shape);
	}
	const fixture = {
		pattern: name,
		graph: layoutGraph(graphFor(name)),
		viewSignals,
		closeup: setup.closeup,
	};
	const file = path.join(outDir, `${name}.json`);
	writeFileSync(file, `${JSON.stringify(fixture, null, "\t")}\n`);
	console.log(`${file}  ${fixture.graph.nodes.length} nodes`);
}
