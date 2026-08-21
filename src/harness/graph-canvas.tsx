import { useEffect, useMemo, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import {
	getRectOfNodes,
	ReactFlowProvider,
	useReactFlow,
	useStore,
} from "reactflow";

import "@/App.css";
import type { Graph, NodeTypeDef, Signal } from "@/bindings/schema";
import { useViewDataStore } from "@/features/patterns/stores/use-view-data-store";
import { patternArgsNodeDef } from "@/shared/lib/react-flow/pattern-args-node-def";
import {
	type EditorController,
	ReactFlowEditor,
} from "@/shared/lib/react-flow-editor";
import circlePillStep from "../../harness/gauntlet/fixtures/circle_pill_step.json";
import gradient from "../../harness/gauntlet/fixtures/gradient.json";
import nodeTypesJson from "../../harness/gauntlet/node-types.json";
import { installTauriStub } from "./tauri-stub";

// ---------------------------------------------------------------------------
// Reference-capture page for the pattern-graph canvas (the GPUI port's visual
// quality bar). Served at /harness-graph.html?pattern=<id>&view=<whole|closeup>;
// `harness/gauntlet/shot-graph.mjs` drives it.
//
// It mounts the REAL `<ReactFlowEditor>` with the REAL node components — no
// stand-ins — against the real saved graph of a real pattern. Determinism rests
// on the same pins the 3D goldens use:
//   1. no backend — Tauri is stubbed, the node catalogue is a checked-in JSON
//      dump of `get_node_types()`, view-node signals are seeded from the
//      fixture rather than executed;
//   2. no animation — the editor is read-only, so nothing hovers or focuses,
//      and `PlaybackIndicator` self-disables with no host audio loaded;
//   3. explicit framing — `whole` is one `fitView` at a fixed padding, and
//      `closeup` is a literal viewport from the fixture, so neither depends on
//      capture order;
//   4. one page load per shot, after `document.fonts.ready` and after every
//      node has been measured by React Flow.
// ---------------------------------------------------------------------------

type View = "whole" | "closeup";

/** Canvas size for the closeup, in CSS pixels (2x that in the captured PNG). */
const CLOSEUP_SIZE = { width: 900, height: 460 };

/**
 * The editor leaves React Flow's default `minZoom` at 0.5, so a 3000px-wide
 * graph cannot be zoomed out to fit a small pane — in the app either. Rather
 * than change app behaviour for a capture, the whole-graph shot sizes its
 * canvas to the graph at that zoom floor.
 */
const MIN_ZOOM = 0.5;
const VIEWS: View[] = ["whole", "closeup"];
const WHOLE_PAD = 24;

/** fitView padding for the whole-graph shot. */
const FIT_PADDING = 0.08;

type Fixture = {
	pattern: string;
	graph: Graph;
	viewSignals: Record<string, Signal>;
	closeup: { x: number; y: number; zoom: number };
};

const FIXTURES = [gradient, circlePillStep] as unknown as Fixture[];
const NODE_TYPES = nodeTypesJson as unknown as NodeTypeDef[];

declare global {
	interface Window {
		__GRAPH__: {
			patterns: string[];
			views: View[];
			ready: () => boolean;
			frame: () => void;
		};
	}
}

installTauriStub();

const params = new URLSearchParams(window.location.search);
const fixture = FIXTURES.find((f) => f.pattern === params.get("pattern"));
const view: View = params.get("view") === "closeup" ? "closeup" : "whole";

function GraphPage({ fixture }: { fixture: Fixture }) {
	const controllerRef = useRef<EditorController | null>(null);
	const { fitView, setViewport, getNodes } = useReactFlow();
	const [loaded, setLoaded] = useState(false);
	// React Flow measures each node after mount; framing before that lands would
	// fit against zero-sized boxes.
	const measured = useStore(
		(s) =>
			s.nodeInternals.size > 0 &&
			[...s.nodeInternals.values()].every((n) => n.width != null),
	);

	const getNodeDefinitions = useMemo(() => {
		const argsDef = patternArgsNodeDef(fixture.graph.args ?? []);
		const defs = argsDef ? [...NODE_TYPES, argsDef] : NODE_TYPES;
		return () => defs;
	}, [fixture]);

	useEffect(() => {
		useViewDataStore.getState().setResults(fixture.viewSignals, {}, {});
		controllerRef.current?.loadGraph(fixture.graph, getNodeDefinitions);
		setLoaded(true);
	}, [fixture, getNodeDefinitions]);

	// Whole view: grow the canvas to the graph's own extent at the zoom floor,
	// so `fitView` frames everything instead of clamping and cropping.
	const [size, setSize] = useState(CLOSEUP_SIZE);
	const sized = view === "closeup" || size !== CLOSEUP_SIZE;
	useEffect(() => {
		if (view === "closeup" || !measured) return;
		const b = getRectOfNodes(getNodes());
		setSize({
			width: Math.ceil(b.width * MIN_ZOOM) + 2 * WHOLE_PAD,
			height: Math.ceil(b.height * MIN_ZOOM) + 2 * WHOLE_PAD,
		});
	}, [measured, getNodes]);

	useEffect(() => {
		window.__GRAPH__ = {
			patterns: FIXTURES.map((f) => f.pattern),
			views: VIEWS,
			ready: () => loaded && measured && sized,
			frame: () => {
				if (view === "closeup") setViewport(fixture.closeup);
				else fitView({ padding: FIT_PADDING, duration: 0 });
			},
		};
	}, [loaded, measured, sized, fitView, setViewport, fixture]);

	return (
		<div id="canvas" style={view === "closeup" ? CLOSEUP_SIZE : size}>
			<ReactFlowEditor
				onChange={() => {}}
				getNodeDefinitions={getNodeDefinitions}
				controllerRef={controllerRef}
				readOnly
			/>
		</div>
	);
}

const root = ReactDOM.createRoot(
	document.getElementById("root") as HTMLElement,
);

if (!fixture) {
	window.__GRAPH__ = {
		patterns: FIXTURES.map((f) => f.pattern),
		views: VIEWS,
		ready: () => false,
		frame: () => {},
	};
	root.render(
		<div style={{ color: "#ccc", font: "12px monospace", padding: 24 }}>
			Pass ?pattern=&lt;id&gt;. Available:{" "}
			{FIXTURES.map((f) => f.pattern).join(", ")}
		</div>,
	);
} else {
	root.render(
		<ReactFlowProvider>
			<GraphPage fixture={fixture} />
		</ReactFlowProvider>,
	);
}
