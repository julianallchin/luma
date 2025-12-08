import { Line, useGLTF } from "@react-three/drei";
import { createPortal, useFrame } from "@react-three/fiber";
import { useMemo, useRef, useState } from "react";
import { Color, type Group, type Mesh, type Object3D } from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { usePrimitiveState } from "../hooks/use-primitive-state";
import { applyPhysicalDimensionScaling } from "../lib/model-scaling";
import type { FixtureModelInfo } from "./fixture-models";

interface StaticFixtureProps {
	fixture: PatchedFixture;
	definition: FixtureDefinition;
	model: FixtureModelInfo;
}

/**
 * Renders a static GLB model with DMX-driven pan/tilt and color.
 * Mirrors the node names from the QLC+ meshes (base/arm/head).
 */
export function StaticFixture({
	fixture,
	definition,
	model,
}: StaticFixtureProps) {
	const gltf = useGLTF(model.url);

	// Clone the scene so each instance has its own transform/material state.
	const scene = useMemo<Group>(() => clone(gltf.scene) as Group, [gltf.scene]);

	const armRef = useRef<Object3D | null>(null);
	const headRef = useRef<Object3D | null>(null);

	// Locate nodes by name and apply physical dimension scaling.
	useMemo(() => {
		armRef.current = scene.getObjectByName("arm") || null;
		headRef.current = scene.getObjectByName("head") || null;

		// Apply scaling based on fixture's physical dimensions, matching QLC+ behavior
		applyPhysicalDimensionScaling(scene, definition);

		return null;
	}, [scene, definition]);

	useGLTF.preload(model.url);

	useMemo(() => {
		// Ensure head meshes start with a non-black emissive so bloom can work later.
		scene.traverse((obj) => {
			if ((obj as Mesh).isMesh) {
				const mat = (obj as Mesh).material;
				if (
					mat &&
					typeof mat === "object" &&
					"emissive" in mat &&
					"emissiveIntensity" in mat
				) {
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

	// Subscribe to Universe State for Head 0
	// Defaulting to head 0 for static/simple fixtures
	const getPrimitive = usePrimitiveState(`${fixture.id}:0`);

	useFrame((ctx) => {
		const state = getPrimitive();
		if (!state) return; // No state yet

		const time = ctx.clock.getElapsedTime();

		let intensity = state.dimmer;

		// Simple Strobe Logic (Semantic)
		// state.strobe is 0.0 (off) to 1.0 (fastest)
		// If > 0, blink.
		if (state.strobe > 0) {
			const hz = state.strobe * 20; // Map 1.0 to 20Hz max
			if (hz > 0) {
				const period = 1 / hz;
				const isOff = time % period > period * 0.5;
				if (isOff) {
					intensity = 0;
				}
			}
		}

		// Only update state if changed significantly to save renders
		// state.color is [r, g, b] 0-1
		const newColor = new Color(state.color[0], state.color[1], state.color[2]);

		if (
			Math.abs(intensity - visualState.intensity) > 0.01 ||
			!visualState.color.equals(newColor)
		) {
			setVisualState({
				intensity: intensity,
				color: newColor,
			});
		}

		// Pan/Tilt logic is currently disabled/static in semantic mode
		// until we add pan/tilt to PrimitiveState.
		/*
		if (armRef.current) {
			// Needs state.pan
		}
		if (headRef.current) {
			// Needs state.tilt
		}
		*/
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
