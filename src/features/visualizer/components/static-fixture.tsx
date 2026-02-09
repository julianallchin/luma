import { useGLTF } from "@react-three/drei";
import { createPortal, useFrame } from "@react-three/fiber";
import { useEffect, useMemo, useRef } from "react";
import {
	AdditiveBlending,
	Color,
	CylinderGeometry,
	DoubleSide,
	type Group,
	type Mesh,
	type MeshStandardMaterial,
	type Object3D,
	ShaderMaterial,
} from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { usePrimitiveState } from "../hooks/use-primitive-state";
import { applyPhysicalDimensionScaling } from "../lib/model-scaling";
import type { FixtureModelInfo, FixtureModelKind } from "./fixture-models";

// ---------------------------------------------------------------------------
// Beam configuration per fixture kind
// ---------------------------------------------------------------------------

interface BeamConfig {
	length: number;
	angleDeg: number;
	softness: number;
	peakOpacity: number;
	originOffset: number;
}

const BEAM_CONFIG: Partial<Record<FixtureModelKind, BeamConfig>> = {
	par: {
		length: 4,
		angleDeg: 50,
		softness: 0.6,
		peakOpacity: 0.18,
		originOffset: 0.1,
	},
	moving_head: {
		length: 7,
		angleDeg: 22,
		softness: 1.4,
		peakOpacity: 0.25,
		originOffset: 0.15,
	},
	scanner: {
		length: 7,
		angleDeg: 18,
		softness: 1.6,
		peakOpacity: 0.28,
		originOffset: 0.15,
	},
	strobe: {
		length: 2.5,
		angleDeg: 70,
		softness: 0.4,
		peakOpacity: 0.12,
		originOffset: 0.05,
	},
};

const DEFAULT_BEAM: BeamConfig = {
	length: 5,
	angleDeg: 30,
	softness: 1.0,
	peakOpacity: 0.2,
	originOffset: 0.12,
};

const NO_BEAM_KINDS = new Set<FixtureModelKind>(["hazer", "smoke"]);

// ---------------------------------------------------------------------------
// Volumetric beam shaders
// ---------------------------------------------------------------------------

const BEAM_VERTEX = /* glsl */ `
varying vec3 vNormal;
varying vec3 vWorldPos;
varying float vAxial;

void main() {
  vNormal = normalize(normalMatrix * normal);
  vec4 wp = modelMatrix * vec4(position, 1.0);
  vWorldPos = wp.xyz;
  // CylinderGeometry UV.y: 0 = bottom (far end), 1 = top (source)
  vAxial = uv.y;
  gl_Position = projectionMatrix * viewMatrix * wp;
}
`;

const BEAM_FRAGMENT = /* glsl */ `
uniform vec3 uColor;
uniform float uIntensity;
uniform float uSoftness;
uniform float uPeakOpacity;

varying vec3 vNormal;
varying vec3 vWorldPos;
varying float vAxial;

void main() {
  vec3 viewDir = normalize(cameraPosition - vWorldPos);
  vec3 n = normalize(vNormal);

  // View-dependent volumetric: faces pointing toward the camera represent
  // more "depth" through the light cone, so they appear brighter.
  // Silhouette edges (normal perpendicular to view) fade out.
  float ndotv = abs(dot(n, viewDir));
  float edge = pow(ndotv, uSoftness);

  // Axial falloff: brightest at fixture (vAxial~1), fading toward far end.
  float axial = mix(0.06, 1.0, pow(vAxial, 1.6));

  float alpha = edge * axial * uIntensity * uPeakOpacity;

  // Emit above 1.0 so bloom catches the beam
  gl_FragColor = vec4(uColor * 1.5, alpha);
}
`;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

interface StaticFixtureProps {
	fixture: PatchedFixture;
	definition: FixtureDefinition;
	model: FixtureModelInfo;
}

