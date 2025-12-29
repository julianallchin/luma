import { Grid, OrbitControls } from "@react-three/drei";
import { Canvas, useFrame } from "@react-three/fiber";
import { Move, RotateCw } from "lucide-react"; // Import Lucide icons
import { Suspense, useEffect, useRef, useState } from "react";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { universeStore } from "../stores/universe-state-store";
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
	const [isHovered, setIsHovered] = useState(false);
	const renderMetricsRef = useRef<RenderMetrics>({ fps: 0, deltaMs: 0 });
	const renderTimeRef = useRef<number | null>(renderAudioTimeSec ?? null);

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
			className="relative h-full w-full bg-background"
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
				<Grid
					infiniteGrid
					fadeDistance={50}
					fadeStrength={4}
					cellColor="#81a1c1"
					sectionColor="#4d707a"
					sectionSize={3}
					cellSize={0.5}
				/>

				{/* Floor to catch light */}
				<mesh
					rotation={[-Math.PI / 2, 0, 0]}
					position={[0, -0.02, 0]}
					receiveShadow
				>
					<planeGeometry args={[50, 50]} />
					<meshStandardMaterial color="#333" roughness={0.8} metalness={0.1} />
				</mesh>

				{/* Fixtures */}
				<Suspense fallback={null}>
					<FixtureGroup
						enableEditing={enableEditing}
						transformMode={transformMode}
					/>
				</Suspense>

				{/* Controls */}
				<OrbitControls makeDefault zoomSpeed={0.5} enableDamping={false} />

				{/* Runtime metrics */}
				<RenderMetricsProbe metricsRef={renderMetricsRef} />
				<RenderTimeSync getTime={() => renderTimeRef.current} />
			</Canvas>

			<StageFpsOverlay renderMetricsRef={renderMetricsRef} />
		</section>
	);
}
