import { describe, expect, it } from "vitest";
import type { ResolvedNode } from "@/bindings/venue-graph";
import { structureNodes } from "./use-venue-store";

function node(over: Partial<ResolvedNode>): ResolvedNode {
	return {
		id: "n",
		kind: "piece",
		catalogRef: "truss/straight",
		label: null,
		parentId: "venue",
		position: [0, 0, 0],
		rotation: [0, 0, 0],
		facing: [0, 0, -1],
		arrayIndex: null,
		setPiece: true,
		params: {},
		...over,
	};
}

describe("structureNodes", () => {
	/**
	 * The solve emits an array as an anchor plus N members, and the anchor
	 * carries the members' `catalogRef` — so a list built from `kind` and
	 * `catalogRef` draws N+1 meshes, the extra one inside the middle member.
	 * `setPiece` is the resolver's own answer and the only thing this filters
	 * on.
	 */
	it("draws one mesh per array member and none for the anchor", () => {
		const nodes = [
			node({
				id: "venue",
				kind: "venue",
				catalogRef: null,
				parentId: null,
				setPiece: false,
			}),
			node({ id: "wall", kind: "array", parentId: "venue", setPiece: false }),
			...[0, 1, 2].map((i) =>
				node({
					id: `wall#${i}`,
					kind: "array",
					parentId: "wall",
					arrayIndex: i,
				}),
			),
			node({
				id: "mover",
				kind: "fixture",
				catalogRef: "fix-1",
				setPiece: false,
			}),
		];
		expect(structureNodes(nodes).map((n) => n.id)).toEqual([
			"wall#0",
			"wall#1",
			"wall#2",
		]);
	});
});
