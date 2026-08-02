import { describe, expect, it, vi } from "vitest";
import type { Graph } from "@/bindings/schema";
import { checkpointGraphDocument, graphFingerprint } from "./graph-checkpoint";

const candidate: Graph = {
	nodes: [
		{
			id: "z",
			typeId: "constant",
			params: { value: 1, alpha: 0.5 },
			positionX: 20,
			positionY: 10,
		},
		{
			id: "a",
			typeId: "view_signal",
			params: {},
			positionX: 40,
			positionY: 10,
		},
	],
	edges: [
		{
			id: "editor-only-id",
			fromNode: "z",
			fromPort: "out",
			toNode: "a",
			toPort: "signal",
		},
	],
	args: [],
};

const canonicalRoundTrip: Graph = {
	...candidate,
	nodes: [
		candidate.nodes[1],
		{
			...candidate.nodes[0],
			params: { alpha: 0.5, value: 1 },
		},
	],
	edges: [{ ...candidate.edges[0], id: "z:out->a:signal" }],
};

describe("graph checkpoint", () => {
	it("fingerprints semantic graphs across host canonicalization", () => {
		expect(graphFingerprint(candidate)).toBe(
			graphFingerprint(canonicalRoundTrip),
		);
	});

	it("recovers a committed save whose IPC response was lost", async () => {
		const responseLost = new Error("response lost");
		const newerAuthoritativeGraph = structuredClone(canonicalRoundTrip);
		newerAuthoritativeGraph.nodes[0] = {
			...newerAuthoritativeGraph.nodes[0],
			positionX: 999,
		};
		const save = vi
			.fn()
			.mockRejectedValueOnce(responseLost)
			.mockResolvedValueOnce({
				revision: "committed-revision",
				graph: newerAuthoritativeGraph,
				changed: true,
			});
		const accept = vi.fn();

		await expect(
			checkpointGraphDocument({
				patternId: "pattern-1",
				implementationId: "implementation-1",
				baseRevision: "base-revision",
				graph: candidate,
				save,
				accept,
			}),
		).resolves.toEqual({
			revision: "committed-revision",
			graph: newerAuthoritativeGraph,
			changed: true,
		});
		expect(save).toHaveBeenCalledTimes(2);
		expect(save.mock.calls[1]).toEqual(save.mock.calls[0]);
		expect(save.mock.calls[0][0].operationId).toEqual(expect.any(String));
		expect(accept).toHaveBeenCalledWith({
			revision: "committed-revision",
			graph: newerAuthoritativeGraph,
			changed: true,
		});
	});

	it("preserves a real conflict when the authoritative graph differs", async () => {
		const conflict = new Error("revision conflict");

		const save = vi.fn(async () => {
			throw conflict;
		});
		const accept = vi.fn();
		await expect(
			checkpointGraphDocument({
				patternId: "pattern-1",
				implementationId: "implementation-1",
				baseRevision: "base-revision",
				graph: candidate,
				save,
				accept,
			}),
		).rejects.toBe(conflict);
		expect(save).toHaveBeenCalledTimes(2);
		expect(save.mock.calls[1]).toEqual(save.mock.calls[0]);
		expect(accept).not.toHaveBeenCalled();
	});
});
