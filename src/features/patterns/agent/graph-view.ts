import type { Edge, Graph, NodeInstance, NodeTypeDef } from "@/bindings/schema";

/**
 * Render a node graph as the compact, id-addressed "GRAPH VIEW" the agent reads.
 *
 * Node ids are already human-readable (`apply_color_1`) and double as the handle
 * the agent uses in every edit tool — there's no separate alias namespace.
 *
 *   scalar_1       Scalar(value=0.7)             -> apply_color_1.intensity
 *   palette_1      SamplePalette(source=sunset)  -> apply_color_1.palette
 *   apply_color_1  ApplyColor(mode=gradient)     -> view_signal_1.in
 *   view_signal_1  ViewSignal(group=front_wash)  -> [out]
 *
 * Format: <id>  <Type>(<params>)  -> <target_id>.<inport>[, …]
 *   - one target per arrow segment; nodes that fan out to many targets wrap to
 *     one target per line.
 *   - `[out]` marks a sink (a viewer or any node whose output nothing consumes).
 *   - when a node has more than one output port, segments are prefixed with the
 *     source port: `out_port -> target.in_port`.
 */
export function renderGraphView(graph: Graph, defs: NodeTypeDef[]): string {
	const nodes = graph.nodes ?? [];
	if (nodes.length === 0) return "<empty graph>";

	const defMap = new Map(defs.map((d) => [d.id, d]));
	const edges = graph.edges ?? [];
	const outByNode = groupOutgoing(edges);

	const idWidth = Math.min(
		28,
		nodes.reduce((m, n) => Math.max(m, n.id.length), 0),
	);

	const lines: string[] = [];
	for (const node of nodes) {
		const def = defMap.get(node.typeId);
		const typeName = formatTypeName(node, def);
		const params = formatParams(node.params);
		const head = `${node.id.padEnd(idWidth)}  ${typeName}${params}`;

		const multiOut = (def?.outputs.length ?? 0) > 1;
		const targets = (outByNode.get(node.id) ?? []).map((e) =>
			multiOut
				? `${e.fromPort} -> ${e.toNode}.${e.toPort}`
				: `${e.toNode}.${e.toPort}`,
		);

		if (targets.length === 0) {
			lines.push(`${head}  -> [out]`);
		} else if (targets.length <= 2) {
			lines.push(`${head}  -> ${targets.join(", ")}`);
		} else {
			// Fan-out: one target per line, aligned under the arrow.
			lines.push(`${head}  -> ${targets[0]}`);
			const pad = " ".repeat(idWidth + 2 + 3 + 1);
			for (const t of targets.slice(1)) lines.push(`${pad}${t}`);
		}
	}

	return lines.join("\n");
}

/** Neighborhood of `nodeId` out to `depth` hops (edges traversed both ways),
 * rendered in the same format. Used for big graphs so the agent can zoom in. */
export function renderSubgraph(
	graph: Graph,
	defs: NodeTypeDef[],
	nodeId: string,
	depth: number,
): string {
	const nodes = graph.nodes ?? [];
	if (!nodes.some((n) => n.id === nodeId)) {
		return `<unknown node id: ${nodeId}>`;
	}
	const edges = graph.edges ?? [];
	const adjacency = new Map<string, Set<string>>();
	const neighbors = (id: string): Set<string> => {
		let set = adjacency.get(id);
		if (!set) {
			set = new Set();
			adjacency.set(id, set);
		}
		return set;
	};
	for (const e of edges) {
		neighbors(e.fromNode).add(e.toNode);
		neighbors(e.toNode).add(e.fromNode);
	}

	const keep = new Set<string>([nodeId]);
	let frontier = [nodeId];
	for (let d = 0; d < depth; d++) {
		const next: string[] = [];
		for (const id of frontier) {
			for (const neighbor of adjacency.get(id) ?? []) {
				if (!keep.has(neighbor)) {
					keep.add(neighbor);
					next.push(neighbor);
				}
			}
		}
		frontier = next;
	}

	const sub: Graph = {
		nodes: nodes.filter((n) => keep.has(n.id)),
		edges: edges.filter((e) => keep.has(e.fromNode) && keep.has(e.toNode)),
		args: graph.args,
	};
	return renderGraphView(sub, defs);
}

/** Compact catalog of every node type: ports (with types) and params. */
export function renderTypeCatalog(defs: NodeTypeDef[]): string {
	const byCategory = new Map<string, NodeTypeDef[]>();
	for (const d of defs) {
		const cat = d.category ?? "Other";
		const group = byCategory.get(cat) ?? [];
		if (group.length === 0) byCategory.set(cat, group);
		group.push(d);
	}
	const lines: string[] = [];
	for (const [cat, group] of [...byCategory.entries()].sort()) {
		lines.push(`## ${cat}`);
		for (const d of group.sort((a, b) => a.id.localeCompare(b.id))) {
			const ins = d.inputs.map((p) => `${p.id}:${p.portType}`).join(" ") || "—";
			const outs =
				d.outputs.map((p) => `${p.id}:${p.portType}`).join(" ") || "—";
			const params =
				d.params
					.map((p) => {
						const def =
							p.paramType === "Number"
								? (p.defaultNumber ?? 0)
								: JSON.stringify(p.defaultText ?? "");
						return `${p.id}=${def}`;
					})
					.join(" ") || "—";
			lines.push(`  ${d.id}`);
			lines.push(`    in:  ${ins}`);
			lines.push(`    out: ${outs}`);
			lines.push(`    params: ${params}`);
		}
	}
	return lines.join("\n");
}

function groupOutgoing(edges: Edge[]): Map<string, Edge[]> {
	const map = new Map<string, Edge[]>();
	for (const e of edges) {
		const list = map.get(e.fromNode) ?? [];
		if (list.length === 0) map.set(e.fromNode, list);
		list.push(e);
	}
	return map;
}

/** PascalCase-ish type label: prefer the def name with spaces stripped, else
 * the raw typeId. */
function formatTypeName(
	node: NodeInstance,
	def: NodeTypeDef | undefined,
): string {
	if (def?.name) return def.name.replace(/\s+/g, "");
	return node.typeId;
}

function formatParams(params: Record<string, unknown> | undefined): string {
	if (!params) return "()";
	const entries = Object.entries(params).filter(([, v]) => v !== undefined);
	if (entries.length === 0) return "()";
	const inner = entries.map(([k, v]) => `${k}=${formatValue(v)}`).join(", ");
	return `(${inner})`;
}

function formatValue(value: unknown, maxLen = 48): string {
	let s: string;
	if (typeof value === "string") s = value;
	else {
		try {
			s = JSON.stringify(value);
		} catch {
			s = String(value);
		}
	}
	return s.length > maxLen ? `${s.slice(0, maxLen - 1)}…` : s;
}
