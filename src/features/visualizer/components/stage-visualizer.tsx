import { OrbitControls } from "@react-three/drei";
import { Canvas, useFrame, useThree } from "@react-three/fiber";
import { Bloom, EffectComposer } from "@react-three/postprocessing";
import { Box, Circle, Move, RotateCw } from "lucide-react";
import { Suspense, useEffect, useMemo, useRef, useState } from "react";
import {
	DoubleSide,
	HalfFloatType,
	PlaneGeometry,
	ShaderMaterial,
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
}

type TransformMode = "translate" | "rotate";
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

export function StageVisualizer({
	enableEditing = false,
	renderAudioTimeSec = null,
}: StageVisualizerProps) {
	const setSelectedPatchedId = useFixtureStore(
		(state) => state.setSelectedPatchedId,
	);
	const [transformMode, setTransformMode] =
		useState<TransformMode>("translate");
	const [showCircleFit, setShowCircleFit] = useState(false);
	const [showGroupBounds, setShowGroupBounds] = useState(false);
	const [isHovered, setIsHovered] = useState(false);
	const renderMetricsRef = useRef<RenderMetrics>({ fps: 0, deltaMs: 0 });
	const renderTimeRef = useRef<number | null>(renderAudioTimeSec ?? null);
	const controlsRef = useRef<OrbitControlsImpl | null>(null);

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
		};

		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [enableEditing, isHovered]);

	return (
		<section
			className="absolute inset-0 bg-background"
			onMouseEnter={() => setIsHovered(true)}
			onMouseLeave={() => setIsHovered(false)}
			aria-label="3D Stage Visualizer"
		>
			{/* UI Overlay */}

			{enableEditing && (
				<div className="absolute left-4 top-4 z-10 flex flex-col gap-2">
					<div className="flex flex-col rounded border border-border bg-background/80 p-1 backdrop-blur-sm">
						<button
							type="button"
							onClick={() => setTransformMode("translate")}
							className={`rounded p-2 transition-colors hover:bg-accent hover:text-accent-foreground ${
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
							className={`rounded p-2 transition-colors hover:bg-accent hover:text-accent-foreground ${
								transformMode === "rotate"
									? "bg-primary text-primary-foreground"
									: "text-muted-foreground"
							}`}
							title="Rotate (E)"
						>
							<RotateCw className="h-4 w-4" />
						</button>
					</div>

					{/* Circle fit debug toggle */}
					<button
						type="button"
						onClick={() => setShowCircleFit((v) => !v)}
						className={`rounded border p-2 transition-colors ${
							showCircleFit
								? "border-green-500 bg-green-500/20 text-green-400"
								: "border-border bg-background/80 text-muted-foreground hover:bg-accent"
						} backdrop-blur-sm`}
						title="Toggle circle fit debug"
					>
						<Circle className="h-4 w-4" />
					</button>

					{/* Group bounds toggle */}
					<button
						type="button"
						onClick={() => setShowGroupBounds((v) => !v)}
						className={`rounded border p-2 transition-colors ${
							showGroupBounds
								? "border-blue-500 bg-blue-500/20 text-blue-400"
								: "border-border bg-background/80 text-muted-foreground hover:bg-accent"
						} backdrop-blur-sm`}
						title="Toggle group bounding boxes"
					>
						<Box className="h-4 w-4" />
					</button>
				</div>
			)}

			<Canvas
				shadows
				camera={{ position: [0, 1, 3], fov: 50 }}
				onPointerMissed={(e) => {
					// Only deselect if we clicked the background (type 'click')
					if (e.type === "click") {
						setSelectedPatchedId(null);
					}
				}}
			>
				<color attach="background" args={["#1a1a1a"]} />

				{/* Basic Lighting */}
				<ambientLight intensity={0.2} />
				<directionalLight
					position={[8, 12, 6]}
					intensity={1.4}
					castShadow
					shadow-mapSize-width={1024}
					shadow-mapSize-height={1024}
				/>

				{/* Floor Grid */}
				<FadingGrid />

				{/* Fixtures */}
				<Suspense fallback={null}>
					<FixtureGroup
						enableEditing={enableEditing}
						transformMode={transformMode}
						showBounds={showGroupBounds}
					/>
				</Suspense>

				{/* Circle fit debug visualization */}
				{showCircleFit && <CircleFitDebug />}

				{/* Controls */}
				<OrbitControls
					ref={controlsRef}
					makeDefault
					zoomSpeed={0.5}
					enableDamping={false}
				/>
				<CameraController controlsRef={controlsRef} />

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
