import { OrbitControls } from "@react-three/drei";
import { Canvas, useFrame, useThree } from "@react-three/fiber";

import {
	Bloom,
	EffectComposer,
	ToneMapping,
} from "@react-three/postprocessing";
import {
	Box,
	Circle,
	FlipHorizontal2,
	Grid3x3,
	LocateFixed,
	Move,
	Orbit,
	RotateCw,
	ZoomIn,
	ZoomOut,
} from "lucide-react";
import { KernelSize, ToneMappingMode } from "postprocessing";
import type { ReactElement } from "react";
import {
	Suspense,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import type { Camera, PerspectiveCamera, Scene, WebGLRenderer } from "three";
import {
	DoubleSide,
	HalfFloatType,
	PCFSoftShadowMap,
	PlaneGeometry,
	ShaderMaterial,
	Vector3,
} from "three";
import type { OrbitControls as OrbitControlsImpl } from "three-stdlib";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { PropsOverlay } from "@/features/stage/components/props-overlay";
import { StagePiecesLayer } from "@/features/stage/components/stage-pieces-layer";
import { installPointerDragTracker } from "@/features/stage/lib/orbit-state";
import { getPieceGroup } from "@/features/stage/lib/piece-refs";
import { useStagePieceStore } from "@/features/stage/stores/use-stage-piece-store";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { VolumetricHaze } from "../effects/volumetric-haze";
import {
	appendRenderTelemetry,
	getThreeTelemetrySnapshot,
	startRenderTelemetry,
} from "../lib/render-telemetry";
import {
	disposeSpotlightPool,
	initSpotlightPool,
	MAX_POOL,
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
import { UnifiedTransform } from "./unified-transform";

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
	 * Per-frame getter for the render audio time. Prefer this over
	 * `renderAudioTimeSec` for live playback: a changing number prop re-renders
	 * this entire (large) tree on every audio tick, while a stable getter is
	 * read inside the render loop with zero re-renders. Wins when both are set.
	 */
	getRenderAudioTime?: (() => number | null) | null;
	/**
	 * Force dark stage off (lit environment). Used in the Universe editor.
	 */
	forceLightStage?: boolean;
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

  float minor = gridLine(coord, uCellSize, 0.25);
  float major = gridLine(coord, uSectionSize, 0.25);

  vec3 color = mix(uCellColor, uSectionColor, major);
  float alpha = max(minor * 0.01, major * 0.04) * fade * uOpacity;

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
				uCellColor: { value: [1.0, 1.0, 1.0] },
				uSectionColor: { value: [1.0, 1.0, 1.0] },
				uFadeDistance: { value: 50.0 },
				uFadeStrength: { value: 2.0 },
				uOpacity: { value: 0.4 },
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

function ToolbarButton({
	active = false,
	onClick,
	title,
	children,
}: {
	active?: boolean;
	onClick: () => void;
	title: string;
	children: React.ReactNode;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			title={title}
			aria-pressed={active}
			className={cn(
				"w-9 h-7 inline-flex items-center justify-center bg-gutter border-r border-trim last:border-r-0 transition-colors outline-none",
				"text-foreground/70 hover:bg-hover hover:text-foreground",
				active &&
					"bg-hover text-foreground ring-1 ring-inset ring-foreground/40",
			)}
		>
			{children}
		</button>
	);
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

	// Save camera position on OrbitControls change.
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

	// Window-level pointer drag tracker so the stage feature can suppress
	// click selection at the end of an orbit (OrbitControls captures the
	// pointer and R3F's own drag detection doesn't see the move).
	useEffect(() => installPointerDragTracker(), []);

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
		<div className="absolute top-0 right-0 z-10 p-4 flex flex-col items-end">
			<button
				type="button"
				onClick={() => setOpen((v) => !v)}
				className="p-0 leading-none text-[10px] text-neutral-200 font-mono hover:underline focus:outline-none"
				title="Universe/render frame rates"
			>
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
	const enabled = useRenderSettingsStore((s) => s.fixtureSpotlights);

	useEffect(() => {
		initSpotlightPool(scene);
		return () => disposeSpotlightPool(scene);
	}, [scene]);

	useEffect(() => {
		setPoolConfig(enabled ? MAX_POOL : 0);
	}, [enabled]);

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
	getRenderAudioTime = null,
	forceLightStage = false,
}: StageVisualizerProps) {
	const darkStageSetting = useRenderSettingsStore((s) => s.darkStage);
	const darkStage = forceLightStage ? false : darkStageSetting;
	const clearSelection = useFixtureStore((state) => state.clearSelection);
	const selectFixturesByIds = useFixtureStore(
		(state) => state.selectFixturesByIds,
	);
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);
	const renderSettings = useRenderSettingsStore();
	const volumetricHazeEnabled = renderSettings.volumetricHaze && darkStage;
	const postProcessingEnabled = volumetricHazeEnabled || renderSettings.bloom;
	const postProcessingEffects = [
		volumetricHazeEnabled ? (
			<VolumetricHaze
				// Render targets are built once per mount — remount on scale change.
				key={`volumetric-haze-${renderSettings.hazeResolution}`}
				fixtures={patchedFixtures}
				hazeDensity={renderSettings.hazeDensity}
				steps={renderSettings.hazeSteps}
				denoise={renderSettings.hazeDenoise}
				resolutionScale={renderSettings.hazeResolution}
			/>
		) : null,
		renderSettings.bloom ? (
			<Bloom
				key="bloom"
				mipmapBlur={false}
				kernelSize={KernelSize.SMALL}
				radius={0.4}
				luminanceThreshold={0.9}
				luminanceSmoothing={0.2}
				intensity={0.7}
			/>
		) : null,
		// AgX as the final effect: maps the over-range HDR core into a
		// white-hot highlight that desaturates gracefully instead of clipping
		// saturated beam colors to pure white. Must run last.
		<ToneMapping key="tonemap" mode={ToneMappingMode.AGX} />,
	].filter(Boolean) as ReactElement[];
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
	const [telemetryReady, setTelemetryReady] = useState(false);
	const [showGrid, setShowGrid] = useState(true);
	const renderMetricsRef = useRef<RenderMetrics>({ fps: 0, deltaMs: 0 });
	const renderTimeRef = useRef<number | null>(renderAudioTimeSec ?? null);
	const controlsRef = useRef<OrbitControlsImpl | null>(null);
	const glRef = useRef<WebGLRenderer | null>(null);
	const sceneRef = useRef<Scene | null>(null);

	const armedMeshPath = useStagePieceStore((s) => s.armedMeshPath);
	const commitStagePlace = useStagePieceStore((s) => s.commitPlace);
	const cancelStagePlace = useStagePieceStore((s) => s.cancelPlace);
	const clearStageSelection = useStagePieceStore((s) => s.clearSelection);
	const stageSelectionSize = useStagePieceStore((s) => s.selectedIds.size);
	const stagePieces = useStagePieceStore((s) => s.pieces);
	const selectPiecesByIds = useStagePieceStore((s) => s.selectPiecesByIds);
	const removeSelectedPieces = useStagePieceStore(
		(s) => s.removeSelectedPieces,
	);

	// Load stage pieces for the venue being visualized. Every visualizer mounts
	// pieces (not just the editor), so the init lives here rather than
	// the Universe Designer's StageBuilderPanel. The fixture store's venue wins:
	// outside /venue/* routes (pattern editor) the global currentVenue is
	// cleared, and the fixture store tracks the selected instance's venue.
	const appVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const fixtureVenueId = useFixtureStore((s) => s.venueId);
	const currentVenueId = fixtureVenueId ?? appVenueId;
	const initializeStage = useStagePieceStore((s) => s.initialize);
	useEffect(() => {
		if (currentVenueId) initializeStage(currentVenueId);
	}, [currentVenueId, initializeStage]);
	const removeSelectedFixtures = useFixtureStore(
		(s) => s.removeSelectedFixtures,
	);

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

	// Initialize Universe State Listener
	useEffect(() => {
		const unlistenPromise = universeStore.init();
		return () => {
			unlistenPromise.then((unlisten) => unlisten());
		};
	}, []);

	// Dolly the camera toward/away from the orbit target. factor<1 zooms in,
	// factor>1 zooms out. Matches OrbitControls' internal behavior so the
	// change handler still persists the new position to the store.
	const dollyBy = useCallback((factor: number) => {
		const controls = controlsRef.current;
		if (!controls) return;
		const offset = new Vector3().subVectors(
			controls.object.position,
			controls.target,
		);
		offset.multiplyScalar(factor);
		controls.object.position.copy(controls.target).add(offset);
		controls.update();
	}, []);

	useEffect(() => {
		renderTimeRef.current = renderAudioTimeSec ?? null;
	}, [renderAudioTimeSec]);

	useEffect(() => {
		if (!enableEditing) return;

		const handleKeyDown = (e: KeyboardEvent) => {
			// Escape: cancel armed placement OR clear all selection.
			if (e.key === "Escape") {
				if (armedMeshPath) {
					cancelStagePlace();
				} else {
					clearSelection();
					clearStageSelection();
				}
				return;
			}

			// Delete / Backspace: remove every selected fixture AND stage
			// piece. Runs regardless of canvas hover so the keystroke
			// works from anywhere on the Universe page. We skip when an
			// input is focused so it doesn't eat in-place rename keys.
			const target = e.target as HTMLElement | null;
			const isEditing =
				target &&
				(["INPUT", "TEXTAREA"].includes(target.tagName) ||
					target.isContentEditable);
			if (!isEditing && (e.key === "Delete" || e.key === "Backspace")) {
				e.preventDefault();
				if (stageSelectionSize > 0) removeSelectedPieces();
				if (selectionSize > 0) removeSelectedFixtures();
				return;
			}

			if (!isHovered) return; // Only handle the rest if mouse is over the canvas

			// Unity-style hotkeys
			if (e.key.toLowerCase() === "w") setTransformMode("translate");
			if (e.key.toLowerCase() === "e") setTransformMode("rotate");
			if (e.key.toLowerCase() === "q")
				setTransformPivot((p) => (p === "individual" ? "group" : "individual"));
		};

		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [
		enableEditing,
		isHovered,
		armedMeshPath,
		cancelStagePlace,
		clearSelection,
		clearStageSelection,
		stageSelectionSize,
		selectionSize,
		removeSelectedPieces,
		removeSelectedFixtures,
	]);

	const getTelemetrySnapshot = useCallback(
		() => ({
			...getThreeTelemetrySnapshot(glRef.current, sceneRef.current),
			renderMetrics: renderMetricsRef.current,
			universe: universeStore.getSignalMetrics(),
			visualizer: {
				fixtureCount: patchedFixtures.length,
				darkStage,
				settings: {
					bloom: renderSettings.bloom,
					fixtureSpotlights: renderSettings.fixtureSpotlights,
					hazeDensity: renderSettings.hazeDensity,
					hazeSteps: renderSettings.hazeSteps,
					maxDpr: renderSettings.maxDpr,
					postProcessingMounted: postProcessingEnabled,
					volumetricHaze: renderSettings.volumetricHaze,
					volumetricHazeMounted: volumetricHazeEnabled,
				},
			},
		}),
		[
			darkStage,
			patchedFixtures.length,
			postProcessingEnabled,
			renderSettings,
			volumetricHazeEnabled,
		],
	);

	useEffect(() => {
		if (!telemetryReady || !glRef.current) return;

		const canvas = glRef.current.domElement;
		const handleContextLost = (event: Event) => {
			event.preventDefault();
			appendRenderTelemetry("webgl-context-lost", getTelemetrySnapshot());
		};
		const handleContextRestored = () => {
			appendRenderTelemetry("webgl-context-restored", getTelemetrySnapshot());
		};

		canvas.addEventListener("webglcontextlost", handleContextLost);
		canvas.addEventListener("webglcontextrestored", handleContextRestored);
		const stopTelemetry = startRenderTelemetry({
			getSnapshot: getTelemetrySnapshot,
		});

		return () => {
			stopTelemetry();
			canvas.removeEventListener("webglcontextlost", handleContextLost);
			canvas.removeEventListener("webglcontextrestored", handleContextRestored);
		};
	}, [getTelemetrySnapshot, telemetryReady]);

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
				const fixtureHits: string[] = [];
				const pieceHits: string[] = [];
				const vec = new Vector3();

				const insideRect = (px: number, py: number) =>
					px >= left && px <= right && py >= top && py <= bottom;

				for (const f of patchedFixtures) {
					// Z-up (data) to Y-up (Three.js): swap Y↔Z
					vec.set(f.posX, f.posZ, f.posY);
					vec.project(camera);
					const px = (vec.x * 0.5 + 0.5) * size.width;
					const py = (-vec.y * 0.5 + 0.5) * size.height;
					if (insideRect(px, py)) fixtureHits.push(f.id);
				}

				// Stage pieces: project their *live* world position from the
				// scene-graph (parent-local pose collapses through ancestors).
				// We can't use raw posX/Y/Z because attached pieces store them
				// in parent-local space.
				for (const p of stagePieces) {
					const g = getPieceGroup(p.id);
					if (!g) continue;
					g.updateMatrixWorld(true);
					g.getWorldPosition(vec);
					vec.project(camera);
					const px = (vec.x * 0.5 + 0.5) * size.width;
					const py = (-vec.y * 0.5 + 0.5) * size.height;
					if (insideRect(px, py)) pieceHits.push(p.id);
				}

				if (fixtureHits.length > 0) selectFixturesByIds(fixtureHits);
				if (pieceHits.length > 0) selectPiecesByIds(pieceHits);
				marqueeJustFinished.current = true;
			}
		}

		setMarqueeActive(false);
		setMarqueeRect(null);
	}, [
		marqueeActive,
		marqueeRect,
		patchedFixtures,
		stagePieces,
		selectFixturesByIds,
		selectPiecesByIds,
	]);

	return (
		<section
			ref={sectionRef}
			className={cn(
				"absolute inset-0 bg-gutter",
				armedMeshPath && "cursor-crosshair",
			)}
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

			{/* Corner framing ticks */}
			<div className="pointer-events-none absolute inset-2 z-10">
				<div className="absolute left-0 top-0 h-3 w-3 border-l border-t border-foreground/40" />
				<div className="absolute right-0 top-0 h-3 w-3 border-r border-t border-foreground/40" />
				<div className="absolute left-0 bottom-0 h-3 w-3 border-l border-b border-foreground/40" />
				<div className="absolute right-0 bottom-0 h-3 w-3 border-r border-b border-foreground/40" />
			</div>

			{/* UI Overlay */}

			{enableEditing && (
				<>
					{/* Top-left: Props library overlay */}
					<PropsOverlay />

					{/* Bottom-centered toolbar: transform + zoom */}
					<div className="absolute inset-x-0 bottom-4 z-10 flex justify-center pointer-events-none">
						<div className="flex bg-gutter border border-trim select-none pointer-events-auto">
							<ToolbarButton
								active={transformMode === "translate"}
								onClick={() => setTransformMode("translate")}
								title="Translate (W)"
							>
								<Move className="h-3.5 w-3.5" />
							</ToolbarButton>
							<ToolbarButton
								active={transformMode === "rotate"}
								onClick={() => setTransformMode("rotate")}
								title="Rotate (E)"
							>
								<RotateCw className="h-3.5 w-3.5" />
							</ToolbarButton>

							<div className="w-px bg-trim" aria-hidden />

							<ToolbarButton
								active={showGrid}
								onClick={() => setShowGrid((v) => !v)}
								title={showGrid ? "Hide grid" : "Show grid"}
							>
								<Grid3x3 className="h-3.5 w-3.5" />
							</ToolbarButton>
							<ToolbarButton onClick={() => dollyBy(0.8)} title="Zoom in">
								<ZoomIn className="h-3.5 w-3.5" />
							</ToolbarButton>
							<ToolbarButton onClick={() => dollyBy(1.25)} title="Zoom out">
								<ZoomOut className="h-3.5 w-3.5" />
							</ToolbarButton>

							<div className="w-px bg-trim" aria-hidden />

							<ToolbarButton
								active={showCircleFit}
								onClick={() => setShowCircleFit((v) => !v)}
								title="Toggle circle fit debug"
							>
								<Circle className="h-3.5 w-3.5" />
							</ToolbarButton>
							<ToolbarButton
								active={showGroupBounds}
								onClick={() => setShowGroupBounds((v) => !v)}
								title="Toggle group bounding boxes"
							>
								<Box className="h-3.5 w-3.5" />
							</ToolbarButton>
							<ToolbarButton
								active={showMirror}
								onClick={() => setShowMirror((v) => !v)}
								title="Toggle mirror debug"
							>
								<FlipHorizontal2 className="h-3.5 w-3.5" />
							</ToolbarButton>
						</div>
					</div>

					{/* Pivot mode toolbar — visible when 2+ fixtures or stage pieces selected */}
					{(selectionSize > 1 || stageSelectionSize > 1) && (
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
				</>
			)}

			<Canvas
				shadows={{ type: PCFSoftShadowMap }}
				camera={{ position: [0, 1, 3], fov: 50 }}
				dpr={[1, renderSettings.maxDpr ?? 2]}
				// R3F's ResizeObserver currently runs through react-use-measure's
				// scroll callback. Override both defaults so flex-panel drags
				// resize the renderer in the same frame as the DOM container.
				resize={{ debounce: { resize: 0, scroll: 0 } }}
				onCreated={({ gl, scene }) => {
					glRef.current = gl;
					sceneRef.current = scene;
					setTelemetryReady(true);
					appendRenderTelemetry("webgl-created", {
						...getThreeTelemetrySnapshot(gl, scene),
						visualizer: {
							fixtureCount: patchedFixtures.length,
						},
					});
				}}
				onPointerMissed={(e) => {
					if (e.type !== "click" || marqueeJustFinished.current) {
						marqueeJustFinished.current = false;
						return;
					}
					// Armed placement consumes background clicks
					if (armedMeshPath) {
						commitStagePlace();
						return;
					}
					if (!e.shiftKey) {
						clearSelection();
						clearStageSelection();
					}
				}}
			>
				<color attach="background" args={[darkStage ? "#000000" : "#191919"]} />

				{/* Lighting */}
				<ambientLight intensity={darkStage ? 0 : 0.2} />
				{!darkStage && (
					<directionalLight
						position={[8, 12, 6]}
						intensity={1.4}
						castShadow
						shadow-mapSize-width={4096}
						shadow-mapSize-height={4096}
						shadow-camera-left={-15}
						shadow-camera-right={15}
						shadow-camera-top={15}
						shadow-camera-bottom={-15}
						shadow-camera-near={0.5}
						shadow-camera-far={60}
						shadow-normalBias={0.01}
					/>
				)}

				{/* Floor — dark surface receives light/shadows; grid overlays in editor */}
				<DarkFloor />
				{!darkStage && showGrid && <FadingGrid />}

				{/* Spotlight pool — fixed number of Three.js lights */}
				<SpotlightPoolManager />

				{/* Fixtures */}
				<Suspense fallback={null}>
					<FixtureGroup
						enableEditing={enableEditing}
						showBounds={showGroupBounds}
						hideBeams
					/>
				</Suspense>

				{/* Stage pieces (set-design: floors, trusses, speakers, ...).
				    Rendered in every visualizer so props appear in track / pattern
				    / perform views, not just the Universe editor. The `enableEditing`
				    flag still gates click-select / hover / gizmo wiring inside. */}
				<StagePiecesLayer
					enableEditing={enableEditing}
					transformMode={transformMode}
				/>

				{/* Unified gizmo — one widget operating on the union of
				    selected fixtures + stage cluster roots. */}
				{enableEditing && (
					<UnifiedTransform
						enableEditing={enableEditing}
						transformMode={transformMode}
						transformPivot={transformPivot}
					/>
				)}

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

				{postProcessingEnabled ? (
					<EffectComposer
						multisampling={4}
						stencilBuffer={false}
						frameBufferType={HalfFloatType}
					>
						{postProcessingEffects}
					</EffectComposer>
				) : null}

				{/* Runtime metrics */}
				<RenderMetricsProbe metricsRef={renderMetricsRef} />
				<RenderTimeSync
					getTime={getRenderAudioTime ?? (() => renderTimeRef.current)}
				/>
			</Canvas>

			<StageFpsOverlay renderMetricsRef={renderMetricsRef} />
		</section>
	);
}
