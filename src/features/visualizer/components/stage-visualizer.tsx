import { Grid, OrbitControls } from "@react-three/drei";
import { Canvas } from "@react-three/fiber";
import { Move, RotateCw } from "lucide-react"; // Import Lucide icons
import { useEffect, useState } from "react";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { dmxStore } from "../stores/dmx-store";
import { FixtureGroup } from "./fixture-group";

interface StageVisualizerProps {
	/**
	 * Whether to allow selecting and transforming fixtures.
	 * Enable this in the Universe editor.
	 */
	enableEditing?: boolean;
}

type TransformMode = "translate" | "rotate";

export function StageVisualizer({
	enableEditing = false,
}: StageVisualizerProps) {
	const setSelectedPatchedId = useFixtureStore(
		(state) => state.setSelectedPatchedId,
	);
	const [transformMode, setTransformMode] =
		useState<TransformMode>("translate");
	const [isHovered, setIsHovered] = useState(false);

	// Initialize DMX Listener
	useEffect(() => {
		const unlistenPromise = dmxStore.init();
		return () => {
			unlistenPromise.then((unlisten) => unlisten());
		};
	}, []);

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
				<FixtureGroup
					enableEditing={enableEditing}
					transformMode={transformMode}
				/>

				{/* Controls */}
				<OrbitControls makeDefault zoomSpeed={0.5} />
			</Canvas>
		</section>
	);
}
