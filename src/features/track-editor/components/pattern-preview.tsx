import { Canvas, useFrame, useThree } from "@react-three/fiber";
import { Bloom, EffectComposer } from "@react-three/postprocessing";
import { invoke } from "@tauri-apps/api/core";
import { Loader2 } from "lucide-react";
import {
	createContext,
	Suspense,
	useCallback,
	useContext,
	useEffect,
	useRef,
} from "react";
import { HalfFloatType } from "three";
import type { PrimitiveState, UniverseState } from "@/bindings/universe";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { FixtureGroup } from "@/features/visualizer/components/fixture-group";
import { HazeDenoise } from "@/features/visualizer/effects/haze-denoise";
import { VolumetricHaze } from "@/features/visualizer/effects/volumetric-haze";
import { PrimitiveOverrideContext } from "@/features/visualizer/hooks/use-primitive-state";
import { useCameraStore } from "@/features/visualizer/stores/use-camera-store";

// ---------------------------------------------------------------------------
// Shared preview state — allows one persistent Canvas to be driven by
// whichever PatternPreview is currently active.
// ---------------------------------------------------------------------------

type PreviewState = {
	frames: UniverseState[];
	durationSec: number;
} | null;

const PreviewDataContext = createContext<React.RefObject<PreviewState> | null>(
	null,
);

/** Reads the shared preview data ref and cycles through frames each tick. */
function PreviewDriver({
	getPrimitiveRef,
}: {
	getPrimitiveRef: React.MutableRefObject<
		(id: string) => PrimitiveState | undefined
	>;
}) {
	const dataRef = useContext(PreviewDataContext);
	const currentFrameRef = useRef<UniverseState | null>(null);

	useFrame(() => {
		const data = dataRef?.current;
		if (!data || data.frames.length === 0) {
			currentFrameRef.current = null;
			return;
		}

		const clock = (performance.now() / 1000) % data.durationSec;
		const idx =
			Math.floor((clock / data.durationSec) * data.frames.length) %
			data.frames.length;
		currentFrameRef.current = data.frames[idx];
	});

	getPrimitiveRef.current = (id: string) =>
		currentFrameRef.current?.primitives[id];

	return null;
}

/** Syncs the R3F camera to the main visualizer camera whenever it changes. */
function CameraSync() {
	const { camera } = useThree();
	const position = useCameraStore((s) => s.position);
	const target = useCameraStore((s) => s.target);

	useEffect(() => {
		camera.position.set(...position);
		camera.lookAt(...target);
	}, [camera, position, target]);

	return null;
}

// ---------------------------------------------------------------------------
// PreviewCanvas — mount this ONCE in the PatternRegistry. It owns the single
// WebGL context and reads frame data from `previewDataRef`.
// ---------------------------------------------------------------------------

export function PreviewCanvas({
	previewDataRef,
}: {
	previewDataRef: React.RefObject<PreviewState>;
}) {
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);
	const cameraPosition = useCameraStore((s) => s.position);

	const getPrimitiveRef = useRef<(id: string) => PrimitiveState | undefined>(
		() => undefined,
	);
	const getterFn = useCallback(() => getPrimitiveRef.current, []);

	if (patchedFixtures.length === 0) return null;

	return (
		<PreviewDataContext.Provider value={previewDataRef}>
			<PrimitiveOverrideContext.Provider value={getterFn}>
				<Canvas
					camera={{ position: cameraPosition, fov: 50 }}
					dpr={2}
					frameloop="always"
					gl={{ antialias: false, powerPreference: "low-power" }}
				>
					<color attach="background" args={["#000000"]} />

					<mesh rotation={[-Math.PI / 2, 0, 0]}>
						<planeGeometry args={[200, 200]} />
						<meshStandardMaterial color="#030303" roughness={0.95} />
					</mesh>

					<Suspense fallback={null}>
						<FixtureGroup
							enableEditing={false}
							transformMode="translate"
							transformPivot="individual"
							hideBeams
						/>
					</Suspense>

					<EffectComposer
						multisampling={0}
						stencilBuffer={false}
						frameBufferType={HalfFloatType}
					>
						<VolumetricHaze
							fixtures={patchedFixtures}
							hazeDensity={1}
							steps={4}
						/>
						<HazeDenoise blurRadius={2} depthThreshold={0.02} />
						<Bloom
							luminanceThreshold={0.4}
							luminanceSmoothing={0.9}
							intensity={0.6}
							mipmapBlur
						/>
					</EffectComposer>

					<PreviewDriver getPrimitiveRef={getPrimitiveRef} />
					<CameraSync />
				</Canvas>
			</PrimitiveOverrideContext.Provider>
		</PreviewDataContext.Provider>
	);
}

// ---------------------------------------------------------------------------
// PatternPreview — lightweight component that fetches frames and writes them
// into the shared ref. The actual rendering is done by PreviewCanvas.
// ---------------------------------------------------------------------------

const PREVIEW_FPS = 20;

/** Fetch preview frames for a pattern. Can be called before the popover opens. */
export function fetchPreviewFrames(
	patternId: string,
	trackId: string,
	venueId: string,
	beatGrid: import("@/bindings/schema").BeatGrid | null,
	playheadPosition: number,
): { promise: Promise<PreviewState>; cancel: () => void } {
	let cancelled = false;

	// Compute 1-bar time range
	let startTime = playheadPosition;
	let endTime = playheadPosition + 2;

	if (beatGrid && beatGrid.downbeats.length >= 2) {
		const dbs = beatGrid.downbeats;
		startTime = dbs[0];
		endTime = dbs[1];

		for (let i = 0; i < dbs.length - 1; i++) {
			if (dbs[i] <= playheadPosition && playheadPosition < dbs[i + 1]) {
				startTime = dbs[i];
				endTime = dbs[i + 1];
				break;
			}
		}

		if (playheadPosition >= dbs[dbs.length - 1]) {
			const lastBarLen =
				dbs.length >= 2 ? dbs[dbs.length - 1] - dbs[dbs.length - 2] : 2;
			startTime = dbs[dbs.length - 1];
			endTime = startTime + lastBarLen;
		}
	}

	const durationSec = endTime - startTime;

	const promise = invoke<UniverseState[]>("preview_pattern", {
		patternId,
		trackId,
		venueId,
		startTime,
		endTime,
		beatGrid,
		fps: PREVIEW_FPS,
	}).then((frames): PreviewState => {
		if (cancelled) return null;
		return { frames, durationSec };
	});

	return {
		promise,
		cancel: () => {
			cancelled = true;
		},
	};
}

/** Loading overlay shown while preview frames are being fetched. */
export function PatternPreviewOverlay() {
	return (
		<div className="absolute inset-0 z-10 bg-black/80 flex items-center justify-center">
			<Loader2 className="w-4 h-4 text-muted-foreground animate-spin" />
		</div>
	);
}
