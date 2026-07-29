import type { Graph, NodeInstance } from "@/bindings/schema";

/**
 * Layered (left → right) auto-layout for an agent-built graph.
 *
 * Signal flows from sources (pattern_args, scalars, palettes) on the left to
 * sinks (view nodes) on the right, so we place each node in a column equal to
 * its longest-path depth from a source, then order nodes within each column by
 * the barycenter of their neighbours to reduce edge crossings. This suits the
 * orthogonal `FilletEdge` routing — clean horizontal runs between columns with
 * vertical drops within them.
 *
 * Deterministic: the same topology always yields the same positions, so
 * param-only edits don't shuffle the canvas. Heights vary (view nodes are tall)
 * so the row gap is generous rather than exact.
 */

const COL_GAP = 320;
const ROW_GAP = 36; // vertical breathing room *between* nodes

/**
 * Estimated rendered height per node type, in px. React Flow only knows real
 * sizes after a node mounts, and the agent lays out *before* new nodes render,
 * so we estimate by type — tall canvas/editor nodes vs compact value nodes.
 * Good enough for non-overlapping stacking; exact heights would need a measure
 * pass after render.
 */
function estimateNodeHeight(node: NodeInstance): number {
	switch (node.typeId) {
		case "view_signal":
		case "view_channel":
		case "view_events":
		case "view_uv":
		case "mel_spec_viewer":
			return 260; // canvas preview
		case "adsr":
		case "beat_envelope":
			return 210; // envelope editor
		case "color":
		case "palette":
		case "gradient":
			return 170; // swatch / stops editor
		case "frequency_amplitude":
		case "noise":
		case "rainbow":
		case "falloff":
			return 130;
		default:
			return 96; // standard / math / scalar / threshold / invert / …
	}
}

export function layoutGraph(graph: Graph): Graph {
	const nodes = graph.nodes ?? [];
	if (nodes.length === 0) return graph;
	const edges = graph.edges ?? [];

	const ids = nodes.map((n) => n.id);
	const idSet = new Set(ids);
	const parents = new Map<string, string[]>();
	const children = new Map<string, string[]>();
	for (const id of ids) {
		parents.set(id, []);
		children.set(id, []);
	}
	for (const e of edges) {
		if (!idSet.has(e.fromNode) || !idSet.has(e.toNode)) continue;
		if (e.fromNode === e.toNode) continue;
		children.get(e.fromNode)?.push(e.toNode);
		parents.get(e.toNode)?.push(e.fromNode);
	}

	// Column = longest path from any source. Memoized DFS with a cycle guard
	// (graphs shouldn't cycle, but never recurse forever if one slips through).
	const depth = new Map<string, number>();
	const visiting = new Set<string>();
	const computeDepth = (id: string): number => {
		const cached = depth.get(id);
		if (cached !== undefined) return cached;
		if (visiting.has(id)) return 0; // cycle: break
		visiting.add(id);
		let d = 0;
		for (const p of parents.get(id) ?? []) {
			d = Math.max(d, computeDepth(p) + 1);
		}
		visiting.delete(id);
		depth.set(id, d);
		return d;
	};
	for (const id of ids) computeDepth(id);

	// Group into columns, preserving the graph's node order as the initial order.
	const maxCol = Math.max(...ids.map((id) => depth.get(id) ?? 0));
	const columns: string[][] = Array.from({ length: maxCol + 1 }, () => []);
	for (const id of ids) columns[depth.get(id) ?? 0].push(id);

	// Current row of each node (its index within its column).
	const rowOf = new Map<string, number>();
	const reindex = () => {
		for (const col of columns) {
			col.forEach((id, row) => {
				rowOf.set(id, row);
			});
		}
	};
	reindex();

	const barycenter = (id: string, neighbours: Map<string, string[]>) => {
		const ns = neighbours.get(id) ?? [];
		const rows = ns
			.map((n) => rowOf.get(n))
			.filter((r): r is number => r !== undefined);
		if (rows.length === 0) return rowOf.get(id) ?? 0;
		return rows.reduce((a, b) => a + b, 0) / rows.length;
	};

	// A few crossing-reduction sweeps: order each column by the barycenter of its
	// parents (down sweep) then children (up sweep). Stable enough in 2 rounds.
	const stableSort = (col: string[], key: (id: string) => number) =>
		col
			.map((id, i) => ({ id, i, k: key(id) }))
			.sort((a, b) => a.k - b.k || a.i - b.i)
			.map((e) => e.id);

	for (let round = 0; round < 2; round++) {
		for (let c = 1; c <= maxCol; c++) {
			columns[c] = stableSort(columns[c], (id) => barycenter(id, parents));
			reindex();
		}
		for (let c = maxCol - 1; c >= 0; c--) {
			columns[c] = stableSort(columns[c], (id) => barycenter(id, children));
			reindex();
		}
	}

	// Estimated height per node (pattern_args rows come from the args list, not
	// its params), so columns stack by real size instead of a fixed gap.
	const nodeById = new Map(nodes.map((n) => [n.id, n]));
	const argCount = (graph.args ?? []).length;
	const heightOf = (id: string): number => {
		const node = nodeById.get(id);
		if (!node) return 96;
		if (node.typeId === "pattern_args") return 60 + Math.max(1, argCount) * 26;
		return estimateNodeHeight(node);
	};

	// Assign positions: stack each column by cumulative height + gap, and
	// vertically center every column around a shared axis so the graph reads
	// balanced rather than top-anchored.
	const colHeight = (col: string[]) =>
		col.reduce((sum, id) => sum + heightOf(id), 0) +
		Math.max(0, col.length - 1) * ROW_GAP;
	const tallest = Math.max(...columns.map(colHeight));
	const pos = new Map<string, { x: number; y: number }>();
	columns.forEach((col, c) => {
		let y = (tallest - colHeight(col)) / 2;
		for (const id of col) {
			pos.set(id, { x: c * COL_GAP, y });
			y += heightOf(id) + ROW_GAP;
		}
	});

	return {
		...graph,
		nodes: nodes.map((n) => {
			const p = pos.get(n.id);
			return p ? { ...n, positionX: p.x, positionY: p.y } : n;
		}),
	};
}
