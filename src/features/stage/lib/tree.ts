/**
 * Build a parent → children tree from the flat stage_pieces list. Used by
 * the renderer to nest each piece's three.js group inside its parent's,
 * so attached pieces inherit transforms via the scene graph.
 */

import type { StagePiece } from "@/bindings/stage";

export interface StagePieceNode {
	piece: StagePiece;
	children: StagePieceNode[];
}

export function buildTree(pieces: StagePiece[]): StagePieceNode[] {
	const byId = new Map<string, StagePieceNode>();
	for (const p of pieces) byId.set(p.id, { piece: p, children: [] });

	const roots: StagePieceNode[] = [];
	for (const node of byId.values()) {
		const parentId = node.piece.parentPieceId;
		if (
			parentId &&
			byId.has(parentId) &&
			!createsCycle(node.piece.id, parentId, byId)
		) {
			byId.get(parentId)?.children.push(node);
		} else {
			roots.push(node);
		}
	}
	return roots;
}

/**
 * Return every piece id in the subtree rooted at `rootId` (inclusive).
 * Used by the gizmo to exclude self + descendants from snap candidates —
 * snapping a piece onto its own descendant would create a parent cycle
 * and silently hide both nodes from the rendered tree.
 */
export function descendantIdsOf(
	pieces: StagePiece[],
	rootId: string,
): Set<string> {
	const childrenByParent = childMap(pieces);
	const result = new Set<string>([rootId]);
	const stack = [rootId];
	while (stack.length) {
		const cur = stack.pop();
		if (cur === undefined) break;
		const kids = childrenByParent.get(cur);
		if (!kids) continue;
		for (const kid of kids) {
			if (result.has(kid)) continue;
			result.add(kid);
			stack.push(kid);
		}
	}
	return result;
}

/**
 * Subtree of `id` — the piece itself and every parent_piece_id
 * descendant. This is the set the user perceives as "what moves when I
 * drag this": the clicked piece + everything that cascades through
 * three.js's scene graph.
 *
 * No walking *up* the parent chain. Selection is always the clicked
 * piece. To move a whole stage of decks, the user clicks the root in
 * the hierarchy panel, marquees them, or shift-clicks.
 *
 * Kept as a thin alias over {@link descendantIdsOf} so callers can be
 * explicit about intent ("the cluster that visually moves") even
 * though the implementation is just a downward walk.
 */
export function clusterMembersOf(
	pieces: StagePiece[],
	id: string,
): Set<string> {
	return descendantIdsOf(pieces, id);
}

function childMap(pieces: StagePiece[]): Map<string, string[]> {
	const m = new Map<string, string[]>();
	for (const p of pieces) {
		if (!p.parentPieceId) continue;
		const arr = m.get(p.parentPieceId);
		if (arr) arr.push(p.id);
		else m.set(p.parentPieceId, [p.id]);
	}
	return m;
}

function createsCycle(
	nodeId: string,
	parentId: string,
	byId: Map<string, StagePieceNode>,
): boolean {
	let cur: string | null = parentId;
	const seen = new Set<string>();
	while (cur) {
		if (cur === nodeId) {
			console.error(
				`[stage] cycle detected — treating ${nodeId} as root (parent chain looped back through ${parentId})`,
			);
			return true;
		}
		if (seen.has(cur)) return false; // pre-existing cycle elsewhere; not ours
		seen.add(cur);
		cur = byId.get(cur)?.piece.parentPieceId ?? null;
	}
	return false;
}
