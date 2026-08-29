import { useGLTF } from "@react-three/drei";
import { useMemo } from "react";
import {
	type BufferGeometry,
	type Group,
	type Material,
	type Mesh,
	type MeshStandardMaterial,
	Quaternion,
	Vector3,
} from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";

/**
 * One venue node's GLB, drawn at its parent group's pose.
 *
 * The meshes are extracted from the loaded scene and re-emitted as flat JSX
 * `<mesh>` elements rather than mounted via `<primitive>`: R3F's `<primitive>`
 * is a pass-through and does not apply `receiveShadow` to descendants, so the
 * material compiles without `USE_SHADOWMAP` and the piece never catches a
 * shadow.
 */
export function StageNodeMesh({ url }: { url: string }) {
	const gltf = useGLTF(url);

	const meshEntries = useMemo(() => {
		const cloned = clone(gltf.scene) as Group;
		cloned.updateMatrixWorld(true);

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

			// The stage_lab GLBs ship primitives with POSITION + TEXCOORD_0 and
			// no NORMAL. Three.js's lighting and shadow shaders both need vertex
			// normals; without them the surface makes no shadow contribution at
			// all. Computed once per geometry, which is shared across clones.
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

		return meshes;
	}, [gltf.scene]);

	useGLTF.preload(url);

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
		</>
	);
}
