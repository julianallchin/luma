import { TransformControls } from "@react-three/drei";
import { useCallback, useEffect, useMemo, useRef } from "react";
import {
	type Group,
	MathUtils,
	Matrix3,
	Matrix4,
	type Object3D,
	Quaternion,
	Raycaster,
	Vector3,
} from "three";
import type { StagePiece } from "@/bindings/stage";
import {
	getMeshGeometry,
	getMeshSockets,
} from "@/features/stage/lib/mesh-cache";
import { getPieceGroup } from "@/features/stage/lib/piece-refs";
import {
	type SnapResult,
	type SnapSurface,
	solveSnap,
} from "@/features/stage/lib/snap";
import { getStageMesh } from "@/features/stage/lib/stage-meshes";
import { descendantIdsOf } from "@/features/stage/lib/tree";
import { useStagePieceStore } from "@/features/stage/stores/use-stage-piece-store";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { getFixtureGroup } from "../lib/fixture-refs";

type TransformMode = "translate" | "rotate";
type TransformPivot = "individual" | "group";

interface UnifiedTransformProps {
	enableEditing: boolean;
	transformMode: TransformMode;
	transformPivot: TransformPivot;
}

interface TargetSnapshot {
	kind: "fixture" | "stage-root";
	id: string;
	group: Group;
	/** Group's world position at drag start (= GLB origin in world). */
	startWorldPos: Vector3;
	startWorldQ: Quaternion;
	/**
	 * Per-target rotation anchor in world space. For stage pieces this is
	 * the bbox bottom-center (so rotating spins around the middle of the
	 * piece, not its GLB-corner origin). For fixtures it equals
	 * `startWorldPos` since their origin is already centered.
	 */
	startAnchor: Vector3;
}

/**
 * One gizmo to rule them all. Renders a single TransformControls widget
 * that operates on the union of selected fixtures + selected stage
 * pieces.
 *
 * Selection model:
 *   - Each selected fixture is its own controlled target.
 *   - Each selected stage piece is its own controlled target. We *don't*
 *     walk up parent_piece_id — clicking a piece selects that piece,
 *     and its scene-graph descendants come along for free via cascade.
 *     To move a whole snapped stage, the user clicks the root in the
 *     hierarchy panel, marquees the decks, or shift-clicks them.
 *   - To avoid double-shifting, if both a piece and one of its
 *     ancestors are selected, the descendant is dropped from the
 *     target list — the ancestor's drag covers it via cascade.
 *
 * Translate behavior:
 *   - Multi-target: rigid delta translate of every target. No snap.
 *   - Single stage target: snap solver runs every frame. If the user
 *     drags the piece off its snap point, the solver returns a free /
 *     ground / surface result and `reparentAndMove` detaches /
 *     reattaches accordingly. Hold Shift on the held piece to disable
 *     snap and place freely.
 *
 * Rotate behavior (no snap):
 *   - `individual` pivot — each target rotates around its own anchor
 *     (snap point for parented riders, bbox bottom-center otherwise).
 *   - `group` pivot — every target orbits around the gizmo pivot.
 *
 * The pivot widget itself sits at the primary's anchor (single
 * selection) or the centroid of all selected anchors (multi).
 */
