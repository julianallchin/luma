import { useFrame } from "@react-three/fiber";
import { useGLTF } from "@react-three/drei";
import { useMemo, useRef } from "react";
import { Color, MathUtils, Object3D, type Group, type Mesh } from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import {
	type DmxMapping,
	getDmxMapping,
	getHeadState,
} from "../lib/fixture-utils";
import { dmxStore } from "../stores/dmx-store";
import type { FixtureModelInfo } from "./fixture-models";

interface StaticFixtureProps {
	fixture: PatchedFixture;
	definition: FixtureDefinition;
	modeName: string;
	model: FixtureModelInfo;
}

/**
 * Renders a static GLB model with DMX-driven pan/tilt and color.
 * Mirrors the node names from the QLC+ meshes (base/arm/head).
 */
export function StaticFixture({
	fixture,
	definition,
	modeName,
	model,
}: StaticFixtureProps) {
	const gltf = useGLTF(model.url);

	// Clone the scene so each instance has its own transform/material state.
	const scene = useMemo<Group>(() => clone(gltf.scene) as Group, [gltf.scene]);

	const armRef = useRef<Object3D | null>(null);
	const headRef = useRef<Object3D | null>(null);

	// Locate nodes by name once.
	useMemo(() => {
		armRef.current = scene.getObjectByName("arm") || null;
		headRef.current = scene.getObjectByName("head") || null;
		return null;
	}, [scene]);

	// DMX channel mapping for the first head (common for moving heads/scanners/pars).
	const mapping: DmxMapping = useMemo(
		() => getDmxMapping(definition, modeName, 0),
		[definition, modeName],
	);

	// Focus angles (fallbacks match QLC+ defaults)
	const { panMax, tiltMax } = useMemo(() => {
		// Physical.Focus is not typed in bindings; read it defensively.
		const focus = (definition as any)?.Physical?.Focus;
		const panMaxVal =
			typeof focus?.["@PanMax"] === "number" ? focus["@PanMax"] : 360;
		const tiltMaxVal =
			typeof focus?.["@TiltMax"] === "number" ? focus["@TiltMax"] : 270;
		return { panMax: panMaxVal || 360, tiltMax: tiltMaxVal || 270 };
	}, [definition]);

	const color = useMemo(() => new Color(), []);

	useGLTF.preload(model.url);

	useMemo(() => {
		// Ensure head meshes start with a non-black emissive so bloom can work later.
		scene.traverse((obj) => {
			if ((obj as Mesh).isMesh) {
				const mat = (obj as Mesh).material as any;
				if (mat && "emissive" in mat) {
					mat.emissive = mat.emissive ?? new Color(0, 0, 0);
					mat.emissiveIntensity = mat.emissiveIntensity ?? 0;
				}
			}
		});
		return null;
	}, [scene]);

	useFrame(() => {
		const universeData = dmxStore.getUniverse(Number(fixture.universe));
		if (!universeData) return;

		const startAddress = Number(fixture.address) - 1; // 0-based
		const state = getHeadState(mapping, universeData, startAddress);

		// Pan around Y, tilt around X to mirror the QLC+ mesh hierarchy.
		if (armRef.current) {
			const panDeg = (state.pan / 255) * panMax;
			armRef.current.rotation.y = MathUtils.degToRad(panDeg);
		}

		if (headRef.current) {
			const tiltDeg = (state.tilt / 255) * tiltMax;
			// Center tilt around forward to mimic real fixtures.
			headRef.current.rotation.x = MathUtils.degToRad(tiltDeg - tiltMax / 2);

			// Apply emissive color/intensity to head meshes.
			headRef.current.traverse((obj) => {
				if ((obj as Mesh).isMesh) {
					const mat = (obj as Mesh).material as any;
					if (!mat || !("emissive" in mat)) return;
					color.setRGB(state.color.r, state.color.g, state.color.b);
					mat.emissive.copy(color);
					mat.emissiveIntensity = state.intensity;
					mat.toneMapped = false;
				}
			});
		}
	});

	return <primitive object={scene} />;
}
