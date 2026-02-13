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
import {
	Suspense,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import type { Camera } from "three";
import {
	DoubleSide,
	HalfFloatType,
	PlaneGeometry,
	ShaderMaterial,
	Vector3,
} from "three";
import type { OrbitControls as OrbitControlsImpl } from "three-stdlib";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { universeStore } from "../stores/universe-state-store";
import { useCameraStore } from "../stores/use-camera-store";
import { CircleFitDebug } from "./circle-fit-debug";
import { FixtureGroup } from "./fixture-group";
import { MirrorDebug } from "./mirror-debug";

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
	 * Dark stage mode — kills ambient/directional light, replaces the grid
	 * with a matte black floor, and darkens fixture models so only beams
	 * and emissives are visible. Useful for realistic perform preview.
	 */
	darkStage?: boolean;
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

	return <mesh geometry={mesh.geo} material={mesh.mat} />;
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

	// Restore camera position on mount
	useEffect(() => {
		if (!initialized.current && controlsRef.current) {
			camera.position.set(...position);
			controlsRef.current.target.set(...target);
			controlsRef.current.update();
			initialized.current = true;
		}
	}, [camera, controlsRef, position, target]);

	// Save camera position on OrbitControls change
	useEffect(() => {
		const controls = controlsRef.current;
		if (!controls) return;

		const handleChange = () => {
			const pos = camera.position.toArray() as [number, number, number];
			const tgt = controls.target.toArray() as [number, number, number];
			setCamera(pos, tgt);
		};

		controls.addEventListener("end", handleChange);
		return () => {
			controls.removeEventListener("end", handleChange);
		};
	}, [camera, controlsRef, setCamera]);

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

function StageFpsOverlay({
	renderMetricsRef,
}: {
	renderMetricsRef: React.MutableRefObject<RenderMetrics>;
}) {
	const [metrics, setMetrics] = useState({
		signalFps: 0,
		signalDelta: 0,
		bufferReadFps: 0,
		bufferReadDelta: 0,
		renderFps: 0,
		renderDelta: 0,
	});

	useEffect(() => {
		const id = window.setInterval(() => {
			const signal = universeStore.getSignalMetrics();
			const render = renderMetricsRef.current;

			setMetrics({
				signalFps: signal.fps ?? 0,
				signalDelta: signal.deltaMs ?? 0,
				bufferReadFps: signal.readFps ?? 0,
				bufferReadDelta: signal.readDeltaMs ?? 0,
				renderFps: render.fps ?? 0,
				renderDelta: render.deltaMs ?? 0,
			});
		}, 300);

		return () => clearInterval(id);
	}, [renderMetricsRef]);

	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="absolute bottom-2 right-2 z-10 px-2 py-1 bg-neutral-900/90 rounded text-[10px] text-neutral-200 font-mono backdrop-blur-sm border border-neutral-800 shadow-sm hover:border-neutral-700 transition-colors"
					title="Universe/render frame rates"
				>
					sig {metrics.signalFps.toFixed(0)} / read{" "}
					{metrics.bufferReadFps.toFixed(0)} / render{" "}
					{metrics.renderFps.toFixed(0)} fps
				</button>
			</PopoverTrigger>
			<PopoverContent className="w-64 text-[11px] font-mono bg-neutral-950 border-neutral-800 text-neutral-200">
				<div className="space-y-1">
					<div className="flex justify-between">
						<span>signal fps</span>
						<span>{metrics.signalFps.toFixed(1)}</span>
					</div>
					<div className="flex justify-between text-neutral-400">
						<span>signal delta</span>
						<span>{metrics.signalDelta.toFixed(1)} ms</span>
					</div>
					<div className="flex justify-between">
						<span>buffer read fps</span>
						<span>{metrics.bufferReadFps.toFixed(1)}</span>
					</div>
					<div className="flex justify-between text-neutral-400">
						<span>buffer read delta</span>
						<span>{metrics.bufferReadDelta.toFixed(1)} ms</span>
					</div>
					<div className="h-px bg-neutral-800 my-2" />
					<div className="flex justify-between">
						<span>render fps</span>
						<span>{metrics.renderFps.toFixed(1)}</span>
					</div>
					<div className="flex justify-between text-neutral-400">
						<span>render delta</span>
						<span>{metrics.renderDelta.toFixed(1)} ms</span>
					</div>
					<div className="text-[10px] text-neutral-500 pt-2">
						Universe updates stream from Rust; render is the three.js canvas.
					</div>
				</div>
			</PopoverContent>
		</Popover>
	);
}

function DarkFloor() {
	return (
		<mesh rotation={[-Math.PI / 2, 0, 0]} receiveShadow>
			<planeGeometry args={[200, 200]} />
			<meshStandardMaterial color="#050505" roughness={0.95} />
		</mesh>
	);
}

export function StageVisualizer({
	enableEditing = false,
	renderAudioTimeSec = null,
	darkStage = false,
}: StageVisualizerProps) {
	const clearSelection = useFixtureStore((state) => state.clearSelection);
	const selectFixturesByIds = useFixtureStore(
		(state) => state.selectFixturesByIds,
	);
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);
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

				{/* Basic Lighting */}
				<ambientLight intensity={darkStage ? 0.08 : 0.2} />
				{!darkStage && (
					<directionalLight
						position={[8, 12, 6]}
						intensity={1.4}
						castShadow
						shadow-mapSize-width={1024}
						shadow-mapSize-height={1024}
					/>
				)}

				{/* Floor */}
				{darkStage ? <DarkFloor /> : <FadingGrid />}

				{/* Fixtures */}
				<Suspense fallback={null}>
					<FixtureGroup
						enableEditing={enableEditing}
						transformMode={transformMode}
						transformPivot={transformPivot}
						showBounds={showGroupBounds}
					/>
				</Suspense>

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
				<CameraExposer cameraRef={cameraRef} sizeRef={canvasSizeRef} />

				{/* Post-processing */}
				<EffectComposer
					multisampling={0}
					stencilBuffer={false}
					frameBufferType={HalfFloatType}
				>
					<Bloom
						luminanceThreshold={1}
						luminanceSmoothing={0.3}
						intensity={0.5}
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
