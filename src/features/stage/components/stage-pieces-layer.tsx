import { Suspense, useMemo } from "react";
import { buildTree, clusterMembersOf } from "../lib/tree";
import { useStagePieceStore } from "../stores/use-stage-piece-store";
import { StageGhost } from "./stage-ghost";
import { StagePieceNode } from "./stage-piece-node";

type TransformMode = "translate" | "rotate";

interface StagePiecesLayerProps {
	enableEditing: boolean;
	transformMode: TransformMode;
}

/**
 * Renders the stage piece tree, hover/selection outline data, and the
 * placement ghost. The transform gizmo is mounted separately at the
 * visualizer level (`UnifiedTransform`) so it can operate across both
 * fixtures and stage pieces.
 */
export function StagePiecesLayer({
	enableEditing,
	transformMode,
}: StagePiecesLayerProps) {
	const pieces = useStagePieceStore((s) => s.pieces);
	const selectedIds = useStagePieceStore((s) => s.selectedIds);
	const lastSelectedId = useStagePieceStore((s) => s.lastSelectedId);
	const hoveredId = useStagePieceStore((s) => s.hoveredId);

	const roots = useMemo(() => buildTree(pieces), [pieces]);

	const selectedClusterIds = useMemo(() => {
		if (selectedIds.size === 0) return null;
		const out = new Set<string>();
		for (const id of selectedIds) {
			for (const m of clusterMembersOf(pieces, id)) out.add(m);
		}
		return out;
	}, [selectedIds, pieces]);

	const hoveredClusterIds = useMemo(
		() => (hoveredId ? clusterMembersOf(pieces, hoveredId) : null),
		[hoveredId, pieces],
	);

	return (
		<Suspense fallback={null}>
			{roots.map((root) => (
				<StagePieceNode
					key={root.piece.id}
					node={root}
					enableEditing={enableEditing}
					transformMode={transformMode}
					primaryId={lastSelectedId}
					selectedClusterIds={selectedClusterIds}
					hoveredClusterIds={hoveredClusterIds}
				/>
			))}
			<StageGhost />
		</Suspense>
	);
}
