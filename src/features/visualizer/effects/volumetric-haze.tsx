import { useFrame } from "@react-three/fiber";
import { useContext, useEffect, useMemo, useRef } from "react";
import { Euler, Matrix4, Quaternion, Vector3 } from "three";
import type { FixtureDefinition, PatchedFixture } from "@/bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import {
	type FixtureModelKind,
	getModelForFixture,
	isProcedural,
} from "../components/fixture-models";
import { PrimitiveOverrideContext } from "../hooks/use-primitive-state";
import { universeStore } from "../stores/universe-state-store";
import { HazeCompositeEffect } from "./haze-composite-effect";
import { HazeTemporalPass } from "./haze-temporal-pass";
import { MAX_LIGHTS, VolumetricHazePass } from "./volumetric-haze-pass";

// ---------------------------------------------------------------------------
// One continuous luminaire model. A fixture type is just an opening angle
// (and a lumen budget) on a single zoom axis — beam → spot → par → wash is
// one smooth sweep of `fieldAngleDeg` with no per-type special cases.
// Concentration (lumens per solid angle) derives brightness, throw, edge
// hardness, and scatter anisotropy. A live zoom channel plugs in by
// animating `fieldAngleDeg` before calling coneFromOpening.
// ---------------------------------------------------------------------------

interface LuminaireConfig {
	/** Full field angle (deg) — the fixture's opening. */
	fieldAngleDeg: number;
	/** Relative lumen output; 1 = a stock moving-head lamp. */
	lumens: number;
}

const LUMINAIRES: Partial<Record<FixtureModelKind, LuminaireConfig>> = {
	moving_head: { fieldAngleDeg: 26, lumens: 1 },
	scanner: { fieldAngleDeg: 22, lumens: 1 },
	par: { fieldAngleDeg: 70, lumens: 1 },
	strobe: { fieldAngleDeg: 100, lumens: 3 },
};

const DEFAULT_LUMINAIRE: LuminaireConfig = { fieldAngleDeg: 26, lumens: 1 };
const PIXEL_LUMINAIRE: LuminaireConfig = { fieldAngleDeg: 60, lumens: 0.6 };

interface ConeParams {
	cosBeam: number;
	cosField: number;
	range: number;
	wash: number;
	/** Intensity multiplier: lumens x solid-angle concentration. */
	gain: number;
}

function coneSolidAngle(fullAngleDeg: number): number {
	return 2 * Math.PI * (1 - Math.cos(((fullAngleDeg / 2) * Math.PI) / 180));
}

function smoothstep01(edge0: number, edge1: number, x: number): number {
	const t = Math.min(1, Math.max(0, (x - edge0) / (edge1 - edge0)));
	return t * t * (3 - 2 * t);
}

/** Concentration reference: a 30° spot has gain 1.5 and 12m of throw. */
const REF_SOLID_ANGLE = coneSolidAngle(30);

function coneFromOpening(cfg: LuminaireConfig): ConeParams {
	const fieldDeg = Math.min(160, Math.max(4, cfg.fieldAngleDeg));
	// Same energy through a smaller solid angle = hotter, whiter, longer throw.
	const concentration = REF_SOLID_ANGLE / coneSolidAngle(fieldDeg);
	// Wide openings scatter near-isotropically and develop a soft shoulder;
	// the beam:field ratio (the 50%-intensity contour of the peaked profile)
	// narrows continuously as the cone opens.
	const wash = smoothstep01(20, 80, fieldDeg);
	const beamRatio = 0.6 - 0.25 * wash;
	const halfRad = ((fieldDeg / 2) * Math.PI) / 180;
	return {
		cosField: Math.cos(halfRad),
		cosBeam: Math.cos(halfRad * beamRatio),
		range: Math.min(18, Math.max(3, 12 * Math.sqrt(concentration))),
		wash,
		gain: 1.5 * cfg.lumens * Math.min(6, Math.max(0.1, concentration)),
	};
}

const NO_BEAM_KINDS = new Set<FixtureModelKind>(["hazer", "smoke"]);
const HAZE_KINDS = NO_BEAM_KINDS;

