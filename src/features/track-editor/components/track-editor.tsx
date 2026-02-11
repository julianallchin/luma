import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2 } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import type { HostAudioSnapshot } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { TrackBrowser } from "@/features/tracks/components/track-browser";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import { cn } from "@/shared/lib/utils";
import { useAnnotationPreviewStore } from "../stores/use-annotation-preview-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { InspectorPanel } from "./inspector-panel";
import { Timeline } from "./timeline";
import { TrackSidebar } from "./track-sidebar";

type TrackEditorProps = {
	trackId?: number | null;
	trackName?: string;
};

function DragGhost() {
	const draggingPatternId = useTrackEditorStore((s) => s.draggingPatternId);
	const dragOrigin = useTrackEditorStore((s) => s.dragOrigin);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const ref = useRef<HTMLDivElement>(null);

	useEffect(() => {
		if (!draggingPatternId || !ref.current) return;

		const el = ref.current;
		const handleMove = (e: MouseEvent) => {
			el.style.left = `${e.clientX}px`;
			el.style.top = `${e.clientY}px`;
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
			ref={ref}
			className="fixed pointer-events-none z-50 px-2 py-1.5 rounded shadow-lg border border-white/10 flex items-center gap-2 bg-neutral-900/90"
			style={{
				left: dragOrigin.x,
				top: dragOrigin.y,
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

function Timecode() {
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);

	// Calculate beat and bar from playhead position using downbeats array
	const getTimecode = () => {
		if (!beatGrid?.downbeats?.length || !beatGrid?.beats?.length) {
			return { bar: "0.0", beat: 0 };
		}

		// Find which bar we're in by finding the last downbeat <= playheadPosition
		let barIndex = 0;
		for (let i = 0; i < beatGrid.downbeats.length; i++) {
			if (beatGrid.downbeats[i] <= playheadPosition) {
				barIndex = i;
			} else {
				break;
			}
		}

		const barStart = beatGrid.downbeats[barIndex];
		const barNumber = barIndex + 1;

		// Find which beat within this bar
		let beatInBar = 1;
		for (const beat of beatGrid.beats) {
			if (beat > barStart && beat <= playheadPosition) {
				beatInBar++;
			}
		}

		// Clamp beat to beatsPerBar
		beatInBar = Math.min(beatInBar, beatGrid.beatsPerBar);

		// Total beat count (for the BEAT display)
		let totalBeat = 0;
		for (const beat of beatGrid.beats) {
			if (beat <= playheadPosition) {
				totalBeat++;
			}
		}

		return {
			bar: `${barNumber}.${beatInBar}`,
			beat: totalBeat,
		};
	};

	const { bar, beat } = getTimecode();
	const seconds = playheadPosition.toFixed(2);

	return (
		<div className="flex items-center gap-3 text-xs font-mono">
			<div className="flex items-center gap-1">
				<span className="text-muted-foreground">BAR</span>
				<span className="text-foreground w-10 text-right">{bar}</span>
			</div>
			<div className="flex items-center gap-1">
				<span className="text-muted-foreground">BEAT</span>
				<span className="text-foreground w-8 text-right">{beat}</span>
			</div>
			<div className="flex items-center gap-1">
				<span className="text-muted-foreground">SEC</span>
				<span className="text-foreground w-12 text-right">{seconds}</span>
			</div>
		</div>
	);
}

export function TrackEditor({ trackId, trackName }: TrackEditorProps) {
	const loadTrack = useTrackEditorStore((s) => s.loadTrack);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);
	const loadTrackPlayback = useTrackEditorStore((s) => s.loadTrackPlayback);
	const resetTrack = useTrackEditorStore((s) => s.resetTrack);
	const activeTrackId = useTrackEditorStore((s) => s.trackId);
	const error = useTrackEditorStore((s) => s.error);
	const setError = useTrackEditorStore((s) => s.setError);
	const syncPlaybackState = useTrackEditorStore((s) => s.syncPlaybackState);
	const isPlaying = useTrackEditorStore((s) => s.isPlaying);
	const play = useTrackEditorStore((s) => s.play);
	const pause = useTrackEditorStore((s) => s.pause);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const annotationsLoading = useTrackEditorStore((s) => s.annotationsLoading);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
	const playbackRate = useTrackEditorStore((s) => s.playbackRate);
	const setPlaybackRate = useTrackEditorStore((s) => s.setPlaybackRate);
	const isCompositing = useTrackEditorStore((s) => s.isCompositing);
	const setIsCompositing = useTrackEditorStore((s) => s.setIsCompositing);
	const isDraggingAnnotation = useTrackEditorStore(
		(s) => s.isDraggingAnnotation,
	);
	const panelHeight = useTrackEditorStore((s) => s.panelHeight);
	const setPanelHeight = useTrackEditorStore((s) => s.setPanelHeight);
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);

	const resolvedTrackId = trackId ?? null;
	const resolvedTrackName =
		trackName ?? (resolvedTrackId !== null ? `Track ${resolvedTrackId}` : "");

	// Debounce compositing to avoid rebuilding on every drag/resize
	const compositeTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(
		null,
	);
	const lastCompositedRef = useRef<string>("");
	const lastCompositeContextRef = useRef<string>("");
	const isResizingRef = useRef(false);
	const timelinePanelRef = useRef<HTMLDivElement>(null);
	const timelineInnerRef = useRef<HTMLDivElement>(null);

	// Composite track patterns (debounced)
	const compositeTrack = useCallback(
		(immediate = false, skipCache = false) => {
			if (activeTrackId === null) return;
			// Clear any pending timeout
			if (compositeTimeoutRef.current) {
				clearTimeout(compositeTimeoutRef.current);
				compositeTimeoutRef.current = null;
			}

			const doComposite = async () => {
				setIsCompositing(true);
				try {
					if (currentVenueId === null) {
						console.warn("No venue selected, skipping composite");
						return;
					}
					await invoke("composite_track", {
						trackId: activeTrackId,
						venueId: currentVenueId,
						skipCache,
					});
				} catch (err) {
					console.error("Failed to composite track:", err);
				} finally {
					setIsCompositing(false);
				}
			};

			if (immediate) {
				doComposite();
			} else {
				// Debounce by 300ms
				compositeTimeoutRef.current = setTimeout(doComposite, 300);
			}
		},
		[activeTrackId, currentVenueId, setIsCompositing],
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
		if (currentVenueId !== null) {
			useFixtureStore.getState().initialize(currentVenueId);
		} else {
			useFixtureStore.getState().initialize();
		}
	}, [currentVenueId]);

	useEffect(() => {
		// Load patterns first, then track data
		if (resolvedTrackId === null) {
			if (activeTrackId === null) {
				resetTrack();
			}
			loadPatterns();
			if (activeTrackId !== null) {
				loadTrackPlayback(activeTrackId);
			}
			return;
		}
		loadPatterns().then(() => {
			loadTrack(resolvedTrackId, resolvedTrackName);
		});
	}, [
		resolvedTrackId,
		resolvedTrackName,
		loadPatterns,
		loadTrack,
		loadTrackPlayback,
		resetTrack,
		activeTrackId,
	]);

	// Composite when annotations or venue/track context change (debounced)
	useEffect(() => {
		if (activeTrackId === null || currentVenueId === null) {
			lastCompositedRef.current = "";
			lastCompositeContextRef.current = "";
			return;
		}
		if (annotationsLoading) return;
		// Skip compositing during drag/resize - wait until committed
		if (isDraggingAnnotation) return;

		// Create a signature of current annotations
		const signature = annotations
			.map(
				(a) =>
					`${a.id}:${a.patternId}:${a.startTime}:${a.endTime}:${a.zIndex}:${a.blendMode}:${JSON.stringify(a.args)}`,
			)
			.join("|");
		const compositeContext = `${activeTrackId}:${currentVenueId}`;
		const compositeKey = `${compositeContext}|${signature}`;

		// Only re-composite if annotations actually changed
		if (compositeKey !== lastCompositedRef.current) {
			const isInitialLoad = lastCompositedRef.current === "";
			const isContextChange =
				compositeContext !== lastCompositeContextRef.current;
			lastCompositedRef.current = compositeKey;
			lastCompositeContextRef.current = compositeContext;
			// Immediate on initial load with cache skip, debounced on subsequent changes
			const forceComposite = isInitialLoad || isContextChange;
			compositeTrack(forceComposite, forceComposite);
		}
	}, [
		activeTrackId,
		annotations,
		annotationsLoading,
		compositeTrack,
		currentVenueId,
		isDraggingAnnotation,
	]);

	// Load annotation previews when annotations or context change
	useEffect(() => {
		if (activeTrackId === null || currentVenueId === null) return;
		if (annotationsLoading || isDraggingAnnotation) return;
		if (annotations.length === 0) {
			useAnnotationPreviewStore.getState().clear();
			return;
		}
		useAnnotationPreviewStore
			.getState()
			.loadPreviews(activeTrackId, currentVenueId);
	}, [
		activeTrackId,
		annotations,
		annotationsLoading,
		currentVenueId,
		isDraggingAnnotation,
	]);

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
				if (activeTrackId === null) return;
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
	}, [activeTrackId, isPlaying, play, pause]);

	const handleResizeStart = useCallback(
		(e: React.MouseEvent) => {
			e.preventDefault();
			const startY = e.clientY;
			const startHeight = panelHeight;
			isResizingRef.current = true;
			const panel = timelinePanelRef.current;
			const inner = timelineInnerRef.current;
			if (panel) panel.style.transition = "none";

			const handleMove = (ev: MouseEvent) => {
				const delta = startY - ev.clientY;
				const clamped = Math.max(200, Math.min(600, startHeight + delta));
				if (panel) panel.style.maxHeight = `${clamped}px`;
				if (inner) inner.style.height = `${clamped - 6}px`;
				window.dispatchEvent(new Event("resize"));
			};

			const handleUp = (ev: MouseEvent) => {
				isResizingRef.current = false;
				const delta = startY - ev.clientY;
				const final = Math.max(200, Math.min(600, startHeight + delta));
				setPanelHeight(final);
				if (panel) panel.style.transition = "";
				window.removeEventListener("mousemove", handleMove);
				window.removeEventListener("mouseup", handleUp);
			};

			window.addEventListener("mousemove", handleMove);
			window.addEventListener("mouseup", handleUp);
		},
		[panelHeight, setPanelHeight],
	);

	const handleDismissError = useCallback(() => {
		setError(null);
	}, [setError]);

	if (activeTrackId === null && resolvedTrackId === null) {
		return <TrackBrowser />;
	}

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
				{/* Left Panel - Tracks / Patterns */}
				<TrackSidebar />

				{/* Center - Main Visualizer */}
				<div className="flex-1 flex flex-col min-w-0">
					<div className="flex-1 min-h-0 relative">
						<StageVisualizer
							enableEditing={false}
							renderAudioTimeSec={playheadPosition}
						/>
						<div className="absolute top-4 left-4 flex items-center gap-3 rounded-md border border-border/60 bg-background/80 px-3 py-1.5 text-xs shadow-sm">
							<Timecode />
							<div className="h-4 w-px bg-border" />
							<div className="flex items-center gap-1">
								<button
									type="button"
									onClick={() => {
										void setPlaybackRate(1);
									}}
									aria-pressed={playbackRate === 1}
									className={cn(
										"px-2 py-1 rounded",
										playbackRate === 1
											? "bg-muted text-foreground"
											: "text-muted-foreground hover:text-foreground",
									)}
								>
									1x
								</button>
								<button
									type="button"
									onClick={() => {
										void setPlaybackRate(0.5);
									}}
									aria-pressed={playbackRate === 0.5}
									className={cn(
										"px-2 py-1 rounded",
										playbackRate === 0.5
											? "bg-muted text-foreground"
											: "text-muted-foreground hover:text-foreground",
									)}
								>
									0.5x
								</button>
							</div>
						</div>
						{isCompositing && (
							<div className="absolute top-4 right-4 flex items-center gap-2 pointer-events-none">
								<Loader2 className="w-4 h-4 animate-spin" />
							</div>
						)}
					</div>
				</div>

				{/* Right Panel - Inspector */}
				<InspectorPanel />
			</div>

			{/* Bottom - Timeline (includes minimap) */}
			<div
				ref={timelinePanelRef}
				className="border-t border-border overflow-hidden transition-[max-height] duration-300 ease-in-out"
				style={{ maxHeight: activeTrackId !== null ? panelHeight : 0 }}
			>
				{/* Drag handle */}
				{/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle is mouse-only */}
				<div
					className="h-1.5 cursor-row-resize flex items-center justify-center hover:bg-muted/40 active:bg-muted/60"
					onMouseDown={handleResizeStart}
				>
					<div className="w-8 h-0.5 rounded-full bg-muted-foreground/30" />
				</div>
				<div ref={timelineInnerRef} style={{ height: panelHeight - 6 }}>
					<Timeline />
				</div>
			</div>
		</div>
	);
}