export function StaticFixture({
	fixture,
	definition,
	model,
}: StaticFixtureProps) {
	const gltf = useGLTF(model.url);
	const scene = useMemo<Group>(() => clone(gltf.scene) as Group, [gltf.scene]);

	const armRef = useRef<Object3D | null>(null);
	const headRef = useRef<Object3D | null>(null);

	// Locate nodes, apply scaling, clone materials, collect head meshes.
	const headMeshes = useMemo(() => {
		armRef.current = scene.getObjectByName("arm") || null;
		headRef.current = scene.getObjectByName("head") || null;

		applyPhysicalDimensionScaling(scene, definition);

		// Clone materials so per-instance emissive state is independent.
		scene.traverse((obj) => {
			if ((obj as Mesh).isMesh) {
				const mesh = obj as Mesh;
				if (!Array.isArray(mesh.material)) {
					mesh.material = mesh.material.clone();
				}
			}
		});

		// Collect meshes in the head node for lens-glow emissive updates.
		const target = headRef.current || scene;
		const meshes: Mesh[] = [];
		target.traverse((obj) => {
			if ((obj as Mesh).isMesh) {
				const mat = (obj as Mesh).material as MeshStandardMaterial;
				if (mat && "emissive" in mat) {
					mat.emissive = new Color(0, 0, 0);
					mat.emissiveIntensity = 0;
					meshes.push(obj as Mesh);
				}
			}
		});
		return meshes;
	}, [scene, definition]);

	useGLTF.preload(model.url);

	// ---- Beam geometry & shader material ------------------------------------

	const hasBeam = !NO_BEAM_KINDS.has(model.kind);
	const beamCfg = BEAM_CONFIG[model.kind] ?? DEFAULT_BEAM;

	const beamGeo = useMemo(() => {
		if (!hasBeam) return null;
		const halfAngle = (beamCfg.angleDeg / 2) * (Math.PI / 180);
		const farRadius = Math.tan(halfAngle) * beamCfg.length;
		return new CylinderGeometry(0.04, farRadius, beamCfg.length, 32, 1, true);
	}, [hasBeam, beamCfg]);

	const beamMat = useMemo(() => {
		if (!hasBeam) return null;
		return new ShaderMaterial({
			vertexShader: BEAM_VERTEX,
			fragmentShader: BEAM_FRAGMENT,
			uniforms: {
				uColor: { value: new Color(1, 1, 1) },
				uIntensity: { value: 0 },
				uSoftness: { value: beamCfg.softness },
				uPeakOpacity: { value: beamCfg.peakOpacity },
			},
			transparent: true,
			depthWrite: false,
			side: DoubleSide,
			blending: AdditiveBlending,
			toneMapped: false,
		});
	}, [hasBeam, beamCfg]);

	useEffect(() => {
		return () => {
			beamGeo?.dispose();
			beamMat?.dispose();
		};
	}, [beamGeo, beamMat]);

	// ---- DMX state ----------------------------------------------------------

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
		m.start = m.current;
		m.target = newTargetDeg;
		m.t = 0;
		m.duration = Math.max(1e-3, distance / Math.max(1e-3, speedDegPerSec));
	};

	const stepMotion = (axis: "pan" | "tilt", deltaSec: number) => {
		const m = motionRef.current[axis];
		if (m.t >= 1) {
			m.current = m.target;
			return m.current;
		}
		m.t = Math.min(1, m.t + deltaSec / Math.max(1e-3, m.duration));
		m.current = m.start + (m.target - m.start) * easeInOutCubic(m.t);
		return m.current;
	};

	// ---- Per-frame update ----------------------------------------------------

	useFrame((ctx, deltaSec) => {
		const state = getPrimitive();
		if (!state) return;

		const time = ctx.clock.getElapsedTime();
		let intensity = state.dimmer;

		// Strobe
		if (state.strobe > 0) {
			const hz = state.strobe * 20;
			if (hz > 0) {
				const period = 1 / hz;
				if (time % period > period * 0.5) intensity = 0;
			}
		}

		// Update beam shader uniforms directly (no React re-render)
		if (beamMat) {
			beamMat.uniforms.uColor.value.setRGB(
				state.color[0],
				state.color[1],
				state.color[2],
			);
			beamMat.uniforms.uIntensity.value = Math.min(1, intensity);
		}

		// Head mesh emissive (lens glow)
		for (const mesh of headMeshes) {
			const mat = mesh.material as MeshStandardMaterial;
			mat.emissive.setRGB(state.color[0], state.color[1], state.color[2]);
			mat.emissiveIntensity = intensity * 3;
		}

		// Motion smoothing (pan / tilt)
		const panDeg = state.position?.[0];
		const tiltDeg = state.position?.[1];
		const PAN_SPEED = 60;
		const TILT_SPEED = 40;
		const EPSILON = 0.05;

		if (Number.isFinite(panDeg)) {
			if (Math.abs(panDeg - motionRef.current.pan.target) > EPSILON) {
				retarget("pan", panDeg as number, PAN_SPEED);
			}
		}
		if (Number.isFinite(tiltDeg)) {
			if (Math.abs(tiltDeg - motionRef.current.tilt.target) > EPSILON) {
				retarget("tilt", tiltDeg as number, TILT_SPEED);
			}
		}

		const smoothPan = Number.isFinite(panDeg)
			? stepMotion("pan", deltaSec)
			: motionRef.current.pan.current;
		const smoothTilt = Number.isFinite(tiltDeg)
			? stepMotion("tilt", deltaSec)
			: motionRef.current.tilt.current;

		if (armRef.current) {
			armRef.current.rotation.y = (smoothPan * Math.PI) / 180;
		}
		if (headRef.current) {
			headRef.current.rotation.x = -(smoothTilt * Math.PI) / 180;
		}
	});

	// ---- Render --------------------------------------------------------------

	const lightTarget = headRef.current || scene;

	return (
		<primitive object={scene}>
			{hasBeam &&
				beamGeo &&
				beamMat &&
				createPortal(
					<mesh
						geometry={beamGeo}
						material={beamMat}
						position={[0, -(beamCfg.length / 2 - beamCfg.originOffset), 0]}
						renderOrder={10}
					/>,
					lightTarget,
				)}
		</primitive>
	);
}
