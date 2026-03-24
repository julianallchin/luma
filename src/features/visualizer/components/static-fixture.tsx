import { useGLTF } from "@react-three/drei";
import { createPortal, useFrame } from "@react-three/fiber";
import { useEffect, useMemo, useRef } from "react";
import {
	AdditiveBlending,
	Color,
	CylinderGeometry,
	DoubleSide,
	Euler,
	type Group,
	type Mesh,
	type MeshStandardMaterial,
	type Object3D,
	Quaternion,
	ShaderMaterial,
	Vector3,
} from "three";
import { clone } from "three/examples/jsm/utils/SkeletonUtils.js";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { usePrimitiveState } from "../hooks/use-primitive-state";
import { applyPhysicalDimensionScaling } from "../lib/model-scaling";
import { submitLight } from "../lib/spotlight-pool";
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
	/** View-independent scatter floor (0–1). Keeps the beam visible from side
	 *  angles where the view-dependent term would otherwise drop to zero. */
	scatter: number;
}

const BEAM_CONFIG: Partial<Record<FixtureModelKind, BeamConfig>> = {
	par: {
		length: 4,
		angleDeg: 90,
		softness: 1.3,
		peakOpacity: 0.18,
		originOffset: 0.1,
		scatter: 0.15,
	},
	moving_head: {
		length: 7,
		angleDeg: 22,
		softness: 1.4,
		peakOpacity: 0.25,
		originOffset: 0.15,
		scatter: 0.08,
	},
	scanner: {
		length: 7,
		angleDeg: 18,
		softness: 1.6,
		peakOpacity: 0.28,
		originOffset: 0.15,
		scatter: 0.06,
	},
	strobe: {
		length: 2.5,
		angleDeg: 70,
		softness: 0.4,
		peakOpacity: 0.12,
		originOffset: 0.05,
		scatter: 0.2,
	},
};

const DEFAULT_BEAM: BeamConfig = {
	length: 5,
	angleDeg: 30,
	softness: 1.0,
	peakOpacity: 0.2,
	originOffset: 0.12,
	scatter: 0.1,
};

const NO_BEAM_KINDS = new Set<FixtureModelKind>(["hazer", "smoke"]);

const _axisX = new Vector3(1, 0, 0);
const _axisY = new Vector3(0, 1, 0);

// Scene light intensity multiplier per fixture kind
const LIGHT_INTENSITY: Partial<Record<FixtureModelKind, number>> = {
	par: 10,
	moving_head: 40,
	scanner: 40,
	strobe: 8,
};
const DEFAULT_LIGHT_INTENSITY = 15;

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
uniform float uScatter;

varying vec3 vNormal;
varying vec3 vWorldPos;
varying float vAxial;

void main() {
  vec3 viewDir = normalize(cameraPosition - vWorldPos);
  vec3 n = normalize(vNormal);

  float ndotv = abs(dot(n, viewDir));

  // View-dependent depth: surfaces facing the camera represent more
  // "thickness" through the light volume, so they appear brighter.
  float viewThrough = pow(ndotv, uSoftness);

  // Blend between a view-independent scatter floor and full view-through.
  // Real fog scatters light in all directions, so the beam should remain
  // visible from side angles where ndotv approaches zero.
  float edge = mix(uScatter, 1.0, viewThrough);

  // Fade at the geometric silhouette to prevent a hard cone boundary.
  edge *= smoothstep(0.0, 0.1, ndotv);

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
	/** Hide cylinder beam geometry (volumetric raymarching replaces it). */
	hideBeams?: boolean;
}

