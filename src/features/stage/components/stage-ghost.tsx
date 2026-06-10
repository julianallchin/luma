import { useGLTF } from "@react-three/drei";
import { useFrame, useThree } from "@react-three/fiber";
import { Suspense, useEffect, useMemo, useRef } from "react";
import {
	Box3,
	type Group,
	Matrix3,
	type Mesh,
	type MeshStandardMaterial,
	Plane,
	Raycaster,
	Vector2,
	Vector3,
} from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";
import { getMeshSockets, registerMeshGeometry } from "../lib/mesh-cache";
import { getPieceGroup } from "../lib/piece-refs";
import { type SnapSurface, solveSnap } from "../lib/snap";
import { getStageMesh } from "../lib/stage-meshes";
import { useStagePieceStore } from "../stores/use-stage-piece-store";

function isDescendantOf(
	obj: { parent: unknown } | null,
	ancestor: Group,
): boolean {
	let cur: unknown = obj;
	while (cur) {
		if (cur === ancestor) return true;
		cur = (cur as { parent?: unknown }).parent ?? null;
	}
	return false;
}

/**
 * Cursor-following ghost during the arm-from-palette placement flow.
 *
 *  - Raycasts the pointer against the Y=0 plane each frame.
 *  - Calls the socket snap solver to compute the held piece's world pose
 *    given the cursor target and the current scene.
 *  - Publishes the solver's `(position, quaternion, parentId)` to the
 *    store so the outer click handler can commit it.
 *  - Renders a semi-transparent preview at the snapped pose.
 *
 * Hold Shift to disable snap (free placement at the cursor with the held
 * mesh's `grab` socket on the ground).
 */
export function StageGhost() {
	const armedMeshPath = useStagePieceStore((s) => s.armedMeshPath);
	const setGhost = useStagePieceStore((s) => s.setGhost);
	const pieces = useStagePieceStore((s) => s.pieces);

	const { camera, gl } = useThree();
	const groupRef = useRef<Group | null>(null);
	const pointer = useRef(new Vector2());
	const shiftHeld = useRef(false);

	useEffect(() => {
		if (!armedMeshPath) return;
		const dom = gl.domElement;
		const handleMove = (e: PointerEvent) => {
			const rect = dom.getBoundingClientRect();
			pointer.current.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
			pointer.current.y = -((e.clientY - rect.top) / rect.height) * 2 + 1;
			shiftHeld.current = e.shiftKey;
		};
		const handleKey = (e: KeyboardEvent) => {
			shiftHeld.current = e.shiftKey;
		};
		dom.addEventListener("pointermove", handleMove);
		window.addEventListener("keydown", handleKey);
		window.addEventListener("keyup", handleKey);
		return () => {
			dom.removeEventListener("pointermove", handleMove);
			window.removeEventListener("keydown", handleKey);
			window.removeEventListener("keyup", handleKey);
		};
	}, [armedMeshPath, gl]);

	useEffect(() => {
		if (!armedMeshPath) setGhost(null);
	}, [armedMeshPath, setGhost]);

	const raycaster = useMemo(() => new Raycaster(), []);
	const groundPlane = useMemo(() => new Plane(new Vector3(0, 1, 0), 0), []);
	const cursorHit = useMemo(() => new Vector3(), []);

	useFrame(() => {
		if (!armedMeshPath) return;
		raycaster.setFromCamera(pointer.current, camera);

		// Collect live world matrices + group refs for placed pieces.
		const groupsByPiece: {
			piece: (typeof pieces)[number];
			group: ReturnType<typeof getPieceGroup>;
		}[] = [];
		const scenePieces = pieces
			.map((p) => {
				const g = getPieceGroup(p.id);
				if (!g) return null;
				g.updateMatrixWorld(true);
				groupsByPiece.push({ piece: p, group: g });
				return {
					id: p.id,
					meshPath: p.meshPath,
					worldMatrix: g.matrixWorld.clone(),
				};
			})
			.filter((x): x is NonNullable<typeof x> => x !== null);

		// Cursor target: first try raycasting against the scene (so hovering
		// over a deck's top surface points the cursor *there*, not at the
		// ground beneath the deck). Fall back to the Y=0 plane if nothing's
		// hit.
		const candidates = groupsByPiece
			.map((g) => g.group)
			.filter((g): g is NonNullable<typeof g> => g !== null);
		const hits =
			candidates.length > 0 ? raycaster.intersectObjects(candidates, true) : [];

		let surface: SnapSurface | undefined;
		if (hits.length > 0) {
			cursorHit.copy(hits[0].point);

			// If the hit traces back to a placed "floor" piece, build a
			// surface-snap host at the exact hit point. Lets equipment/speakers
			// land anywhere on a deck top, parented to that deck.
			const hit = hits[0];
			const hitGroup = groupsByPiece.find(
				(g) => g.group !== null && isDescendantOf(hit.object, g.group),
			);
			if (hitGroup && hitGroup.group && hit.face) {
				const def = getStageMesh(hitGroup.piece.meshPath);
				const isUpward =
					hit.face.normal
						.clone()
						.transformDirection(hit.object.matrixWorld)
						.dot(new Vector3(0, 1, 0)) > 0.7;
				if (def?.kind === "floor" && isUpward) {
					const hostMatrix = hitGroup.group.matrixWorld.clone();
					const inv = hostMatrix.clone().invert();
					const localPoint = cursorHit.clone().applyMatrix4(inv);
					const normalMatrix = new Matrix3().getNormalMatrix(inv);
					const localNormal = hit.face.normal
						.clone()
						.transformDirection(hit.object.matrixWorld)
						.applyMatrix3(normalMatrix)
						.normalize();
					surface = {
						pieceId: hitGroup.piece.id,
						hostMatrix,
						localPoint,
						localNormal,
						type: "floor_top",
					};
				}
			}
		} else if (!raycaster.ray.intersectPlane(groundPlane, cursorHit)) {
			return;
		}

		const result = solveSnap({
			heldMeshPath: armedMeshPath,
			cursorWorld: cursorHit.clone(),
			pieces: scenePieces,
			shiftHeld: shiftHeld.current,
			surface,
			lookupSockets: getMeshSockets,
		});

		setGhost({
			position: [result.position.x, result.position.y, result.position.z],
			quaternion: [
				result.quaternion.x,
				result.quaternion.y,
				result.quaternion.z,
				result.quaternion.w,
			],
			parentId: result.parentId,
		});

		if (groupRef.current) {
			groupRef.current.position.copy(result.position);
			groupRef.current.quaternion.copy(result.quaternion);
		}
	});

	if (!armedMeshPath) return null;
	const def = getStageMesh(armedMeshPath);
	if (!def) return null;

	return (
		<Suspense fallback={null}>
			<GhostMesh url={def.url} meshPath={armedMeshPath} groupRef={groupRef} />
		</Suspense>
	);
}

