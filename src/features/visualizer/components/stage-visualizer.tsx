import { OrbitControls } from "@react-three/drei";
import { Canvas, useFrame, useThree } from "@react-three/fiber";

import { Bloom, EffectComposer } from "@react-three/postprocessing";
import {
	Box,
	Circle,
	FlipHorizontal2,
	LocateFixed,
	Move,
	Orbit,
	RotateCw,
} from "lucide-react";
import type { EffectComposer as EffectComposerImpl } from "postprocessing";
import {
	Suspense,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import type { Camera, PerspectiveCamera } from "three";
import {
	DoubleSide,
	HalfFloatType,
	PlaneGeometry,
	ShaderMaterial,
	Vector2,
	Vector3,
} from "three";
import type { OrbitControls as OrbitControlsImpl } from "three-stdlib";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { HazeDenoise } from "../effects/haze-denoise";
import { VolumetricHaze } from "../effects/volumetric-haze";
import {
	disposeSpotlightPool,
	initSpotlightPool,
	beginFrame as poolBeginFrame,
	endFrame as poolEndFrame,
	setPoolConfig,
} from "../lib/spotlight-pool";
import { universeStore } from "../stores/universe-state-store";
import { useCameraStore } from "../stores/use-camera-store";
import { useRenderSettingsStore } from "../stores/use-render-settings-store";
import { CircleFitDebug } from "./circle-fit-debug";
import { FixtureGroup } from "./fixture-group";
import { MirrorDebug } from "./mirror-debug";
import { MovementPyramids } from "./movement-pyramids";

interface StageVisualizerProps {
	/**
	 * Whether to allow selecting and transforming fixtures.
	 * Enable this in the Universe editor.
	 */
	enableEditing?: boolean;
	/**
	 * Absolute audio time (seconds) to render against for interpolation.
	 */
	renderAudioTimeSec?: number | null;
	/**
	 * Force dark stage off (lit environment). Used in the Universe editor.
	 */
	forceLightStage?: boolean;
	/**
	 * Ref populated with a handle for offline video export. The consumer
	 * (track-editor export flow) calls `beginExport/renderFrame/endExport`.
	 */
	exportHandleRef?: React.MutableRefObject<StageExportHandle | null>;
}

type TransformMode = "translate" | "rotate";
type TransformPivot = "individual" | "group";
type RenderMetrics = { fps: number; deltaMs: number };

// ---------------------------------------------------------------------------
// Custom floor grid — distance-fading shader, no depth writes (EffectComposer-safe)
// ---------------------------------------------------------------------------

const GRID_VERTEX = /* glsl */ `
varying vec3 vWorldPos;
void main() {
  vec4 wp = modelMatrix * vec4(position, 1.0);
  vWorldPos = wp.xyz;
  gl_Position = projectionMatrix * viewMatrix * wp;
}
`;

const GRID_FRAGMENT = /* glsl */ `
uniform float uCellSize;
uniform float uSectionSize;
uniform vec3 uCellColor;
uniform vec3 uSectionColor;
uniform float uFadeDistance;
uniform float uFadeStrength;
uniform float uOpacity;

varying vec3 vWorldPos;

float gridLine(vec2 coord, float size, float thickness) {
  vec2 fw = fwidth(coord / size);
  vec2 grid = abs(fract(coord / size - 0.5) - 0.5);
  vec2 line = smoothstep(fw * (thickness + 0.5), fw * 0.5, grid);
  return max(line.x, line.y);
}

void main() {
  vec2 coord = vWorldPos.xz;
  float dist = length(vWorldPos - cameraPosition);

  float fade = 1.0 - smoothstep(uFadeDistance * 0.3, uFadeDistance, dist);
  fade = pow(fade, uFadeStrength);

  float minor = gridLine(coord, uCellSize, 0.5);
  float major = gridLine(coord, uSectionSize, 1.5);

  vec3 color = mix(uCellColor, uSectionColor, major);
  float alpha = max(minor * 0.02, major * 0.09) * fade * uOpacity;

  if (alpha < 0.001) discard;
  gl_FragColor = vec4(color, alpha);
}
`;

function FadingGrid() {
	const mesh = useMemo(() => {
		const geo = new PlaneGeometry(200, 200);
		geo.rotateX(-Math.PI / 2);
		const mat = new ShaderMaterial({
			vertexShader: GRID_VERTEX,
			fragmentShader: GRID_FRAGMENT,
			uniforms: {
				uCellSize: { value: 0.5 },
				uSectionSize: { value: 3.0 },
				uCellColor: { value: [0.506, 0.631, 0.757] }, // #81a1c1
				uSectionColor: { value: [0.302, 0.439, 0.478] }, // #4d707a
				uFadeDistance: { value: 50.0 },
				uFadeStrength: { value: 2.0 },
				uOpacity: { value: 0.6 },
			},
			transparent: true,
			depthWrite: false,
			side: DoubleSide,
		});
		return { geo, mat };
	}, []);

	useEffect(() => {
		return () => {
			mesh.geo.dispose();
			mesh.mat.dispose();
		};
	}, [mesh]);

	return (
		<mesh geometry={mesh.geo} material={mesh.mat} position={[0, 0.002, 0]} />
	);
}

function RenderMetricsProbe({
	metricsRef,
}: {
	metricsRef: React.MutableRefObject<RenderMetrics>;
}) {
	useFrame((_, delta) => {
		const deltaMs = delta * 1000;
		const fps = delta > 0 ? 1 / delta : metricsRef.current.fps;
		const smoothed = metricsRef.current.fps
			? metricsRef.current.fps * 0.9 + fps * 0.1
			: fps;

		metricsRef.current = { fps: smoothed, deltaMs };
	});

	return null;
}

function RenderTimeSync({ getTime }: { getTime: () => number | null }) {
	useFrame(() => {
		universeStore.setRenderAudioTime(getTime());
	});
	return null;
}

function CameraController({
	controlsRef,
}: {
	controlsRef: React.RefObject<OrbitControlsImpl | null>;
}) {
	const { camera } = useThree();
	const { position, target, setCamera } = useCameraStore();
	const initialized = useRef(false);
	const suppressSync = useRef(false);

	// Apply camera position from store (on mount + external resets)
	useEffect(() => {
		if (!controlsRef.current) return;
		// On first mount, always apply. After that, only apply external resets
		// (detected by suppression flag not being set from our own handleChange).
		if (!initialized.current || !suppressSync.current) {
			camera.position.set(...position);
			controlsRef.current.target.set(...target);
			controlsRef.current.update();
			initialized.current = true;
		}
		suppressSync.current = false;
	}, [camera, controlsRef, position, target]);

	// Save camera position on OrbitControls change
	useEffect(() => {
		const controls = controlsRef.current;
		if (!controls) return;

		const handleChange = () => {
			const pos = camera.position.toArray() as [number, number, number];
			const tgt = controls.target.toArray() as [number, number, number];
			suppressSync.current = true;
			setCamera(pos, tgt);
		};

		controls.addEventListener("end", handleChange);
		return () => {
			controls.removeEventListener("end", handleChange);
		};
	}, [camera, controlsRef, setCamera]);

	return null;
}

/** Syncs the Three.js camera FOV with the render-settings store. */
function FovSync() {
	const fov = useRenderSettingsStore((s) => s.fov ?? 50);
	const { camera } = useThree();

	useEffect(() => {
		if ("fov" in camera) {
			(camera as PerspectiveCamera).fov = fov;
			(camera as PerspectiveCamera).updateProjectionMatrix();
		}
	}, [camera, fov]);

	return null;
}

/** Exposes Three.js camera and canvas size to the outer component via refs. */
function CameraExposer({
	cameraRef,
	sizeRef,
}: {
	cameraRef: React.MutableRefObject<Camera | null>;
	sizeRef: React.MutableRefObject<{ width: number; height: number }>;
}) {
	const { camera, size } = useThree();
	cameraRef.current = camera;
	sizeRef.current = size;
	return null;
}

/**
 * Handle exposed by `StageVisualizer` for offline video export.
 *
 * The live EffectComposer is temporarily resized to the export resolution and
 * driven manually via R3F's `advance()`. The composer writes to the canvas's
 * default framebuffer; we read those pixels back. This includes volumetric
 * haze, haze denoise, and bloom — i.e. whatever the preview shows.
 *
 * `advance(t * 1000, true)` updates R3F's internal clock with our export
 * timestamp, which makes the haze shader's time-driven noise drift
 * deterministic per-frame across runs.
 */
export interface FrameTimings {
	advanceMs: number;
	renderMs: number;
	readPixelsMs: number;
}

export interface StageExportHandle {
	beginExport(width: number, height: number): void;
	renderFrame(timeSec: number, out: Uint8Array): FrameTimings;
	endExport(): void;
}

function ExportHandleExposer({
	handleRef,
	composerRef,
}: {
	handleRef: React.MutableRefObject<StageExportHandle | null>;
	composerRef: React.MutableRefObject<EffectComposerImpl | null>;
}) {
	// Non-reactive accessor into the R3F store.
	const get = useThree((s) => s.get);

	useEffect(() => {
		let savedFrameloop: "always" | "demand" | "never" = "always";
		let savedWidth = 0;
		let savedHeight = 0;
		let savedPixelRatio = 1;
		let exportWidth = 0;
		let exportHeight = 0;
		let active = false;

		handleRef.current = {
			beginExport(width, height) {
				active = true;
				exportWidth = width;
				exportHeight = height;

				const state = get();
				savedFrameloop = state.frameloop;
				savedPixelRatio = state.gl.getPixelRatio();
				const sz = state.gl.getSize(new Vector2());
				savedWidth = sz.x;
				savedHeight = sz.y;

				state.set({ frameloop: "never" });
				// Force exact 1:1 pixel mapping: the export resolution is the
				// renderer's backing-store resolution, not DPR-multiplied.
				state.gl.setPixelRatio(1);
				// `updateStyle=false` keeps the visible canvas at its DOM size,
				// so the on-screen preview isn't visibly resized during export.
				state.gl.setSize(width, height, false);
				composerRef.current?.setSize(width, height);
			},
			renderFrame(timeSec, out) {
				if (!active) return { advanceMs: 0, renderMs: 0, readPixelsMs: 0 };
				const state = get();
				const t0 = performance.now();
				// Runs fixture useFrames (universe → materials/lights) and then
				// the EffectComposer's render pass which writes the composited
				// frame to the canvas's default framebuffer.
				state.advance(timeSec * 1000, true);
				const t1 = performance.now();
				const ctx = state.gl.getContext();
				// Ensure we're reading from the canvas FBO, not whatever the
				// composer left bound internally.
				ctx.bindFramebuffer(ctx.FRAMEBUFFER, null);
				ctx.readPixels(
					0,
					0,
					exportWidth,
					exportHeight,
					ctx.RGBA,
					ctx.UNSIGNED_BYTE,
					out,
				);
				const t2 = performance.now();
				return {
					advanceMs: t1 - t0,
					// The composer renders during advance(); no separate render step.
					renderMs: 0,
					readPixelsMs: t2 - t1,
				};
			},
			endExport() {
				active = false;
				const state = get();
				state.gl.setPixelRatio(savedPixelRatio);
				state.gl.setSize(savedWidth, savedHeight, false);
				composerRef.current?.setSize(savedWidth, savedHeight);
				state.set({ frameloop: savedFrameloop });
			},
		};

		return () => {
			handleRef.current = null;
		};
	}, [get, handleRef, composerRef]);

	return null;
}

const HISTORY_LEN = 60;

function FpsSparkline({
	data,
	color,
	height = 28,
}: {
	data: number[];
	color: string;
	height?: number;
}) {
	const width = 120;
	const max = Math.max(120, ...data);

	if (data.length < 2) return <div style={{ width, height }} />;

	const points = data
		.map((v, i) => {
			const x = (i / (HISTORY_LEN - 1)) * width;
			const y = height - (v / max) * (height - 2) - 1;
			return `${x},${y}`;
		})
		.join(" ");

	// Fill area — close straight down from last data point, not to canvas edge
	const lastX = ((data.length - 1) / (HISTORY_LEN - 1)) * width;
	const fillPoints = `0,${height} ${points} ${lastX},${height}`;

	return (
		<svg
			width={width}
			height={height}
			className="shrink-0"
			role="img"
			aria-label="FPS sparkline"
		>
			<defs>
				<linearGradient id={`grad-${color}`} x1="0" y1="0" x2="0" y2="1">
					<stop offset="0%" stopColor={color} stopOpacity={0.3} />
					<stop offset="100%" stopColor={color} stopOpacity={0.03} />
				</linearGradient>
			</defs>
			<polygon points={fillPoints} fill={`url(#grad-${color})`} />
			<polyline
				points={points}
				fill="none"
				stroke={color}
				strokeWidth={1.5}
				strokeLinejoin="round"
			/>
		</svg>
	);
}

function StageFpsOverlay({
	renderMetricsRef,
}: {
	renderMetricsRef: React.MutableRefObject<RenderMetrics>;
}) {
	const [open, setOpen] = useState(false);
	const [metrics, setMetrics] = useState({
		signalFps: 0,
		bufferReadFps: 0,
		renderFps: 0,
	});
	const historyRef = useRef({
		signal: [] as number[],
		bufferRead: [] as number[],
		render: [] as number[],
	});

	useEffect(() => {
		const id = window.setInterval(() => {
			const signal = universeStore.getSignalMetrics();
			const render = renderMetricsRef.current;

			const h = historyRef.current;
			const push = (arr: number[], v: number) => {
				arr.push(v);
				if (arr.length > HISTORY_LEN) arr.shift();
			};
			push(h.signal, signal.fps ?? 0);
			push(h.bufferRead, signal.readFps ?? 0);
			push(h.render, render.fps ?? 0);

			setMetrics({
				signalFps: signal.fps ?? 0,
				bufferReadFps: signal.readFps ?? 0,
				renderFps: render.fps ?? 0,
			});
		}, 300);

		return () => clearInterval(id);
	}, [renderMetricsRef]);

	const h = historyRef.current;

	return (
		<div className="absolute bottom-2 right-2 z-10">
			<button
				type="button"
				onClick={() => setOpen((v) => !v)}
				className="px-2 py-1 bg-neutral-900/90 text-[10px] text-neutral-200 font-mono backdrop-blur-sm border border-neutral-800 shadow-sm hover:border-neutral-700 transition-colors"
				title="Universe/render frame rates"
			>
				sig {metrics.signalFps.toFixed(0)} / read{" "}
				{metrics.bufferReadFps.toFixed(0)} / render{" "}
				{metrics.renderFps.toFixed(0)} fps
			</button>
			{open && (
				<div className="mt-1 p-2 bg-neutral-950/95 border border-neutral-800 backdrop-blur-sm text-[11px] font-mono text-neutral-200 w-[200px]">
					<div className="space-y-2">
						<div>
							<div className="flex justify-between mb-0.5">
								<span style={{ color: "#00ffcc" }}>signal</span>
								<span style={{ color: "#00ffcc" }}>
									{metrics.signalFps.toFixed(0)}
								</span>
							</div>
							<FpsSparkline data={[...h.signal]} color="#00ffcc" />
						</div>
						<div>
							<div className="flex justify-between mb-0.5">
								<span style={{ color: "#ff44cc" }}>buffer read</span>
								<span style={{ color: "#ff44cc" }}>
									{metrics.bufferReadFps.toFixed(0)}
								</span>
							</div>
							<FpsSparkline data={[...h.bufferRead]} color="#ff44cc" />
						</div>
						<div>
							<div className="flex justify-between mb-0.5">
								<span style={{ color: "#ffcc00" }}>render</span>
								<span style={{ color: "#ffcc00" }}>
									{metrics.renderFps.toFixed(0)}
								</span>
							</div>
							<FpsSparkline data={[...h.render]} color="#ffcc00" />
						</div>
					</div>
				</div>
			)}
		</div>
	);
}

function SpotlightPoolManager() {
	const { scene } = useThree();
	const spotlightCount = useRenderSettingsStore((s) => s.spotlightCount);
	const shadows = useRenderSettingsStore((s) => s.shadows);
	const enabled = useRenderSettingsStore((s) => s.fixtureSpotlights);

	useEffect(() => {
		initSpotlightPool(scene);
		return () => disposeSpotlightPool(scene);
	}, [scene]);

	useEffect(() => {
		setPoolConfig(enabled ? spotlightCount : 0, shadows);
	}, [spotlightCount, shadows, enabled]);

	useFrame(() => poolBeginFrame(), -1);
	return null;
}

function SpotlightPoolEndFrame() {
	// Priority 0.5 doesn't exist — R3F supports any number.
	// Fixtures run at default priority 0, EffectComposer at 1.
	// We finalize at 0.5 so it's after fixtures but before composer.
	useFrame(() => poolEndFrame(), 0.5);
	return null;
}

function DarkFloor() {
	return (
		<mesh rotation={[-Math.PI / 2, 0, 0]} receiveShadow>
			<planeGeometry args={[200, 200]} />
			<meshStandardMaterial color="#030303" roughness={0.95} />
		</mesh>
	);
}

export function StageVisualizer({
	enableEditing = false,
	renderAudioTimeSec = null,
	forceLightStage = false,
	exportHandleRef,
}: StageVisualizerProps) {
	const darkStageSetting = useRenderSettingsStore((s) => s.darkStage);
	const darkStage = forceLightStage ? false : darkStageSetting;
	const clearSelection = useFixtureStore((state) => state.clearSelection);
	const selectFixturesByIds = useFixtureStore(
		(state) => state.selectFixturesByIds,
	);
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);
	const renderSettings = useRenderSettingsStore();
	const selectionSize = useFixtureStore(
		(state) => state.selectedPatchedIds.size,
	);
	const [transformMode, setTransformMode] =
		useState<TransformMode>("translate");
	const [transformPivot, setTransformPivot] =
		useState<TransformPivot>("individual");
	const [showCircleFit, setShowCircleFit] = useState(false);
	const [showGroupBounds, setShowGroupBounds] = useState(false);
	const [showMirror, setShowMirror] = useState(false);
	const [isHovered, setIsHovered] = useState(false);
	const renderMetricsRef = useRef<RenderMetrics>({ fps: 0, deltaMs: 0 });
	const renderTimeRef = useRef<number | null>(renderAudioTimeSec ?? null);
	const controlsRef = useRef<OrbitControlsImpl | null>(null);

	// Marquee selection state
	const marqueeJustFinished = useRef(false);
	const [marqueeActive, setMarqueeActive] = useState(false);
	const [marqueeRect, setMarqueeRect] = useState<{
		x1: number;
		y1: number;
		x2: number;
		y2: number;
	} | null>(null);
	const sectionRef = useRef<HTMLElement | null>(null);
	const cameraRef = useRef<Camera | null>(null);
	const canvasSizeRef = useRef<{ width: number; height: number }>({
		width: 0,
		height: 0,
	});
	const composerRef = useRef<EffectComposerImpl | null>(null);

	// Initialize Universe State Listener
	useEffect(() => {
		const unlistenPromise = universeStore.init();
		return () => {
			unlistenPromise.then((unlisten) => unlisten());
		};
	}, []);

	useEffect(() => {
		renderTimeRef.current = renderAudioTimeSec ?? null;
	}, [renderAudioTimeSec]);

	useEffect(() => {
		if (!enableEditing) return;

		const handleKeyDown = (e: KeyboardEvent) => {
			if (!isHovered) return; // Only handle keys if mouse is over the canvas

			// Unity-style hotkeys
			if (e.key.toLowerCase() === "w") setTransformMode("translate");
			if (e.key.toLowerCase() === "e") setTransformMode("rotate");
			if (e.key.toLowerCase() === "q")
				setTransformPivot((p) => (p === "individual" ? "group" : "individual"));
		};

		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [enableEditing, isHovered]);

	// Marquee handlers
	const handleMarqueeDown = useCallback(
		(e: React.MouseEvent) => {
			if (!enableEditing || !e.shiftKey || e.button !== 0) return;
			const rect = sectionRef.current?.getBoundingClientRect();
			if (!rect) return;
			const x = e.clientX - rect.left;
			const y = e.clientY - rect.top;
			setMarqueeRect({ x1: x, y1: y, x2: x, y2: y });
			setMarqueeActive(true);
		},
		[enableEditing],
	);

	const handleMarqueeMove = useCallback(
		(e: React.MouseEvent) => {
			if (!marqueeActive || !marqueeRect) return;
			const rect = sectionRef.current?.getBoundingClientRect();
			if (!rect) return;
			setMarqueeRect((prev) =>
				prev
					? {
							...prev,
							x2: e.clientX - rect.left,
							y2: e.clientY - rect.top,
						}
					: null,
			);
		},
		[marqueeActive, marqueeRect],
	);

	const handleMarqueeUp = useCallback(() => {
		if (!marqueeActive || !marqueeRect) return;

		const camera = cameraRef.current;
		const size = canvasSizeRef.current;
		if (camera && size.width > 0) {
			const left = Math.min(marqueeRect.x1, marqueeRect.x2);
			const right = Math.max(marqueeRect.x1, marqueeRect.x2);
			const top = Math.min(marqueeRect.y1, marqueeRect.y2);
			const bottom = Math.max(marqueeRect.y1, marqueeRect.y2);

			// Only process if the marquee is bigger than a few pixels (avoid accidental clicks)
			if (right - left > 5 || bottom - top > 5) {
				const hits: string[] = [];
				const vec = new Vector3();

				for (const f of patchedFixtures) {
					// Z-up (data) to Y-up (Three.js): swap Y↔Z
					vec.set(f.posX, f.posZ, f.posY);
					vec.project(camera);
					const px = (vec.x * 0.5 + 0.5) * size.width;
					const py = (-vec.y * 0.5 + 0.5) * size.height;

					if (px >= left && px <= right && py >= top && py <= bottom) {
						hits.push(f.id);
					}
				}

				if (hits.length > 0) {
					selectFixturesByIds(hits);
				}
				marqueeJustFinished.current = true;
			}
		}

		setMarqueeActive(false);
		setMarqueeRect(null);
	}, [marqueeActive, marqueeRect, patchedFixtures, selectFixturesByIds]);

	return (
		<section
			ref={sectionRef}
			className="absolute inset-0 bg-background"
			onMouseEnter={() => setIsHovered(true)}
			onMouseLeave={() => setIsHovered(false)}
			onMouseDown={handleMarqueeDown}
			onMouseMove={handleMarqueeMove}
			onMouseUp={handleMarqueeUp}
			aria-label="3D Stage Visualizer"
		>
			{/* Marquee overlay */}
			{marqueeActive && marqueeRect && (
				<div
					className="absolute z-20 border border-yellow-400/60 bg-yellow-400/10 pointer-events-none"
					style={{
						left: Math.min(marqueeRect.x1, marqueeRect.x2),
						top: Math.min(marqueeRect.y1, marqueeRect.y2),
						width: Math.abs(marqueeRect.x2 - marqueeRect.x1),
						height: Math.abs(marqueeRect.y2 - marqueeRect.y1),
					}}
				/>
			)}

			{/* UI Overlay */}

			{enableEditing && (
				<>
					{/* Transform mode toolbar */}
					<div className="absolute left-4 top-4 z-10 flex flex-col rounded-md border border-border bg-background/80 p-1 backdrop-blur-sm">
						<button
							type="button"
							onClick={() => setTransformMode("translate")}
							className={`size-8 inline-flex items-center justify-center rounded-md transition-colors hover:bg-accent hover:text-accent-foreground ${
								transformMode === "translate"
									? "bg-primary text-primary-foreground"
									: "text-muted-foreground"
							}`}
							title="Translate (W)"
						>
							<Move className="h-4 w-4" />
						</button>

						<button
							type="button"
							onClick={() => setTransformMode("rotate")}
							className={`size-8 inline-flex items-center justify-center rounded-md transition-colors hover:bg-accent hover:text-accent-foreground ${
								transformMode === "rotate"
									? "bg-primary text-primary-foreground"
									: "text-muted-foreground"
							}`}
							title="Rotate (E)"
						>
							<RotateCw className="h-4 w-4" />
						</button>
					</div>

					{/* Pivot mode toolbar — visible when 2+ fixtures selected */}
					{selectionSize > 1 && (
						<div className="absolute left-4 top-[5.5rem] z-10 flex flex-col rounded-md border border-border bg-background/80 p-1 backdrop-blur-sm">
							<button
								type="button"
								onClick={() => setTransformPivot("individual")}
								className={`size-8 inline-flex items-center justify-center rounded-md transition-colors hover:bg-accent hover:text-accent-foreground ${
									transformPivot === "individual"
										? "bg-primary text-primary-foreground"
										: "text-muted-foreground"
								}`}
								title="Rotate each in place (Q)"
							>
								<LocateFixed className="h-4 w-4" />
							</button>

							<button
								type="button"
								onClick={() => setTransformPivot("group")}
								className={`size-8 inline-flex items-center justify-center rounded-md transition-colors hover:bg-accent hover:text-accent-foreground ${
									transformPivot === "group"
										? "bg-primary text-primary-foreground"
										: "text-muted-foreground"
								}`}
								title="Rotate around selection center (Q)"
							>
								<Orbit className="h-4 w-4" />
							</button>
						</div>
					)}

					{/* Debug visualization toggles */}
					<div className="absolute left-4 bottom-4 z-10 flex flex-row gap-1 rounded-md border border-border bg-background/80 p-1 backdrop-blur-sm">
						<button
							type="button"
							onClick={() => setShowCircleFit((v) => !v)}
							className={`size-8 inline-flex items-center justify-center rounded-md transition-colors ${
								showCircleFit
									? "bg-green-500/20 text-green-400"
									: "text-muted-foreground hover:bg-accent hover:text-accent-foreground"
							}`}
							title="Toggle circle fit debug"
						>
							<Circle className="h-4 w-4" />
						</button>

						<button
							type="button"
							onClick={() => setShowGroupBounds((v) => !v)}
							className={`size-8 inline-flex items-center justify-center rounded-md transition-colors ${
								showGroupBounds
									? "bg-blue-500/20 text-blue-400"
									: "text-muted-foreground hover:bg-accent hover:text-accent-foreground"
							}`}
							title="Toggle group bounding boxes"
						>
							<Box className="h-4 w-4" />
						</button>

						<button
							type="button"
							onClick={() => setShowMirror((v) => !v)}
							className={`size-8 inline-flex items-center justify-center rounded-md transition-colors ${
								showMirror
									? "bg-orange-500/20 text-orange-400"
									: "text-muted-foreground hover:bg-accent hover:text-accent-foreground"
							}`}
							title="Toggle mirror debug"
						>
							<FlipHorizontal2 className="h-4 w-4" />
						</button>
					</div>
				</>
			)}

			<Canvas
				shadows
				camera={{ position: [0, 1, 3], fov: 50 }}
				dpr={[1, renderSettings.maxDpr ?? 2]}
				onPointerMissed={(e) => {
					// Only deselect if we clicked the background (type 'click') and shift isn't held
					if (
						e.type === "click" &&
						!e.shiftKey &&
						!marqueeJustFinished.current
					) {
						clearSelection();
					}
					marqueeJustFinished.current = false;
				}}
			>
				<color attach="background" args={[darkStage ? "#000000" : "#1a1a1a"]} />

				{/* Lighting */}
				<ambientLight intensity={darkStage ? 0 : 0.2} />
				{!darkStage && (
					<directionalLight
						position={[8, 12, 6]}
						intensity={1.4}
						castShadow
						shadow-mapSize-width={1024}
						shadow-mapSize-height={1024}
					/>
				)}

				{/* Floor — dark surface receives light/shadows; grid overlays in editor */}
				<DarkFloor />
				{!darkStage && <FadingGrid />}

				{/* Spotlight pool — fixed number of Three.js lights */}
				<SpotlightPoolManager />

				{/* Fixtures */}
				<Suspense fallback={null}>
					<FixtureGroup
						enableEditing={enableEditing}
						transformMode={transformMode}
						transformPivot={transformPivot}
						showBounds={showGroupBounds}
						hideBeams
					/>
				</Suspense>

				{/* Finalize spotlight assignments after all fixtures submit */}
				<SpotlightPoolEndFrame />

				{/* Movement extent pyramids for selected mover group */}
				{enableEditing && <MovementPyramids />}

				{/* Circle fit debug visualization */}
				{showCircleFit && <CircleFitDebug />}

				{/* Mirror debug visualization */}
				{showMirror && <MirrorDebug />}

				{/* Controls */}
				<OrbitControls
					ref={controlsRef}
					makeDefault
					zoomSpeed={0.5}
					enableDamping={false}
					enabled={!marqueeActive}
				/>
				<CameraController controlsRef={controlsRef} />
				<FovSync />
				<CameraExposer cameraRef={cameraRef} sizeRef={canvasSizeRef} />
				{exportHandleRef && (
					<ExportHandleExposer
						handleRef={exportHandleRef}
						composerRef={composerRef}
					/>
				)}

				{/* Post-processing */}
				<EffectComposer
					ref={composerRef}
					multisampling={0}
					stencilBuffer={false}
					frameBufferType={HalfFloatType}
				>
					<VolumetricHaze
						fixtures={patchedFixtures}
						hazeDensity={
							renderSettings.volumetricHaze
								? darkStage
									? renderSettings.hazeDensity
									: 0
								: 0
						}
						steps={renderSettings.hazeSteps}
					/>
					<HazeDenoise
						blurRadius={renderSettings.volumetricHaze && darkStage ? 2 : 0}
						depthThreshold={0.02}
					/>
					<Bloom
						luminanceThreshold={0.4}
						luminanceSmoothing={0.9}
						intensity={renderSettings.bloom ? 0.6 : 0}
						mipmapBlur
					/>
				</EffectComposer>

				{/* Runtime metrics */}
				<RenderMetricsProbe metricsRef={renderMetricsRef} />
				<RenderTimeSync getTime={() => renderTimeRef.current} />
			</Canvas>

			<StageFpsOverlay renderMetricsRef={renderMetricsRef} />
		</section>
	);
}
