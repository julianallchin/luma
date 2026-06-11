import type { Graph } from "@/bindings/schema";

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
const ROW_GAP = 170;

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
			col.forEach((id, row) => rowOf.set(id, row));
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

	// Assign positions, vertically centering each column around a shared axis so
	// the whole graph reads balanced rather than top-anchored.
	const tallest = Math.max(...columns.map((col) => col.length));
	const pos = new Map<string, { x: number; y: number }>();
	columns.forEach((col, c) => {
		const offset = ((tallest - col.length) * ROW_GAP) / 2;
		col.forEach((id, row) => {
			pos.set(id, { x: c * COL_GAP, y: offset + row * ROW_GAP });
		});
	});

	return {
		...graph,
		nodes: nodes.map((n) => {
			const p = pos.get(n.id);
			return p ? { ...n, positionX: p.x, positionY: p.y } : n;
		}),
	};
}