function GhostMesh({
	url,
	meshPath,
	groupRef,
}: {
	url: string;
	meshPath: string;
	groupRef: React.MutableRefObject<Group | null>;
}) {
	const gltf = useGLTF(url);
	const sceneClone = useMemo<Group>(
		() => clone(gltf.scene) as Group,
		[gltf.scene],
	);

	useEffect(() => {
		// First time we render this mesh as a ghost (before any
		// `StagePieceObject` of the same type has registered it), the
		// global socket cache is empty — `solveSnap` then has no grab
		// socket and falls through to free placement, plopping the GLB
		// pivot (usually a corner) at the cursor with no snap candidates.
		// Register the bbox here so the next frame has the sockets.
		//
		// Measure a *detached* clone, NOT `sceneClone`: `sceneClone` is
		// mounted under the cursor-following group, which `useFrame` has
		// already snapped onto a nearby piece by the time this effect runs.
		// `setFromObject` walks world matrices, so measuring it would bake
		// that world offset into the bbox — and the idempotent cache would
		// freeze the contaminated box, parking the sockets metres from the
		// model. A fresh clone has an identity world matrix → pivot-local box.
		const measureClone = clone(gltf.scene) as Group;
		measureClone.updateMatrixWorld(true);
		const box = new Box3().setFromObject(measureClone);
		registerMeshGeometry(meshPath, box);

		sceneClone.traverse((obj) => {
			if (!(obj as Mesh).isMesh) return;
			const mesh = obj as Mesh;
			if (Array.isArray(mesh.material)) return;
			const mat = (mesh.material as MeshStandardMaterial).clone();
			mat.transparent = true;
			mat.opacity = 0.45;
			mat.depthWrite = false;
			if ("emissive" in mat) {
				mat.emissive.set(0xfacc15);
				mat.emissiveIntensity = 0.25;
			}
			mesh.material = mat;
		});
	}, [sceneClone, meshPath]);

	useGLTF.preload(url);

	return (
		<group ref={groupRef}>
			<primitive object={sceneClone} />
		</group>
	);
}