export function UnifiedTransform({
	enableEditing,
	transformMode,
	transformPivot,
}: UnifiedTransformProps) {
	const fixtureIds = useFixtureStore((s) => s.selectedPatchedIds);
	const primaryFixtureId = useFixtureStore((s) => s.lastSelectedPatchedId);
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);
	const moveFixtureSpatial = useFixtureStore((s) => s.moveFixtureSpatial);

	const stagePieceIds = useStagePieceStore((s) => s.selectedIds);
	const primaryStageId = useStagePieceStore((s) => s.lastSelectedId);
	const allPieces = useStagePieceStore((s) => s.pieces);
	const movePieceWorld = useStagePieceStore((s) => s.movePieceWorld);
	const reparentAndMove = useStagePieceStore((s) => s.reparentAndMove);

	const shiftHeld = useRef(false);

	const pivotRef = useRef<Group>(null);
	const isDragging = useRef(false);
	const dragPivotMode = useRef<TransformPivot>("individual");
	const dragTargets = useRef<TargetSnapshot[]>([]);
	const startPivotPos = useRef(new Vector3());
	const startPivotQ = useRef(new Quaternion());
	const lastSnap = useRef<SnapResult | null>(null);

	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			shiftHeld.current = e.shiftKey;
		};
		window.addEventListener("keydown", onKey);
		window.addEventListener("keyup", onKey);
		return () => {
			window.removeEventListener("keydown", onKey);
			window.removeEventListener("keyup", onKey);
		};
	}, []);

	const primaryStagePiece = primaryStageId
		? allPieces.find((p) => p.id === primaryStageId)
		: undefined;
	const primaryFixture = primaryFixtureId
		? patchedFixtures.find((f) => f.id === primaryFixtureId)
		: undefined;

	// Effective stage targets: the user-clicked pieces, minus any whose
	// parent_piece_id chain contains another selected piece. If deck A
	// and a CDJ parented to A are both selected, A's drag cascades to
	// the CDJ via the scene graph; explicitly moving the CDJ too would
	// double-shift it. (The CDJ still visually moves — just through
	// the parent chain.)
	const uniqueStageRootIds = useMemo(() => {
		if (stagePieceIds.size === 0) return [] as string[];
		const byId = new Map(allPieces.map((p) => [p.id, p]));
		const filtered: string[] = [];
		for (const id of stagePieceIds) {
			let isDescendantOfOtherSelection = false;
			let cur = byId.get(id);
			while (cur?.parentPieceId) {
				if (stagePieceIds.has(cur.parentPieceId)) {
					isDescendantOfOtherSelection = true;
					break;
				}
				cur = byId.get(cur.parentPieceId);
			}
			if (!isDescendantOfOtherSelection) filtered.push(id);
		}
		return filtered;
	}, [stagePieceIds, allPieces]);

	const totalSelectedCount = fixtureIds.size + stagePieceIds.size;

	// Park the pivot at its resting position whenever the selection
	// changes (or a fixture/piece is moved by something external). We
	// skip this while a drag is in progress — TransformControls owns
	// the pivot's transform then.
	useEffect(() => {
		if (isDragging.current || !pivotRef.current || totalSelectedCount === 0)
			return;

		const pivot = pivotRef.current;
		const restPos = restingPivotPosition({
			fixtureIds,
			primaryFixture,
			stagePieceIds,
			primaryStagePiece,
			patchedFixtures,
			uniqueStageRootIds,
			allPieces,
		});
		if (!restPos) return;
		pivot.position.copy(restPos);
		pivot.quaternion.set(0, 0, 0, 1);
		pivot.updateMatrixWorld(true);
	}, [
		fixtureIds,
		stagePieceIds,
		primaryFixture,
		primaryStagePiece,
		patchedFixtures,
		uniqueStageRootIds,
		totalSelectedCount,
		allPieces,
	]);

	const onMouseDown = useCallback(() => {
		if (!pivotRef.current || totalSelectedCount === 0) return;

		dragPivotMode.current = transformPivot;
		dragTargets.current = [];

		const piecesById = new Map(allPieces.map((p) => [p.id, p]));

		for (const id of fixtureIds) {
			const g = getFixtureGroup(id);
			if (!g) continue;
			g.updateMatrixWorld(true);
			const startWorldPos = g.getWorldPosition(new Vector3());
			dragTargets.current.push({
				kind: "fixture",
				id,
				group: g,
				startWorldPos,
				startWorldQ: g.getWorldQuaternion(new Quaternion()),
				startAnchor: startWorldPos, // fixtures' origin is centered
			});
		}
		for (const id of uniqueStageRootIds) {
			const g = getPieceGroup(id);
			if (!g) continue;
			g.updateMatrixWorld(true);
			const startWorldPos = g.getWorldPosition(new Vector3());
			const piece = piecesById.get(id);
			const anchor = piece
				? (stagePieceAnchorWorld(piece, allPieces) ?? startWorldPos)
				: startWorldPos;
			dragTargets.current.push({
				kind: "stage-root",
				id,
				group: g,
				startWorldPos,
				startWorldQ: g.getWorldQuaternion(new Quaternion()),
				startAnchor: anchor,
			});
		}

		if (dragTargets.current.length === 0) return;

		startPivotPos.current.copy(pivotRef.current.position);
		startPivotQ.current.copy(pivotRef.current.quaternion);

		isDragging.current = true;
		lastSnap.current = null;
	}, [
		fixtureIds,
		allPieces,
		uniqueStageRootIds,
		transformPivot,
		totalSelectedCount,
	]);

	const onObjectChange = useCallback(() => {
		if (
			!isDragging.current ||
			!pivotRef.current ||
			dragTargets.current.length === 0
		)
			return;

		const pivot = pivotRef.current;
		const deltaPos = pivot.position.clone().sub(startPivotPos.current);
		const deltaQ = pivot.quaternion
			.clone()
			.multiply(startPivotQ.current.clone().invert());

		if (transformMode === "translate") {
			for (const t of dragTargets.current) {
				const desiredPos = t.startWorldPos.clone().add(deltaPos);
				writeWorldToTarget(t.group, desiredPos, t.startWorldQ);
			}
		} else if (dragPivotMode.current === "individual") {
			// Each target rotates around its own anchor (bbox bottom-center
			// for stage pieces, group origin for fixtures). For pieces
			// whose anchor differs from the GLB-origin position, we have
			// to move the origin around the anchor as we spin it.
			for (const t of dragTargets.current) {
				const offset = t.startWorldPos.clone().sub(t.startAnchor);
				offset.applyQuaternion(deltaQ);
				const desiredPos = t.startAnchor.clone().add(offset);
				const desiredQ = deltaQ.clone().multiply(t.startWorldQ);
				writeWorldToTarget(t.group, desiredPos, desiredQ);
			}
		} else {
			// Group mode: every target orbits around the gizmo pivot's
			// drag-start position.
			for (const t of dragTargets.current) {
				const offset = t.startWorldPos.clone().sub(startPivotPos.current);
				offset.applyQuaternion(deltaQ);
				const desiredPos = startPivotPos.current.clone().add(offset);
				const desiredQ = deltaQ.clone().multiply(t.startWorldQ);
				writeWorldToTarget(t.group, desiredPos, desiredQ);
			}
		}

		// Single-stage-target translate: re-snap every frame. This is
		// the only path that runs the snap solver — multi-target drags
		// (multiple pieces, mixed with fixtures, etc.) just translate
		// rigidly. Hold Shift on the held piece to disable snap and
		// place freely.
		if (
			transformMode !== "translate" ||
			dragTargets.current.length !== 1 ||
			dragTargets.current[0].kind !== "stage-root"
		)
			return;

		const target = dragTargets.current[0];
		const piece = allPieces.find((p) => p.id === target.id);
		if (!piece) return;
		target.group.updateMatrixWorld(true);

		const heldSockets = getMeshSockets(piece.meshPath);
		const heldGrab = heldSockets.find((s) => s.type === "grab");
		const cursorWorld = heldGrab
			? heldGrab.position.clone().applyMatrix4(target.group.matrixWorld)
			: target.group.getWorldPosition(new Vector3());
		const currentQ = target.group.getWorldQuaternion(new Quaternion());

		const excludeIds = descendantIdsOf(allPieces, piece.id);
		const groupEntries = allPieces
			.filter((p) => !excludeIds.has(p.id))
			.map((p) => {
				const og = getPieceGroup(p.id);
				if (!og) return null;
				og.updateMatrixWorld(true);
				return { id: p.id, meshPath: p.meshPath, group: og };
			})
			.filter((x): x is NonNullable<typeof x> => x !== null);

		const scenePieces = groupEntries.map((g) => ({
			id: g.id,
			meshPath: g.meshPath,
			worldMatrix: g.group.matrixWorld.clone(),
		}));

		const surface = surfaceUnderPoint(cursorWorld, groupEntries);

		const result = solveSnap({
			heldMeshPath: piece.meshPath,
			cursorWorld,
			currentQuaternion: currentQ,
			pieces: scenePieces,
			excludeId: piece.id,
			shiftHeld: shiftHeld.current,
			surface,
			lookupSockets: getMeshSockets,
		});

		lastSnap.current = result;
		writeWorldToTarget(target.group, result.position, result.quaternion);
	}, [allPieces, transformMode]);

	const onMouseUp = useCallback(() => {
		if (dragTargets.current.length === 0) {
			isDragging.current = false;
			lastSnap.current = null;
			return;
		}

		const wasSingleStageTranslate =
			transformMode === "translate" &&
			dragTargets.current.length === 1 &&
			dragTargets.current[0].kind === "stage-root";

		for (const t of dragTargets.current) {
			t.group.updateMatrixWorld(true);
			const worldPos = t.group.getWorldPosition(new Vector3());
			const worldQ = t.group.getWorldQuaternion(new Quaternion());

			if (t.kind === "fixture") {
				// Y-up (Three.js) → Z-up (data): swap Y↔Z.
				moveFixtureSpatial(
					t.id,
					{ x: worldPos.x, y: worldPos.z, z: worldPos.y },
					{
						x: t.group.rotation.x,
						y: t.group.rotation.z,
						z: t.group.rotation.y,
					},
				);
				continue;
			}

			// Stage-root target.
			if (wasSingleStageTranslate) {
				// Use the snap solver's chosen parent — if the user dragged
				// the piece off its socket, parentId will be null (free)
				// or some other surface, and reparentAndMove detaches /
				// re-attaches accordingly. If the solver fell through to
				// free placement we still use the snap result; it has the
				// same world pose as the dragged group.
				const result = lastSnap.current;
				if (result) {
					reparentAndMove(
						t.id,
						result.parentId,
						result.position,
						result.quaternion,
					);
				} else {
					movePieceWorld(t.id, worldPos, worldQ);
				}
			} else {
				// Multi-target, or single + rotate: persist the new pose
				// and keep the existing parent. The snap solver isn't run
				// in these paths so there's no fresh parent decision.
				movePieceWorld(t.id, worldPos, worldQ);
			}
		}

		isDragging.current = false;
		dragTargets.current = [];
		lastSnap.current = null;

		if (pivotRef.current) {
			pivotRef.current.quaternion.set(0, 0, 0, 1);
		}
	}, [moveFixtureSpatial, movePieceWorld, reparentAndMove, transformMode]);

	if (!enableEditing || totalSelectedCount === 0) return null;

	return (
		<>
			<group ref={pivotRef} />
			<TransformControls
				object={pivotRef as React.RefObject<Group>}
				mode={transformMode}
				size={0.5}
				rotationSnap={
					transformMode === "rotate" ? MathUtils.degToRad(15) : undefined
				}
				onMouseDown={onMouseDown}
				onObjectChange={onObjectChange}
				onMouseUp={onMouseUp}
			/>
		</>
	);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function restingPivotPosition(args: {
	fixtureIds: Set<string>;
	primaryFixture: { posX: number; posY: number; posZ: number } | undefined;
	stagePieceIds: Set<string>;
	primaryStagePiece: StagePiece | undefined;
	patchedFixtures: { id: string; posX: number; posY: number; posZ: number }[];
	uniqueStageRootIds: string[];
	allPieces: StagePiece[];
}): Vector3 | null {
	const {
		fixtureIds,
		primaryFixture,
		stagePieceIds,
		primaryStagePiece,
		patchedFixtures,
		uniqueStageRootIds,
		allPieces,
	} = args;

	const total = fixtureIds.size + stagePieceIds.size;

	// Single-selection cases — pivot at the selected item.
	if (total === 1) {
		if (fixtureIds.size === 1 && primaryFixture) {
			// Z-up → Y-up.
			return new Vector3(
				primaryFixture.posX,
				primaryFixture.posZ,
				primaryFixture.posY,
			);
		}
		if (stagePieceIds.size === 1 && primaryStagePiece) {
			return stagePieceAnchorWorld(primaryStagePiece, allPieces);
		}
	}

	// Stage-only and the dedup collapsed us to one effective target
	// (e.g., deck A + a CDJ parented to A — CDJ was dropped). Pivot at
	// the one target's anchor, not at a centroid of the user's clicks.
	if (
		fixtureIds.size === 0 &&
		uniqueStageRootIds.length === 1 &&
		primaryStagePiece
	) {
		return stagePieceAnchorWorld(primaryStagePiece, allPieces);
	}

	// Multi-select or mixed — centroid of every selected item's world position.
	const piecesById = new Map(allPieces.map((p) => [p.id, p]));
	const c = new Vector3();
	let n = 0;
	for (const f of patchedFixtures) {
		if (!fixtureIds.has(f.id)) continue;
		c.x += f.posX;
		c.y += f.posZ;
		c.z += f.posY;
		n++;
	}
	for (const id of stagePieceIds) {
		const piece = piecesById.get(id);
		if (!piece) continue;
		const anchor = stagePieceAnchorWorld(piece, allPieces);
		if (!anchor) continue;
		c.add(anchor);
		n++;
	}
	if (n === 0) return null;
	c.divideScalar(n);
	return c;
}

