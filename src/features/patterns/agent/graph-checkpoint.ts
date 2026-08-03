import type { Edge, Graph, GraphEditResult } from "@/bindings/schema";

export type GraphCheckpointResult = GraphEditResult;

type CheckpointGraphDocumentArgs = {
	patternId: string;
	implementationId: string;
	baseRevision: string;
	graph: Graph;
	save: (input: {
		id: string;
		implementationId: string;
		operationId: string;
		baseRevision: string;
		graph: Graph;
	}) => Promise<GraphCheckpointResult>;
	accept: (result: GraphCheckpointResult) => void;
};

function canonicalEdgeId(edge: Edge): string {
	return `${edge.fromNode}:${edge.fromPort}->${edge.toNode}:${edge.toPort}`;
}

function compareText(left: string, right: string): number {
	return left < right ? -1 : left > right ? 1 : 0;
}

function canonicalJsonValue(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(canonicalJsonValue);
	if (value === null || typeof value !== "object") return value;
	return Object.fromEntries(
		Object.entries(value as Record<string, unknown>)
			.sort(([left], [right]) => compareText(left, right))
			.map(([key, child]) => [key, canonicalJsonValue(child)]),
	);
}

/** Mirror the host's semantic graph ordering closely enough to compare an
 * editor candidate with a graph that has round-tripped through Rust. Object-key
 * order and host-owned edge ids are not authored differences. */
export function graphFingerprint(graph: Graph): string {
	const nodes = [...graph.nodes].sort((left, right) =>
		compareText(left.id, right.id),
	);
	const args = [...graph.args].sort((left, right) =>
		compareText(left.id, right.id),
	);
	const edges = graph.edges
		.map((edge) => ({ ...edge, id: canonicalEdgeId(edge) }))
		.sort(
			(left, right) =>
				compareText(left.toNode, right.toNode) ||
				compareText(left.toPort, right.toPort) ||
				compareText(left.fromNode, right.fromNode) ||
				compareText(left.fromPort, right.fromPort),
		);
	return JSON.stringify(canonicalJsonValue({ nodes, edges, args }));
}

/** Save a complete graph with optimistic concurrency. One stable operation ID
 * spans both IPC attempts, so the host can replay the durable revision outcome
 * instead of guessing from a later graph snapshot. */
export async function checkpointGraphDocument({
	patternId,
	implementationId,
	baseRevision,
	graph,
	save,
	accept,
}: CheckpointGraphDocumentArgs): Promise<GraphCheckpointResult> {
	const request = {
		id: patternId,
		implementationId,
		operationId: crypto.randomUUID(),
		baseRevision,
		graph,
	};
	let result: GraphCheckpointResult;
	try {
		result = await save(request);
	} catch {
		result = await save(request);
	}
	accept(result);
	return result;
}
