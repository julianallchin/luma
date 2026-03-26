import { useFrame } from "@react-three/fiber";
import { useContext, useEffect, useMemo, useRef } from "react";
import { Euler, Quaternion, Vector3 } from "three";
import type { FixtureDefinition, PatchedFixture } from "@/bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import {
	type FixtureModelKind,
	getModelForFixture,
	isProcedural,
} from "../components/fixture-models";
import { PrimitiveOverrideContext } from "../hooks/use-primitive-state";
import { universeStore } from "../stores/universe-state-store";
import {
	MAX_LIGHTS,
	VolumetricHazeEffect,
	type VolumetricHazeOptions,
} from "./volumetric-haze-effect";

// ---------------------------------------------------------------------------
// Beam config (mirrors static-fixture.tsx BEAM_CONFIG)
// ---------------------------------------------------------------------------

interface BeamVolumetricConfig {
	angleDeg: number;
	length: number;
	softness: number;
	/** 0 = tight spot beam, 1 = wide wash flood */
	wash: number;
}

const BEAM_CONFIG: Partial<Record<FixtureModelKind, BeamVolumetricConfig>> = {
	par: { angleDeg: 45, length: 3, softness: 0.5, wash: 1 },
	moving_head: { angleDeg: 30, length: 12, softness: 0.6, wash: 0 },
	scanner: { angleDeg: 24, length: 12, softness: 0.5, wash: 0 },
	strobe: { angleDeg: 70, length: 2.5, softness: 0.6, wash: 1 },
};

const DEFAULT_BEAM: BeamVolumetricConfig = {
	angleDeg: 30,
	length: 5,
	softness: 0.35,
	wash: 0,
};

/** Config for individual matrix/LED bar pixels. */
const PIXEL_BEAM: BeamVolumetricConfig = {
	angleDeg: 50,
	length: 2.5,
	softness: 0.5,
	wash: 1,
};

const NO_BEAM_KINDS = new Set<FixtureModelKind>(["hazer", "smoke"]);
const HAZE_KINDS = NO_BEAM_KINDS; // hazer/smoke fixtures drive haze density

// ---------------------------------------------------------------------------
// Types for the fixture info we need
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// React component — lives inside <EffectComposer>
// ---------------------------------------------------------------------------

interface VolumetricHazeProps extends VolumetricHazeOptions {
	fixtures: PatchedFixture[];
}

interface ResolvedFixture {
	modelKind: FixtureModelKind | null;
	headCount: number;
	isProc: boolean;
	/** Local-space pixel positions for procedural fixtures (Three.js Y-up). */
	pixelPositions: [number, number, number][] | null;
}

function resolveFixture(
	fixture: PatchedFixture,
	cache: Map<string, FixtureDefinition>,
): ResolvedFixture {
	const def = cache.get(fixture.fixturePath);
	if (!def)
		return {
			modelKind: null,
			headCount: 1,
			isProc: false,
			pixelPositions: null,
		};
	const proc = isProcedural(def);
	if (proc) {
		const mode = def.Mode.find((m) => m["@Name"] === fixture.modeName);
		const headCount = mode?.Head?.length || 1;

		// Compute pixel grid positions in local space (same logic as procedural-fixture.tsx)
		let { Dimensions: dims, Layout: layout } = def.Physical || {};
		if (
			(!layout || (layout["@Width"] === 1 && layout["@Height"] === 1)) &&
			headCount > 1
		) {
			layout = { "@Width": headCount, "@Height": 1 };
		}
		const width = (dims?.["@Width"] || 200) / 1000;
		const height = (dims?.["@Height"] || 200) / 1000;
		const depth = (dims?.["@Depth"] || 200) / 1000;
		const lw = layout?.["@Width"] || 1;
		const lh = layout?.["@Height"] || 1;
		const hw = width / lw;
		const hh = height / lh;

		const positions: [number, number, number][] = [];
		const startX = -width / 2 + hw / 2;
		const startY = height / 2 - hh / 2;
		for (let y = 0; y < lh; y++) {
			for (let x = 0; x < lw; x++) {
				// Local space: X right, Y up, Z forward (face)
				// Convert to Three.js Y-up for the fixture group transform
				positions.push([startX + x * hw, startY - y * hh, depth / 2 + 0.001]);
			}
		}

		return {
			modelKind: null,
			headCount,
			isProc: true,
			pixelPositions: positions,
		};
	}
	const info = getModelForFixture(def);
	return {
		modelKind: info?.kind ?? null,
		headCount: 1,
		isProc: false,
		pixelPositions: null,
	};
}