/**
 * World-space anchor for a stage piece — the point the gizmo widget
 * sits on, and the pivot rotation revolves around. Picked to match
 * user intent:
 *
 *   - **Parented piece** (CDJ on deck, speaker on stand, guardrail on
 *     rail, deck snapped to another deck): the *snap point* — the
 *     held-side socket that actually attaches this piece to its
 *     parent. Rotating pivots around the joint, which is what you
 *     instinctively expect.
 *
 *   - **Free piece** (no parent): bbox bottom-center. Stage GLBs have
 *     their local origin at a corner, so `group.getWorldPosition()`
 *     would land the gizmo off in space; bottom-center sits in the
 *     middle of the footprint, at the surface the piece rests on.
 */
function stagePieceAnchorWorld(
	piece: StagePiece,
	allPieces: StagePiece[],
): Vector3 | null {
	const g = getPieceGroup(piece.id);
	if (!g) return null;
	g.updateMatrixWorld(true);

	if (piece.parentPieceId) {
		const attachLocal = inferAttachmentSocketLocal(piece, allPieces);
		if (attachLocal) return attachLocal.applyMatrix4(g.matrixWorld);
	}

	const geom = getMeshGeometry(piece.meshPath);
	if (!geom) return g.getWorldPosition(new Vector3());
	const localAnchor = new Vector3(
		(geom.bbox.min.x + geom.bbox.max.x) / 2,
		geom.bbox.min.y,
		(geom.bbox.min.z + geom.bbox.max.z) / 2,
	);
	return localAnchor.applyMatrix4(g.matrixWorld);
}

