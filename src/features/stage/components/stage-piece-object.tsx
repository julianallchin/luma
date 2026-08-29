import { useGLTF } from "@react-three/drei";
import { useMemo } from "react";
import {
	Box3,
	type BufferGeometry,
	type Group,
	type Material,
	type Mesh,
	type MeshStandardMaterial,
	Quaternion,
	Vector3,
} from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";
import { registerMeshGeometry } from "../lib/mesh-cache";
import { meshUrl } from "../lib/stage-meshes";

/** Skip raycasting for decorative / debug overlays so they don't intercept clicks. */
const NULL_RAYCAST = () => {};

// Outline color tiers, brightest → dimmest.
// Reused for fixtures' selection visual in fixture-object.tsx; keep in sync.
const PRIMARY_COLOR = "#facc15"; // bright yellow — the clicked piece
const CLUSTER_COLOR = "#b8b846"; // muted gold — cluster siblings
const HOVER_COLOR = "#38bdf8"; // sky — hover preview (no selection conflict)

interface StagePieceObjectProps {
	id: string;
	meshPath: string;
	enableEditing: boolean;
	/** Clicked piece — brightest outline. */
	isPrimary: boolean;
	/** Belongs to the selected cluster (but isn't the primary). */
	inSelectedCluster: boolean;
	/** Belongs to the hovered cluster. Lower priority than selection. */
	inHoveredCluster: boolean;
}

/**
 * Leaf content for a single stage piece: loads the GLB, registers its
 * measured bbox into the global mesh cache, and renders the selection
 * wireframe.
 *
 * Positioning, parent-child nesting, and the transform gizmo are handled
 * one level up in `StagePieceNode`.
 */
export function StagePieceObject({
	id: _id,
	meshPath,
	enableEditing,
	isPrimary,
	inSelectedCluster,
	inHoveredCluster,
}: StagePieceObjectProps) {
	// The catalog entry is not needed to *draw* a piece — only its GLB is, and
	// a venue may still hold a mesh the catalog has dropped (the ripped
	// trusses). Drawing what is placed is unconditional; the palette is what
	// the catalog gates.
	const url = meshUrl(meshPath);
	const gltf = useGLTF(url ?? "");

	const { meshEntries, bboxSize, bboxCenter } = useMemo(() => {
		const cloned = clone(gltf.scene) as Group;
		cloned.updateMatrixWorld(true);

		// Extract each Mesh from the cloned scene with its accumulated
		// world matrix, so we can render them as flat JSX <mesh> elements
		// instead of via <primitive object={cloned}>. R3F's <primitive>
		// is a pure pass-through and does NOT apply receiveShadow as a
		// prop to descendant meshes — which is why the GLB-rendered
		// pieces silently fall through with their shader compiled WITHOUT
		// USE_SHADOWMAP. JSX <mesh receiveShadow> sets the flag before
		// the material is first compiled, so the shadow chunk gets
		// included and the deck actually catches shadows.
		const meshes: {
			key: string;
			geometry: BufferGeometry;
			material: Material | Material[];
			position: Vector3;
			quaternion: Quaternion;
			scale: Vector3;
		}[] = [];

		cloned.traverse((obj) => {
			if (!(obj as Mesh).isMesh) return;
			const mesh = obj as Mesh;

			// The stage_lab GLBs ship primitives with only POSITION +
			// TEXCOORD_0 attributes — no NORMAL. Three.js's lighting and
			// shadow shaders both need vertex normals; without them the
			// surface lights only via shader-derived flat normals (no
			// shadow contribution at all). Compute normals once per
			// geometry (shared across all clones of this GLB, so it's a
			// one-time cost).
			if (!mesh.geometry.attributes.normal) {
				mesh.geometry.computeVertexNormals();
			}

			const sharedMats: Material[] = Array.isArray(mesh.material)
				? mesh.material
				: [mesh.material];
			const ownMats: Material[] = sharedMats.map((m) => m.clone());
			for (const m of ownMats) {
				(m as MeshStandardMaterial).needsUpdate = true;
			}

			const position = new Vector3();
			const quaternion = new Quaternion();
			const scale = new Vector3();
			mesh.matrixWorld.decompose(position, quaternion, scale);

			meshes.push({
				key: mesh.uuid,
				geometry: mesh.geometry,
				material: ownMats.length === 1 ? ownMats[0] : ownMats,
				position,
				quaternion,
				scale,
			});
		});

		const box = new Box3().setFromObject(cloned);
		registerMeshGeometry(meshPath, box);
		return {
			meshEntries: meshes,
			bboxSize: box.getSize(new Vector3()),
			bboxCenter: box.getCenter(new Vector3()),
		};
	}, [gltf.scene, meshPath]);

	useGLTF.preload(url ?? "");

	if (!url) return null;

	// Outline priority: primary > cluster member > hover. We only draw at
	// most one box per piece — the bright primary wins over a softer
	// cluster-member tint, etc.
	let outlineColor: string | null = null;
	if (isPrimary) outlineColor = PRIMARY_COLOR;
	else if (inSelectedCluster) outlineColor = CLUSTER_COLOR;
	else if (inHoveredCluster) outlineColor = HOVER_COLOR;

	return (
		<>
			{meshEntries.map((entry) => (
				<mesh
					key={entry.key}
					geometry={entry.geometry}
					material={entry.material}
					position={entry.position}
					quaternion={entry.quaternion}
					scale={entry.scale}
					castShadow
					receiveShadow
				/>
			))}
			{enableEditing && outlineColor && (
				<mesh position={bboxCenter} raycast={NULL_RAYCAST}>
					<boxGeometry args={[bboxSize.x, bboxSize.y, bboxSize.z]} />
					<meshBasicMaterial color={outlineColor} wireframe />
				</mesh>
			)}
		</>
	);
}
