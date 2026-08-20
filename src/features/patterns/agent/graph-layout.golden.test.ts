import { describe, expect, it } from "vitest";
import type {
	Edge,
	Graph,
	NodeInstance,
	PatternArgType,
} from "@/bindings/schema";
import goldens from "../../../../harness/goldens/patterns-agent-graph-layout.json";
import { layoutGraph } from "./graph-layout";

/**
 * Golden-vector characterization test for `layoutGraph`.
 *
 * The module's contract is *determinism*: the same topology must always yield
 * the same positions, so param-only edits don't shuffle the canvas. Three
 * order-sensitive algorithms compose to produce that — longest-path depth via
 * memoized DFS with a visiting-set cycle guard (returns 0 on a back edge, so a
 * cyclic graph's columns depend on node iteration order), two rounds of
 * barycenter crossing-reduction with an explicit index tie-break, and per-column
 * vertical centering against the tallest column using estimated node heights.
 * A reimplementation reproduces all three only by preserving the graph's node
 * array order as the seed order; these vectors are what catches it if it doesn't.
 *
 * Coordinates are exact: every position is a sum/half-sum of integer constants,
 * so equality is asserted with a 1e-9 tolerance purely to be robust to a future
 * float-valued height, not because drift is expected.
 *
 * ── How the goldens were produced ──────────────────────────────────────────
 * A throwaway script (bun) built the same `cases` table below — the handcrafted
 * fixtures plus 35 DAGs from a mulberry32 PRNG seeded with 0x5eed1234, whose
 * edges only ever run from a lower- to a higher-index node so acyclicity is
 * guaranteed by construction — ran each through `expand()` + `layoutGraph()`,
 * and wrote `{ case, input, output }` to
 * `harness/goldens/patterns-agent-graph-layout.json`, where `output` is the
 * `(id, positionX, positionY)` triple of each node in node-array order.
 * The generator is intentionally not runnable from here: this test only reads.
 * To re-record, port the `cases` table into a script and re-run it — and treat
 * any diff as a behavior change to justify, not to rubber-stamp.
 */

type CaseInput = {
	nodes: Array<{ id: string; typeId: string }>;
	edges: Array<{ fromNode: string; toNode: string }>;
	argCount: number;
};

type GoldenCase = {
	case: string;
	input: CaseInput;
	output: Array<{
		id: string;
		positionX: number | null;
		positionY: number | null;
	}>;
};

/** Expands the compact golden input into the real `Graph` shape. */
function expand(input: CaseInput): Graph {
	return {
		nodes: input.nodes.map(
			(n): NodeInstance => ({
				id: n.id,
				typeId: n.typeId,
				params: {},
				positionX: null,
				positionY: null,
			}),
		),
		edges: input.edges.map(
			(e, i): Edge => ({
				id: `e${i}`,
				fromNode: e.fromNode,
				fromPort: "out",
				toNode: e.toNode,
				toPort: "in",
			}),
		),
		args: Array.from({ length: input.argCount }, (_, i) => ({
			id: `a${i}`,
			name: `arg${i}`,
			argType: "float" as PatternArgType,
			defaultValue: {},
		})),
	};
}

const positionsOf = (graph: Graph) =>
	graph.nodes.map((n) => ({
		id: n.id,
		positionX: n.positionX,
		positionY: n.positionY,
	}));

const cases = goldens as GoldenCase[];

describe("layoutGraph golden vectors", () => {
	it("covers a meaningful number of cases", () => {
		expect(cases.length).toBeGreaterThanOrEqual(15);
		expect(cases.length).toBeLessThanOrEqual(60);
		expect(new Set(cases.map((c) => c.case)).size).toBe(cases.length);
	});

	for (const c of cases) {
		it(`matches the recorded layout: ${c.case}`, () => {
			const actual = positionsOf(layoutGraph(expand(c.input)));
			expect(actual.map((p) => p.id)).toEqual(c.output.map((p) => p.id));
			actual.forEach((p, i) => {
				const want = c.output[i];
				expect(p.positionX).toBeCloseTo(want.positionX as number, 9);
				expect(p.positionY).toBeCloseTo(want.positionY as number, 9);
			});
		});
	}
});

describe("layoutGraph determinism", () => {
	for (const c of cases) {
		it(`is idempotent and clone-stable: ${c.case}`, () => {
			const first = layoutGraph(expand(c.input));
			// Re-laying out its own output must not move anything: existing
			// positions are inputs the algorithm is required to ignore.
			expect(positionsOf(layoutGraph(first))).toEqual(positionsOf(first));
			// A deep clone that preserves node-array order must land identically —
			// nothing may depend on object identity or Map/Set insertion accidents
			// beyond the node order itself.
			const clone = structuredClone(expand(c.input));
			expect(positionsOf(layoutGraph(clone))).toEqual(positionsOf(first));
			// Repeating the same call yields the same answer.
			expect(positionsOf(layoutGraph(expand(c.input)))).toEqual(
				positionsOf(first),
			);
		});
	}
});