/**
 * Heuristic: which held-side socket attaches `piece` to its parent?
 * Returns the socket's position in piece-local space.
 *
 * Strategy, in order:
 *   1. If the piece has exactly one non-grab socket, that's it (CDJ +
 *      equipment_mount, speaker + speaker_mount).
 *   2. Otherwise, find the (held, host) socket pair with the smallest
 *      world distance — when a snap is in effect, the two coincide,
 *      so this finds the actual contact pair.
 *   3. If we can't compute distances (parent group missing, no host
 *      sockets), fall back to the first non-grab socket.
 *
 * For surface-snapped riders (CDJ on a deck top, no discrete host
 * socket at the contact point) we still pick the closest pair, which
 * yields the held piece's mount socket — close enough.
 */
function inferAttachmentSocketLocal(
	piece: StagePiece,
	allPieces: StagePiece[],
): Vector3 | null {
	if (!piece.parentPieceId) return null;
	const parent = allPieces.find((p) => p.id === piece.parentPieceId);
	if (!parent) return null;

	const heldSockets = getMeshSockets(piece.meshPath).filter(
		(s) => s.type !== "grab",
	);
	if (heldSockets.length === 0) return null;
	if (heldSockets.length === 1) return heldSockets[0].position.clone();

	const hostSockets = getMeshSockets(parent.meshPath).filter(
		(s) => s.type !== "grab",
	);
	const pieceGroup = getPieceGroup(piece.id);
	const parentGroup = getPieceGroup(parent.id);
	if (!pieceGroup || !parentGroup || hostSockets.length === 0) {
		return heldSockets[0].position.clone();
	}
	pieceGroup.updateMatrixWorld(true);
	parentGroup.updateMatrixWorld(true);

	let best: { held: (typeof heldSockets)[number]; dist: number } | null = null;
	for (const held of heldSockets) {
		const heldWorld = held.position
			.clone()
			.applyMatrix4(pieceGroup.matrixWorld);
		for (const host of hostSockets) {
			const hostWorld = host.position
				.clone()
				.applyMatrix4(parentGroup.matrixWorld);
			const d = heldWorld.distanceTo(hostWorld);
			if (!best || d < best.dist) best = { held, dist: d };
		}
	}
	return best ? best.held.position.clone() : heldSockets[0].position.clone();
}

