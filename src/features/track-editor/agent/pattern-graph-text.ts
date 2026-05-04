import type { Edge, Graph, NodeInstance } from "@/bindings/schema";

/**
 * Render a pattern's node graph as compact text suitable for an LLM context.
 *
 * Output shape:
 *   args:
 *     - color (Color) default {r:255,g:0,b:0,a:1}
 *   nodes:
 *     n_abc12  Oscillator        params {freq:2}
 *     n_def34  ColorOut          params {}
 *   edges:
 *     n_abc12.value  ->  n_def34.intensity
 */
export function patternGraphToText(graphJson: string): string {
	let graph: Graph;
	try {
		graph = JSON.parse(graphJson) as Graph;
	} catch (err) {
		return `<failed to parse graph: ${String(err)}>`;
	}

	const nodes = graph.nodes ?? [];
	const edges = graph.edges ?? [];
	const args = graph.args ?? [];

	const lines: string[] = [];

	if (args.length > 0) {
		lines.push("args:");
		for (const arg of args) {
			const def =
				arg.defaultValue && Object.keys(arg.defaultValue).length > 0
					? ` default ${stringifyShort(arg.defaultValue)}`
					: "";
			lines.push(`  - ${arg.name} (${arg.argType})${def}`);
		}
	}

	if (nodes.length === 0) {
		lines.push("nodes: <empty graph>");
		return lines.join("\n");
	}

	const idLabels = shortIdMap(nodes);
	const nameWidth = Math.min(
		24,
		nodes.reduce((m, n) => Math.max(m, n.typeId.length), 0),
	);

	lines.push("nodes:");
	for (const n of nodes) {
		const lbl = idLabels.get(n.id) ?? n.id;
		const typ = n.typeId.padEnd(nameWidth);
		const params =
			n.params && Object.keys(n.params).length > 0
				? ` params ${stringifyShort(n.params)}`
				: "";
		lines.push(`  ${lbl}  ${typ}${params}`);
	}

	if (edges.length > 0) {
		lines.push("edges:");
		for (const e of edges) {
			const from = idLabels.get(e.fromNode) ?? e.fromNode;
			const to = idLabels.get(e.toNode) ?? e.toNode;
			lines.push(`  ${from}.${e.fromPort}  ->  ${to}.${e.toPort}`);
		}
	}

	return lines.join("\n");
}

/** Build readable short ids like n1, n2, n3 keyed by full node id. */
function shortIdMap(nodes: NodeInstance[]): Map<string, string> {
	const map = new Map<string, string>();
	for (let i = 0; i < nodes.length; i++) {
		map.set(nodes[i].id, `n${i + 1}`);
	}
	return map;
}

/** JSON.stringify but with minimal whitespace and capped length. */
function stringifyShort(value: unknown, maxLen = 80): string {
	let s: string;
	try {
		s = JSON.stringify(value);
	} catch {
		s = String(value);
	}
	return s.length > maxLen ? `${s.slice(0, maxLen - 1)}…` : s;
}

/** Helper to produce textual edge map for unused detection (not used today). */
export type EdgeIndex = Map<string, Edge[]>;