export function StaticFixture({
	fixture,
	definition,
	model,
	hideBeams = false,
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

	// Force all body materials to near-black so only beams/emissives are visible
	// Enable shadow casting/receiving on all meshes
	useEffect(() => {
		scene.traverse((obj) => {
			if (!(obj as Mesh).isMesh) return;
			const mesh = obj as Mesh;
			mesh.castShadow = true;
			mesh.receiveShadow = true;
			const mat = mesh.material as MeshStandardMaterial;
			if (mat && "color" in mat) {
				mat.color.setRGB(0.08, 0.08, 0.08);
			}
		});
	}, [scene]);

	// ---- Beam geometry & shader material ------------------------------------

	const isBeamCapable = !NO_BEAM_KINDS.has(model.kind);
	const hasBeam = !hideBeams && isBeamCapable;
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
				uScatter: { value: beamCfg.scatter },
			},
			transparent: true,
			depthWrite: false,
			side: DoubleSide,
			blending: AdditiveBlending,
			toneMapped: false,
		});
	}, [hasBeam, beamCfg]);

	// Reusable math objects for world-space beam direction
	const _beamDir = useMemo(() => new Vector3(), []);
	const _qTilt = useMemo(() => new Quaternion(), []);
	const _qPan = useMemo(() => new Quaternion(), []);
	const _qFixture = useMemo(() => new Quaternion(), []);
	const _euler = useMemo(() => new Euler(), []);

	useEffect(() => {
		return () => {
			beamGeo?.dispose();
			beamMat?.dispose();
		};
	}, [beamGeo, beamMat]);

	// ---- DMX state ----------------------------------------------------------

	const getPrimitive = usePrimitiveState(`${fixture.id}:0`);

	// ---- Per-frame update ----------------------------------------------------

	useFrame((ctx) => {
		const state = getPrimitive();

		const time = ctx.clock.getElapsedTime();
		let intensity = state?.dimmer ?? 0;

		// Strobe
		if (state && state.strobe > 0) {
			const hz = state.strobe * 20;
			if (hz > 0) {
				const period = 1 / hz;
				if (time % period > period * 0.5) intensity = 0;
			}
		}

		const color = state?.color ?? [0, 0, 0];

		// Update beam shader uniforms directly (no React re-render)
		if (beamMat) {
			beamMat.uniforms.uColor.value.setRGB(color[0], color[1], color[2]);
			beamMat.uniforms.uIntensity.value = Math.min(1, intensity);
		}

		// Head mesh emissive (lens glow)
		for (const mesh of headMeshes) {
			const mat = mesh.material as MeshStandardMaterial;
			mat.emissive.setRGB(color[0], color[1], color[2]);
			mat.emissiveIntensity = intensity * 3;
		}

		// Submit light request to the shared pool
		if (isBeamCapable && intensity > 0.01) {
			const panDeg = state?.position?.[0] ?? 0;
			const tiltDeg = state?.position?.[1] ?? 0;

			_beamDir.set(0, -1, 0);
			_qTilt.setFromAxisAngle(_axisX, -(tiltDeg * Math.PI) / 180);
			_beamDir.applyQuaternion(_qTilt);
			_qPan.setFromAxisAngle(_axisY, (panDeg * Math.PI) / 180);
			_beamDir.applyQuaternion(_qPan);
			_euler.set(fixture.rotX, fixture.rotZ, fixture.rotY);
			_qFixture.setFromEuler(_euler);
			_beamDir.applyQuaternion(_qFixture);
			_beamDir.normalize();

			const lightMul = LIGHT_INTENSITY[model.kind] ?? DEFAULT_LIGHT_INTENSITY;
			submitLight({
				posX: fixture.posX,
				posY: fixture.posZ,
				posZ: fixture.posY,
				dirX: _beamDir.x,
				dirY: _beamDir.y,
				dirZ: _beamDir.z,
				r: color[0],
				g: color[1],
				b: color[2],
				intensity: intensity * lightMul,
				angle: (beamCfg.angleDeg / 2) * (Math.PI / 180),
				distance: beamCfg.length * 2,
			});
		}

		// Position (pan / tilt) — skip when speed=0 (frozen), mimicking real fixture motor freeze
		const speed = state?.speed ?? 1;
		if (speed > 0) {
			const panDeg = state?.position?.[0] ?? 0;
			const tiltDeg = state?.position?.[1] ?? 0;

			if (armRef.current && Number.isFinite(panDeg)) {
				armRef.current.rotation.y = (panDeg * Math.PI) / 180;
			}
			if (headRef.current && Number.isFinite(tiltDeg)) {
				headRef.current.rotation.x = -(tiltDeg * Math.PI) / 180;
			}
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
						ref={(ref) => {
							if (ref) ref.raycast = () => {};
						}}
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