function writeWorldToTarget(
	target: Group,
	worldPos: Vector3,
	worldQ: Quaternion,
): void {
	const parent = target.parent;
	if (!parent) {
		target.position.copy(worldPos);
		target.quaternion.copy(worldQ);
		return;
	}
	parent.updateMatrixWorld(true);
	const parentInv = parent.matrixWorld.clone().invert();
	const worldMat = new Matrix4().compose(
		worldPos,
		worldQ,
		new Vector3(1, 1, 1),
	);
	const localMat = new Matrix4().multiplyMatrices(parentInv, worldMat);
	const localPos = new Vector3();
	const localQ = new Quaternion();
	const localScale = new Vector3();
	localMat.decompose(localPos, localQ, localScale);
	target.position.copy(localPos);
	target.quaternion.copy(localQ);
}

// ---------------------------------------------------------------------------
// Surface raycast (lifted from stage-transform-gizmo)
// ---------------------------------------------------------------------------

function isDescendantOf(obj: Object3D | null, ancestor: Object3D): boolean {
	let cur: Object3D | null = obj;
	while (cur) {
		if (cur === ancestor) return true;
		cur = cur.parent;
	}
	return false;
}

const _downRay = new Raycaster();
const _downDir = new Vector3(0, -1, 0);

function surfaceUnderPoint(
	point: Vector3,
	groups: { id: string; meshPath: string; group: Group }[],
): SnapSurface | undefined {
	_downRay.set(point, _downDir);
	_downRay.far = 100;
	const sceneObjects = groups.map((g) => g.group);
	const hits = _downRay.intersectObjects(sceneObjects, true);
	for (const hit of hits) {
		if (!hit.face) continue;
		const owner = groups.find((g) => isDescendantOf(hit.object, g.group));
		if (!owner) continue;
		const def = getStageMesh(owner.meshPath);
		if (def?.kind !== "floor") continue;
		const worldNormal = hit.face.normal
			.clone()
			.transformDirection(hit.object.matrixWorld);
		if (worldNormal.dot(new Vector3(0, 1, 0)) < 0.7) continue;
		const hostMatrix = owner.group.matrixWorld.clone();
		const inv = hostMatrix.clone().invert();
		const localPoint = hit.point.clone().applyMatrix4(inv);
		const normalMatrix = new Matrix3().getNormalMatrix(inv);
		const localNormal = worldNormal.applyMatrix3(normalMatrix).normalize();
		return {
			pieceId: owner.id,
			hostMatrix,
			localPoint,
			localNormal,
			type: "floor_top",
		};
	}
	return undefined;
}