// Reusable math objects to avoid allocations in the render loop
const _beamDir = new Vector3();
const _qFixture = new Quaternion();
const _qPan = new Quaternion();
const _qTilt = new Quaternion();
const _euler = new Euler();
const _axisX = new Vector3(1, 0, 0);
const _axisY = new Vector3(0, 1, 0);
const _pixelWorld = new Vector3();

export function VolumetricHaze({
	fixtures,
	hazeDensity = 0.5,
	steps = 24,
}: VolumetricHazeProps) {
	const definitionsCache = useFixtureStore((s) => s.definitionsCache);
	const getDefinition = useFixtureStore((s) => s.getDefinition);
	const overrideGetter = useContext(PrimitiveOverrideContext);

	// Ensure all fixture definitions are loaded
	useEffect(() => {
		for (const f of fixtures) {
			if (!definitionsCache.has(f.fixturePath)) {
				getDefinition(f.fixturePath);
			}
		}
	}, [fixtures, definitionsCache, getDefinition]);
	const effect = useMemo(() => {
		return new VolumetricHazeEffect({ hazeDensity, steps });
	}, []);

	// Track the effect ref for cleanup
	const effectRef = useRef(effect);
	effectRef.current = effect;

	useEffect(() => {
		return () => {
			effectRef.current.dispose();
		};
	}, []);

	// Update options without recreating the effect
	useEffect(() => {
		(effect.uniforms.get("uHazeDensity") as { value: number }).value =
			hazeDensity;
	}, [effect, hazeDensity]);

	// Debug mode: press 1-4 in console to toggle. Cycle with keyboard.
	useEffect(() => {
		let mode = 0;
		const handler = (e: KeyboardEvent) => {
			if (e.key === "`") {
				mode = (mode + 1) % 4;
				const labels = ["full", "no noise", "no lights", "passthrough"];
				console.log(`[haze debug] mode: ${mode} (${labels[mode]})`);
				(effect.uniforms.get("uDebugMode") as { value: number }).value = mode;
			}
		};
		window.addEventListener("keydown", handler);
		return () => window.removeEventListener("keydown", handler);
	}, [effect]);

	useEffect(() => {
		(effect.uniforms.get("uRaySteps") as { value: number }).value = steps;
	}, [effect, steps]);

	// Per-frame: collect light data from DMX state and upload
	useFrame((state) => {
		effect.mainCamera = state.camera;

		const time = state.clock.getElapsedTime();
		const getPrimitive = overrideGetter
			? overrideGetter()
			: universeStore.getPrimitive;

		// Read haze density from hazer/smoke fixtures — max dimmer wins
		let hazerLevel = 0;
		for (const fixture of fixtures) {
			const { modelKind } = resolveFixture(fixture, definitionsCache);
			if (modelKind && HAZE_KINDS.has(modelKind)) {
				const s = getPrimitive(`${fixture.id}:0`);
				if (s) hazerLevel = Math.max(hazerLevel, s.dimmer);
			}
		}

		// Modulate base density: when no hazer is active, use 30% of base
		// (some ambient haze is always present in a venue). At full hazer,
		// use 100% of the base density prop.
		const effectiveDensity = hazeDensity * (0.3 + 0.7 * hazerLevel);
		(effect.uniforms.get("uHazeDensity") as { value: number }).value =
			effectiveDensity;

		let lightIdx = 0;

		for (const fixture of fixtures) {
			if (lightIdx >= MAX_LIGHTS) break;

			const { modelKind, headCount, isProc, pixelPositions } = resolveFixture(
				fixture,
				definitionsCache,
			);

			if (isProc && pixelPositions) {
				// --- Procedural (LED matrix/bar): each head is a pixel light ---
				_euler.set(fixture.rotX, fixture.rotZ, fixture.rotY);
				_qFixture.setFromEuler(_euler);

				// Pixels emit forward in local +Z
				_beamDir.set(0, 0, 1);
				_beamDir.applyQuaternion(_qFixture);
				_beamDir.normalize();

				// Fixture world origin (Z-up → Y-up)
				const fxX = fixture.posX;
				const fxY = fixture.posZ;
				const fxZ = fixture.posY;

				const cfg = PIXEL_BEAM;
				const coneRad = (cfg.angleDeg / 2) * (Math.PI / 180);
				const pixelsPerHead = pixelPositions.length / Math.max(1, headCount);

				for (let h = 0; h < headCount; h++) {
					if (lightIdx >= MAX_LIGHTS) break;
					const ps = getPrimitive(`${fixture.id}:${h}`);
					let intensity = ps?.dimmer ?? 0;
					if (intensity < 0.01) continue;

					if (ps && ps.strobe > 0) {
						const hz = ps.strobe * 10;
						if (hz > 0) {
							const period = 1 / hz;
							if (time % period > period * 0.5) intensity = 0;
						}
						if (intensity < 0.01) continue;
					}

					const color = ps?.color ?? [0, 0, 0];

					// Use the center pixel of this head's pixel range
					const pixIdx = Math.min(
						Math.floor(h * pixelsPerHead + pixelsPerHead / 2),
						pixelPositions.length - 1,
					);
					const lp = pixelPositions[pixIdx];

					// Transform local pixel position to world space
					_pixelWorld.set(lp[0], lp[1], lp[2]);
					_pixelWorld.applyQuaternion(_qFixture);
					_pixelWorld.x += fxX;
					_pixelWorld.y += fxY;
					_pixelWorld.z += fxZ;

					effect.setLight(
						lightIdx,
						_pixelWorld.x,
						_pixelWorld.y,
						_pixelWorld.z,
						intensity,
						_beamDir.x,
						_beamDir.y,
						_beamDir.z,
						coneRad,
						color[0],
						color[1],
						color[2],
						cfg.length,
						cfg.softness,
						cfg.wash,
					);
					lightIdx++;
				}
				continue;
			}

			// --- Static fixtures (par, mover, scanner, etc.) ---
			if (!modelKind || NO_BEAM_KINDS.has(modelKind)) continue;

			const beamCfg = BEAM_CONFIG[modelKind] ?? DEFAULT_BEAM;
			const primitiveState = getPrimitive(`${fixture.id}:0`);

			let intensity = primitiveState?.dimmer ?? 0;
			if (intensity < 0.01) continue;

			if (primitiveState && primitiveState.strobe > 0) {
				const hz = primitiveState.strobe * 20;
				if (hz > 0) {
					const period = 1 / hz;
					if (time % period > period * 0.5) intensity = 0;
				}
				if (intensity < 0.01) continue;
			}

			const color = primitiveState?.color ?? [0, 0, 0];
			const panDeg = primitiveState?.position?.[0] ?? 0;
			const tiltDeg = primitiveState?.position?.[1] ?? 0;

			// Beam direction: model (0,-1,0) → tilt → pan → fixture rotation
			_beamDir.set(0, -1, 0);
			_qTilt.setFromAxisAngle(_axisX, -(tiltDeg * Math.PI) / 180);
			_beamDir.applyQuaternion(_qTilt);
			_qPan.setFromAxisAngle(_axisY, (panDeg * Math.PI) / 180);
			_beamDir.applyQuaternion(_qPan);
			_euler.set(fixture.rotX, fixture.rotZ, fixture.rotY);
			_qFixture.setFromEuler(_euler);
			_beamDir.applyQuaternion(_qFixture);
			_beamDir.normalize();

			const posX = fixture.posX;
			const posY = fixture.posZ;
			const posZ = fixture.posY;
			const coneAngleRad = (beamCfg.angleDeg / 2) * (Math.PI / 180);

			effect.setLight(
				lightIdx,
				posX,
				posY,
				posZ,
				intensity,
				_beamDir.x,
				_beamDir.y,
				_beamDir.z,
				coneAngleRad,
				color[0],
				color[1],
				color[2],
				beamCfg.length,
				beamCfg.softness,
				beamCfg.wash,
			);
			lightIdx++;
		}

		effect.commitLights(lightIdx, time);
	});

	// Render as a primitive inside EffectComposer
	return <primitive object={effect} />;
}
