import { createPortal, useFrame } from "@react-three/fiber";
import { useGLTF, Line } from "@react-three/drei";
import { useMemo, useRef, useState } from "react";
import {
	Color,
	MathUtils,
	Object3D,
	type Group,
	type Mesh,
} from "three";
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

	// Physical Dimensions & Focus
	const { panMax, tiltMax } = useMemo(() => {
		// Physical.Focus
		const focus = (definition as any)?.Physical?.Focus;
		const panMaxVal =
			typeof focus?.["@PanMax"] === "number" ? focus["@PanMax"] : 360;
		const tiltMaxVal =
			typeof focus?.["@TiltMax"] === "number" ? focus["@TiltMax"] : 270;

		return {
			panMax: panMaxVal || 360,
			tiltMax: tiltMaxVal || 270,
		};
	}, [definition]);

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

	const [visualState, setVisualState] = useState({
		intensity: 0,
		color: new Color(0, 0, 0),
	});

	useFrame(() => {
		const universeData = dmxStore.getUniverse(Number(fixture.universe));
		if (!universeData) return;

		const startAddress = Number(fixture.address) - 1; // 0-based
		const state = getHeadState(mapping, universeData, startAddress);

		// Only update state if changed significantly to save renders
		const newColor = new Color(state.color.r, state.color.g, state.color.b);
		if (
			Math.abs(state.intensity - visualState.intensity) > 0.01 ||
			!visualState.color.equals(newColor)
		) {
			setVisualState({
				intensity: state.intensity,
				color: newColor,
			});
		}

		// Pan around Y, tilt around X to mirror the QLC+ mesh hierarchy.
		// Direct ref manipulation is fine (doesn't need render)
		if (armRef.current) {
			const panDeg = (state.pan / 255) * panMax;
			armRef.current.rotation.y = MathUtils.degToRad(panDeg);
		}

		if (headRef.current) {
			const tiltDeg = (state.tilt / 255) * tiltMax;
			headRef.current.rotation.x = MathUtils.degToRad(tiltDeg - tiltMax / 2);
		}
	});

	// Determine where to attach the light
	const lightTarget = headRef.current || scene;

	return (
		<primitive object={scene}>
			{createPortal(
				<Line
					points={[
						[0, 0, 0],
						[0, 0, -10], // 10 meters out in negative Z (forward)
					]}
					color={visualState.color}
					lineWidth={visualState.intensity > 0 ? 2 : 0}
					transparent
					opacity={visualState.intensity}
				/>,
				lightTarget,
			)}
		</primitive>
	);
}