import { useEffect, useRef } from "react";
import type { Group } from "three";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { wasPointerDragged } from "../lib/orbit-state";
import { registerPieceGroup, unregisterPieceGroup } from "../lib/piece-refs";
import type { StagePieceNode as StageTreeNode } from "../lib/tree";
import { useStagePieceStore } from "../stores/use-stage-piece-store";
import { StagePieceObject } from "./stage-piece-object";

type TransformMode = "translate" | "rotate";

interface StagePieceNodeProps {
	node: StageTreeNode;
	enableEditing: boolean;
	transformMode: TransformMode;
	primaryId: string | null;
	/** All pieces in the same cluster as the primary selection. */
	selectedClusterIds: Set<string> | null;
	/** All pieces in the same cluster as the hovered piece. */
	hoveredClusterIds: Set<string> | null;
}

/**
 * Recursive wrapper for one stage piece. The group carries the piece's
 * (parent-local) transform and contains both its GLB content and the
 * groups of any attached children, so moving an ancestor cascades to
 * descendants via the scene graph.
 *
 * The transform gizmo is **not** rendered here — see `UnifiedTransform`
 * in the visualizer, which mounts once at the canvas root and targets
 * cluster roots / fixtures via their registered group refs. Putting it
 * here would nest it inside an ancestor's group and double-apply that
 * ancestor's transform.
 */
export function StagePieceNode({
	node,
	enableEditing,
	transformMode,
	primaryId,
	selectedClusterIds,
	hoveredClusterIds,
}: StagePieceNodeProps) {
	const { piece, children } = node;

	const selectPieceById = useStagePieceStore((s) => s.selectPieceById);
	const setHoveredId = useStagePieceStore((s) => s.setHoveredId);
	const armedMeshPath = useStagePieceStore((s) => s.armedMeshPath);
	const commitPlace = useStagePieceStore((s) => s.commitPlace);

	const groupRef = useRef<Group | null>(null);

	const setGroupRef = (group: Group | null) => {
		groupRef.current = group;
		if (group) {
			registerPieceGroup(piece.id, group);
		} else {
			unregisterPieceGroup(piece.id);
		}
	};

	useEffect(() => {
		return () => unregisterPieceGroup(piece.id);
	}, [piece.id]);

	const isPrimary = primaryId === piece.id;
	const inSelectedCluster = selectedClusterIds?.has(piece.id) ?? false;
	const inHoveredCluster = hoveredClusterIds?.has(piece.id) ?? false;

	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: 3D object
		<group
			ref={setGroupRef}
			position={[piece.posX, piece.posZ, piece.posY]}
			rotation={[piece.rotX, piece.rotZ, piece.rotY]}
			scale={piece.scale}
			onClick={(e) => {
				if (!enableEditing) return;
				// Camera orbit drag captures the pointer; R3F's own
				// click-vs-drag check doesn't see the intermediate moves
				// and still fires `click` on mouseup. Our window-level
				// drag tracker catches it.
				if (wasPointerDragged()) return;
				e.stopPropagation();
				if (armedMeshPath) {
					commitPlace();
					return;
				}
				const shift = (e.nativeEvent as PointerEvent).shiftKey;
				selectPieceById(piece.id, { shift });
				// Cross-type clear: a non-shift click on a stage piece
				// also drops any fixture selection (and vice versa in
				// fixture-object.tsx). Shift-click preserves both.
				if (!shift) useFixtureStore.getState().clearSelection();
			}}
			onPointerOver={(e) => {
				if (!enableEditing) return;
				e.stopPropagation();
				setHoveredId(piece.id);
			}}
			onPointerOut={(e) => {
				if (!enableEditing) return;
				e.stopPropagation();
				// Functional clear: only reset if we're still the one hovered.
				// Avoids flicker when the pointer moves directly from one
				// piece to another (their `over` and `out` race).
				const current = useStagePieceStore.getState().hoveredId;
				if (current === piece.id) setHoveredId(null);
			}}
		>
			<StagePieceObject
				id={piece.id}
				meshPath={piece.meshPath}
				enableEditing={enableEditing}
				isPrimary={isPrimary}
				inSelectedCluster={inSelectedCluster}
				inHoveredCluster={inHoveredCluster}
			/>

			{children.map((child) => (
				<StagePieceNode
					key={child.piece.id}
					node={child}
					enableEditing={enableEditing}
					transformMode={transformMode}
					primaryId={primaryId}
					selectedClusterIds={selectedClusterIds}
					hoveredClusterIds={hoveredClusterIds}
				/>
			))}
		</group>
	);
}
