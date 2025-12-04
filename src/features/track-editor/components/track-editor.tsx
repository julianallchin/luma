import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type { HostAudioSnapshot } from "@/bindings/schema";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { PatternRegistry } from "./pattern-registry";
import { Timeline } from "./timeline";
import { InspectorPanel } from "./inspector-panel";

type TrackEditorProps = {
	trackId: number;
	trackName: string;
};

function DragGhost() {
	const draggingPatternId = useTrackEditorStore((s) => s.draggingPatternId);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const [pos, setPos] = useState({ x: 0, y: 0 });

	useEffect(() => {
		if (!draggingPatternId) return;

		const handleMove = (e: MouseEvent) => {
			setPos({ x: e.clientX, y: e.clientY });
		};
		window.addEventListener("mousemove", handleMove);
		return () => window.removeEventListener("mousemove", handleMove);
	}, [draggingPatternId]);

	if (!draggingPatternId) return null;

	const pattern = patterns.find((p) => p.id === draggingPatternId);
	if (!pattern) return null;

	const color = patternColors[pattern.id % patternColors.length];

	return (
		<div
			className="fixed pointer-events-none z-50 px-2 py-1.5 rounded shadow-lg border border-white/10 flex items-center gap-2 bg-neutral-900/90"
			style={{
				left: pos.x,
				top: pos.y,
				transform: "translate(10px, 10px)",
			}}
		>
			<div className="w-3 h-3 rounded-sm" style={{ backgroundColor: color }} />
			<span className="text-xs font-medium text-white">{pattern.name}</span>
		</div>
	);
}

const patternColors = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

export function TrackEditor({ trackId, trackName }: TrackEditorProps) {
	const loadTrack = useTrackEditorStore((s) => s.loadTrack);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);
	const error = useTrackEditorStore((s) => s.error);
	const setError = useTrackEditorStore((s) => s.setError);
	const syncPlaybackState = useTrackEditorStore((s) => s.syncPlaybackState);
	const isPlaying = useTrackEditorStore((s) => s.isPlaying);
	const play = useTrackEditorStore((s) => s.play);
	const pause = useTrackEditorStore((s) => s.pause);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);

	// Debounce compositing to avoid rebuilding on every drag/resize
	const compositeTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const lastCompositedRef = useRef<string>("");

	// Composite track patterns (debounced)
	const compositeTrack = useCallback(
		(immediate = false) => {
			// Clear any pending timeout
			if (compositeTimeoutRef.current) {
				clearTimeout(compositeTimeoutRef.current);
				compositeTimeoutRef.current = null;
			}

			const doComposite = async () => {
				try {
					await invoke("composite_track", { trackId });
				} catch (err) {
					console.error("Failed to composite track:", err);
				}
			};

			if (immediate) {
				doComposite();
			} else {
				// Debounce by 300ms
				compositeTimeoutRef.current = setTimeout(doComposite, 300);
			}
		},
		[trackId],
	);

	// Cleanup timeout on unmount
	useEffect(() => {
		return () => {
			if (compositeTimeoutRef.current) {
				clearTimeout(compositeTimeoutRef.current);
			}
		};
	}, []);

	// Initialize fixtures for the visualizer
	useEffect(() => {
		useFixtureStore.getState().initialize();
	}, []);

	useEffect(() => {
		// Load patterns first, then track data
		loadPatterns().then(() => {
			loadTrack(trackId, trackName);
		});
	}, [trackId, trackName, loadPatterns, loadTrack]);

	// Composite when annotations change (debounced)
	useEffect(() => {
		// Create a signature of current annotations
		const signature = annotations
			.map((a) => `${a.id}:${a.patternId}:${a.startTime}:${a.endTime}:${a.zIndex}`)
			.join("|");

		// Only re-composite if annotations actually changed
		if (signature !== lastCompositedRef.current) {
			const isInitialLoad = lastCompositedRef.current === "";
			lastCompositedRef.current = signature;
			// Immediate on initial load, debounced on subsequent changes
			compositeTrack(isInitialLoad);
		}
	}, [annotations, compositeTrack]);

	// Playback sync
	useEffect(() => {
		let unsub: (() => void) | null = null;
		let cancelled = false;

		listen<HostAudioSnapshot>("host-audio://state", (event) => {
			syncPlaybackState(event.payload);
		}).then((unlisten) => {
			if (cancelled) unlisten();
			else unsub = unlisten;
		});

		invoke<HostAudioSnapshot>("host_snapshot").then((snapshot) => {
			if (!cancelled) syncPlaybackState(snapshot);
		});

		return () => {
			cancelled = true;
			if (unsub) unsub();
		};
	}, [syncPlaybackState]);

	// Global keyboard shortcuts
	useEffect(() => {
		const handleKeyDown = (e: KeyboardEvent) => {
			// Play/Pause
			if (e.code === "Space") {
				// Prevent scrolling only if we're not in a text input
				const target = e.target as HTMLElement;
				const isInput =
					target.tagName === "INPUT" ||
					target.tagName === "TEXTAREA" ||
					target.isContentEditable;

				if (!isInput) {
					e.preventDefault();
					if (isPlaying) {
						pause();
					} else {
						play();
					}
				}
			}
		};

		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [isPlaying, play, pause]);

	const handleDismissError = useCallback(() => {
		setError(null);
	}, [setError]);

	return (
		<div className="flex flex-col h-full bg-background overflow-hidden">
			{/* Drag Ghost */}
			<DragGhost />

			{/* Error banner */}
			{error && (
				<div className="flex items-center justify-between px-4 py-2 bg-destructive/10 border-b border-destructive/20">
					<span className="text-xs text-destructive">{error}</span>
					<button
						type="button"
						onClick={handleDismissError}
						className="text-xs text-destructive hover:text-destructive/80"
					>
						Dismiss
					</button>
				</div>
			)}

			{/* Main content area */}
			<div className="flex flex-1 min-h-0">
				{/* Left Panel - Pattern Registry */}
				<div className="w-64 border-r border-border flex flex-col bg-background/50">
					<div className="p-3 border-b border-border/50">
						<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
							Pattern Registry
						</h2>
					</div>
					<div className="flex-1 overflow-y-auto">
						<PatternRegistry />
					</div>
				</div>

				{/* Center - Main Visualizer */}
				<div className="flex-1 flex flex-col min-w-0">
					<div className="flex-1 min-h-0">
						<StageVisualizer
							enableEditing={false}
							renderAudioTimeSec={playheadPosition}
						/>
					</div>
				</div>

				{/* Right Panel - Inspector */}
				<InspectorPanel />
			</div>

			{/* Bottom - Timeline (includes minimap) */}
			<div className="border-t border-border" style={{ height: 500 }}>
				<Timeline />
			</div>
		</div>
	);
}