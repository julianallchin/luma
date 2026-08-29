/**
 * Cross-language parity for the stage catalog.
 *
 * The catalog is Rust (`gpui/crates/scene/src/catalog.rs`) and this app reads
 * a generated projection of it. Two things could still go wrong, and this
 * pins both:
 *
 *   1. **The data.** `catalog.generated.ts` could be stale — regenerated on
 *      one side and not committed. Every socket in it is resolved here and
 *      compared to `harness/goldens/stage-catalog.json`, which the same Rust
 *      test emitted from the same catalog in the same run.
 *   2. **The algorithm.** `resolveSocket` here and `resolve_socket` there are
 *      two implementations of one contract. The golden was produced by the
 *      Rust one against a pinned bounding box; this test runs the TypeScript
 *      one against the same box.
 *
 * A real GLB is deliberately not loaded: the bbox is an *input* to both
 * sides, so pinning one keeps the comparison about the code under test rather
 * than about two glTF loaders disagreeing by a micron.
 *
 * If this fails, the fix is `cargo test -p luma-scene` and committing what it
 * rewrites — never editing the generated file.
 */

import { Box3, Vector3 } from "three";
import { describe, expect, it } from "vitest";
import golden from "@/../harness/goldens/stage-catalog.json";
import { CATALOG, SOCKET_TYPES } from "../catalog.generated";
import { resolveSocket, type SocketType, socketsMate } from "../sockets";

const PRECISION = 9;

const bbox = new Box3(
	new Vector3(...(golden.bbox[0] as [number, number, number])),
	new Vector3(...(golden.bbox[1] as [number, number, number])),
);

function expectVec(actual: Vector3, expected: number[], label: string) {
	const got = [actual.x, actual.y, actual.z];
	for (let i = 0; i < 3; i++) {
		expect(got[i], `${label}[${i}]`).toBeCloseTo(expected[i], PRECISION);
	}
}

describe("catalog parity with Rust", () => {
	it("covers every piece with authored sockets", () => {
		const authored = CATALOG.filter((p) => p.geometry.kind === "mesh");
		expect(golden.pieces.map((p) => p.id)).toEqual(authored.map((p) => p.id));
		expect(authored.length).toBeGreaterThan(0);
	});

	for (const want of golden.pieces) {
		it(want.id, () => {
			const piece = CATALOG.find((p) => p.id === want.id);
			expect(
				piece,
				`${want.id} missing from the generated catalog`,
			).toBeDefined();
			if (!piece) return;
			expect(piece.sockets.map((s) => s.name)).toEqual(
				want.sockets.map((s) => s.name),
			);
			for (const [i, def] of piece.sockets.entries()) {
				const got = resolveSocket(def, bbox);
				const expected = want.sockets[i];
				const label = `${want.id}/${expected.name}`;
				expect(got.type, label).toBe(expected.type);
				expect(got.mode, label).toBe(expected.mode);
				expect(got.roll, label).toEqual(expected.roll);
				expectVec(got.position, expected.position, `${label}.position`);
				expectVec(got.normal, expected.normal, `${label}.normal`);
				expectVec(got.tangent, expected.tangent, `${label}.tangent`);
				expectVec(got.outward, expected.outward, `${label}.outward`);
			}
		});
	}

	/**
	 * The polarity rule, pair by pair. The golden lists every ordered pair the
	 * Rust rule admits; anything this side admits or refuses differently is a
	 * divergence, not a formatting difference.
	 */
	it("mates exactly the pairs Rust does", () => {
		const want = new Set(golden.mates.map(([held, host]) => `${held}>${host}`));
		const got = new Set<string>();
		for (const held of SOCKET_TYPES) {
			for (const host of SOCKET_TYPES) {
				if (socketsMate(held as SocketType, host as SocketType)) {
					got.add(`${held}>${host}`);
				}
			}
		}
		expect([...got].sort()).toEqual([...want].sort());
	});
});