interface ResolvedFixture {
	modelKind: FixtureModelKind | null;
	headCount: number;
	isProc: boolean;
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

const _beamDir = new Vector3();
const _qFixture = new Quaternion();
const _qPan = new Quaternion();
const _qTilt = new Quaternion();
const _euler = new Euler();
const _axisX = new Vector3(1, 0, 0);
const _axisY = new Vector3(0, 1, 0);
const _pixelWorld = new Vector3();

export interface VolumetricHazeProps {
	fixtures: PatchedFixture[];
	hazeDensity?: number;
	/** Equiangular samples per beam (shader cap 32). Default 8. */
	steps?: number;
	/** Temporal accumulation rate when still; 1/alpha ≈ frames averaged. Higher = faster + noisier. Default 0.4. */
	temporalAlpha?: number;
	/** Render-target resolution scale, e.g. 0.5 for half-res. Default 1.0. */
	resolutionScale?: number;
	/** Temporal accumulation (denoise) on the haze buffer. Default true. */
	denoise?: boolean;
}

export function VolumetricHaze({
	fixtures,
	hazeDensity = 0.5,
	steps = 8,
	temporalAlpha = 0.4,
	resolutionScale = 1.0,
	denoise = true,
}: VolumetricHazeProps) {
	const definitionsCache = useFixtureStore((s) => s.definitionsCache);
	const getDefinition = useFixtureStore((s) => s.getDefinition);
	const overrideGetter = useContext(PrimitiveOverrideContext);

	useEffect(() => {
		for (const f of fixtures) {
			if (!definitionsCache.has(f.fixturePath)) {
				getDefinition(f.fixturePath);
			}
		}
	}, [fixtures, definitionsCache, getDefinition]);

	// Construct passes + composite effect once. Chain is:
	//   hazePass -> temporalPass.input, temporalPass.output -> compositeEffect.
	// The temporal pass ping-pongs, so its output buffer alternates every frame
	// — it pushes the current buffer into the composite via setOutputConsumer
	// rather than the composite binding it once.
	const { hazePass, temporalPass, composite } = useMemo(() => {
		const haze = new VolumetricHazePass({
			hazeDensity,
			steps,
			resolutionScale,
		});
		const temporal = new HazeTemporalPass({
			alpha: temporalAlpha,
			resolutionScale,
		});
		temporal.setInputTexture(haze.texture);
		const comp = new HazeCompositeEffect(temporal.texture, resolutionScale);
		temporal.setOutputConsumer((tex) => comp.setHazeTexture(tex));
		return { hazePass: haze, temporalPass: temporal, composite: comp };
	}, []);

	const hazePassRef = useRef(hazePass);
	hazePassRef.current = hazePass;
	const temporalPassRef = useRef(temporalPass);
	temporalPassRef.current = temporalPass;
	const compositeRef = useRef(composite);
	compositeRef.current = composite;
	// Previous camera world matrix, for the motion alpha-guard in useFrame.
	const prevCam = useRef(new Matrix4());
	// Live-tunable base accumulation rate (the `,` / `.` dial keys adjust it).
	const baseAlpha = useRef(temporalAlpha);

	useEffect(() => {
		return () => {
			hazePassRef.current.dispose();
			temporalPassRef.current.dispose();
			compositeRef.current.dispose();
		};
	}, []);

	useEffect(() => {
		hazePass.material.uniforms.uHazeDensity.value = hazeDensity;
	}, [hazePass, hazeDensity]);

	useEffect(() => {
		hazePass.material.uniforms.uRaySteps.value = steps;
	}, [hazePass, steps]);

	// Denoise bypass: when off, skip the temporal accumulation and point the
	// composite at the raw haze buffer so the per-frame march shows unsmoothed.
	useEffect(() => {
		temporalPass.enabled = denoise;
		composite.setHazeTexture(denoise ? temporalPass.texture : hazePass.texture);
	}, [denoise, temporalPass, composite, hazePass]);

	// Debug mode cycle (backtick to step through 0..3) + live brightness dial.
	// `[` / `]` halve / double the beam gain, `;` / `'` adjust the white leak.
	// Tune until beams read right, then tell me the logged values to bake in.
	useEffect(() => {
		let mode = 0;
		const u = hazePass.material.uniforms;
		const handler = (e: KeyboardEvent) => {
			if (e.key === "`") {
				mode = (mode + 1) % 4;
				const labels = ["full", "no noise", "no lights", "passthrough"];
				console.log(`[haze debug] mode: ${mode} (${labels[mode]})`);
				u.uDebugMode.value = mode;
			} else if (e.key === "]") {
				u.uBeamGain.value *= 1.5;
				console.log(`[haze] uBeamGain = ${u.uBeamGain.value.toFixed(0)}`);
			} else if (e.key === "[") {
				u.uBeamGain.value /= 1.5;
				console.log(`[haze] uBeamGain = ${u.uBeamGain.value.toFixed(0)}`);
			} else if (e.key === "'") {
				u.uWhiteLeak.value = Math.min(1, u.uWhiteLeak.value * 1.3);
				console.log(`[haze] uWhiteLeak = ${u.uWhiteLeak.value.toFixed(3)}`);
			} else if (e.key === ";") {
				u.uWhiteLeak.value /= 1.3;
				console.log(`[haze] uWhiteLeak = ${u.uWhiteLeak.value.toFixed(3)}`);
			} else if (e.key === ".") {
				baseAlpha.current = Math.min(1, baseAlpha.current * 1.3);
				console.log(`[haze] temporal alpha = ${baseAlpha.current.toFixed(2)}`);
			} else if (e.key === ",") {
				baseAlpha.current = Math.max(0.02, baseAlpha.current / 1.3);
				console.log(`[haze] temporal alpha = ${baseAlpha.current.toFixed(2)}`);
			}
		};
		window.addEventListener("keydown", handler);
		return () => window.removeEventListener("keydown", handler);
	}, [hazePass]);

	useFrame((state) => {
		hazePass.mainCamera = state.camera;

		// Temporal alpha guard: while the camera moves, the history is stale
		// (no reprojection yet), so weight fully on the current frame to avoid
		// ghost trails. When still, decay to the accumulating alpha.
		const moved = !state.camera.matrixWorld.equals(prevCam.current);
		temporalPass.setAlpha(moved ? 1.0 : baseAlpha.current);
		prevCam.current.copy(state.camera.matrixWorld);

		const time = state.clock.getElapsedTime();
		const getPrimitive = overrideGetter
			? overrideGetter()
			: universeStore.getPrimitive;

		let hazerLevel = 0;
		for (const fixture of fixtures) {
			const { modelKind } = resolveFixture(fixture, definitionsCache);
			if (modelKind && HAZE_KINDS.has(modelKind)) {
				const s = getPrimitive(`${fixture.id}:0`);
				if (s) hazerLevel = Math.max(hazerLevel, s.dimmer);
			}
		}

		const effectiveDensity = hazeDensity * (0.3 + 0.7 * hazerLevel);
		hazePass.material.uniforms.uHazeDensity.value = effectiveDensity;

		let lightIdx = 0;

		for (const fixture of fixtures) {
			if (lightIdx >= MAX_LIGHTS) break;

			const { modelKind, headCount, isProc, pixelPositions } = resolveFixture(
				fixture,
				definitionsCache,
			);

			if (isProc && pixelPositions) {
				_euler.set(fixture.rotX, fixture.rotZ, fixture.rotY);
				_qFixture.setFromEuler(_euler);
				_beamDir.set(0, 0, 1);
				_beamDir.applyQuaternion(_qFixture);
				_beamDir.normalize();

				const fxX = fixture.posX;
				const fxY = fixture.posZ;
				const fxZ = fixture.posY;

				const cone = coneFromOpening(PIXEL_LUMINAIRE);
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

					const pixIdx = Math.min(
						Math.floor(h * pixelsPerHead + pixelsPerHead / 2),
						pixelPositions.length - 1,
					);
					const lp = pixelPositions[pixIdx];

					_pixelWorld.set(lp[0], lp[1], lp[2]);
					_pixelWorld.applyQuaternion(_qFixture);
					_pixelWorld.x += fxX;
					_pixelWorld.y += fxY;
					_pixelWorld.z += fxZ;

					hazePass.setLight(
						lightIdx,
						_pixelWorld.x,
						_pixelWorld.y,
						_pixelWorld.z,
						// Normalize by sqrt(emitter count) so a 16-pixel bar isn't
						// 16x a spot — overlapping pixel cones sum in the haze, so
						// brightness must be balanced per-fixture, not per-pixel.
						(intensity * cone.gain) / Math.sqrt(Math.max(1, headCount)),
						_beamDir.x,
						_beamDir.y,
						_beamDir.z,
						cone.cosBeam,
						cone.cosField,
						color[0],
						color[1],
						color[2],
						cone.range,
						cone.wash,
					);
					lightIdx++;
				}
				continue;
			}

			if (!modelKind || NO_BEAM_KINDS.has(modelKind)) continue;

			const cone = coneFromOpening(LUMINAIRES[modelKind] ?? DEFAULT_LUMINAIRE);
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

			hazePass.setLight(
				lightIdx,
				posX,
				posY,
				posZ,
				intensity * cone.gain,
				_beamDir.x,
				_beamDir.y,
				_beamDir.z,
				cone.cosBeam,
				cone.cosField,
				color[0],
				color[1],
				color[2],
				cone.range,
				cone.wash,
			);
			lightIdx++;
		}

		hazePass.commitLights(lightIdx, time);
	});

	// Return all three as siblings — react-postprocessing adds raw Pass
	// instances directly and bundles Effect instances into EffectPasses.
	return (
		<>
			<primitive object={hazePass} />
			<primitive object={temporalPass} />
			<primitive object={composite} />
		</>
	);
}
