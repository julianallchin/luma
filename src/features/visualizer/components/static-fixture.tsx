import { useGLTF } from "@react-three/drei";
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

	const motionRef = useRef<{
		pan: {
			initialized: boolean;
			current: number;
			start: number;
			target: number;
			t: number;
			duration: number;
		};
		tilt: {
			initialized: boolean;
			current: number;
			start: number;
			target: number;
			t: number;
			duration: number;
		};
	}>({
		pan: {
			initialized: false,
			current: 0,
			start: 0,
			target: 0,
			t: 1,
			duration: 0.001,
		},
		tilt: {
			initialized: false,
			current: 0,
			start: 0,
			target: 0,
			t: 1,
			duration: 0.001,
		},
	});

	const easeInOutCubic = (t: number) =>
		t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2;

	const retarget = (
		axis: "pan" | "tilt",
		newTargetDeg: number,
		speedDegPerSec: number,
	) => {
		const m = motionRef.current[axis];
		if (!m.initialized) {
			m.initialized = true;
			m.current = newTargetDeg;
			m.start = newTargetDeg;
			m.target = newTargetDeg;
			m.t = 1;
			m.duration = 0.001;
			return;
		}
		const distance = Math.abs(newTargetDeg - m.current);
		const duration = distance / Math.max(1e-3, speedDegPerSec);

		m.start = m.current;
		m.target = newTargetDeg;
		m.t = 0;
		m.duration = Math.max(1e-3, duration);
	};

	const stepMotion = (axis: "pan" | "tilt", deltaSec: number) => {
		const m = motionRef.current[axis];
		if (m.t >= 1) {
			m.current = m.target;
			return m.current;
		}
		const duration = Math.max(1e-3, m.duration);
		m.t = Math.min(1, m.t + deltaSec / duration);
		const t = easeInOutCubic(m.t);
		m.current = m.start + (m.target - m.start) * t;
		return m.current;
	};

	useFrame((ctx, deltaSec) => {
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

		const panDeg = state.position?.[0];
		const tiltDeg = state.position?.[1];

		// Moving-head motion simulation: each fixture eases to the latest target.
		// If a new target arrives mid-move, restart the ease from the current position.
		//
		// Pan generally moves faster than tilt on real fixtures; keep a small minimum
		// duration to avoid jitter when targets update every frame.
		const PAN_SPEED_DEG_PER_SEC = 60;
		const TILT_SPEED_DEG_PER_SEC = 40;
		const TARGET_EPSILON_DEG = 0.05;

		if (Number.isFinite(panDeg)) {
			if (
				Math.abs(panDeg - motionRef.current.pan.target) > TARGET_EPSILON_DEG
			) {
				retarget("pan", panDeg as number, PAN_SPEED_DEG_PER_SEC);
			}
		}

		if (Number.isFinite(tiltDeg)) {
			if (
				Math.abs(tiltDeg - motionRef.current.tilt.target) > TARGET_EPSILON_DEG
			) {
				retarget("tilt", tiltDeg as number, TILT_SPEED_DEG_PER_SEC);
			}
		}

		const smoothedPanDeg = Number.isFinite(panDeg)
			? stepMotion("pan", deltaSec)
			: motionRef.current.pan.current;
		const smoothedTiltDeg = Number.isFinite(tiltDeg)
			? stepMotion("tilt", deltaSec)
			: motionRef.current.tilt.current;

		// Semantic convention: degrees are signed and centered at 0.
		if (armRef.current) {
			armRef.current.rotation.y = (smoothedPanDeg * Math.PI) / 180;
		}

		if (headRef.current) {
			headRef.current.rotation.x = -(smoothedTiltDeg * Math.PI) / 180;
		}
	});

	// Determine where to attach the light
	const lightTarget = headRef.current || scene;

	const beamLength = 8;
	const beamRadius = 0.6;
	const beamOriginOffset = 0.15;

	return (
		<primitive object={scene}>
			{createPortal(
				<mesh
					// `moving_head.glb`'s head points along -Y in its local space.
					// Keep the beam aligned to the head's local forward axis so it tracks pan/tilt.
					// Offset a bit so it starts closer to the lens, not the head center.
					position={[0, -(beamLength / 2 - beamOriginOffset), 0]}
				>
					<cylinderGeometry
						args={[beamRadius * 0.05, beamRadius, beamLength, 12, 1, true]}
					/>
					<meshBasicMaterial
						color={visualState.color}
						transparent
						opacity={Math.min(1, visualState.intensity) * 0.35}
						depthWrite={false}
					/>
				</mesh>,
				lightTarget,
			)}
		</primitive>
	);
}
