import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import type { PlaybackStateSnapshot } from "@/bindings/schema";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { PatternRegistry } from "./pattern-registry";
import { Timeline } from "./timeline";

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

	useEffect(() => {
		// Load patterns first, then track data
		loadPatterns().then(() => {
			loadTrack(trackId, trackName);
		});
	}, [trackId, trackName, loadPatterns, loadTrack]);

	// Playback sync
	useEffect(() => {
		let unsub: (() => void) | null = null;
		let cancelled = false;

		listen<PlaybackStateSnapshot>("pattern-playback://state", (event) => {
			syncPlaybackState(event.payload);
		}).then((unlisten) => {
			if (cancelled) unlisten();
			else unsub = unlisten;
		});

		invoke<PlaybackStateSnapshot>("playback_snapshot").then((snapshot) => {
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

				{/* Center - Main Visualizer (placeholder) */}
				<div className="flex-1 flex flex-col min-w-0">
					<div className="flex-1 min-h-0 flex items-center justify-center bg-base-300/20">
						<div className="text-center text-muted-foreground">
							<div className="text-4xl mb-2 opacity-30">▶</div>
							<div className="text-sm font-medium">{trackName}</div>
							<div className="text-xs opacity-50 mt-1">
								Visualizer coming soon
							</div>
						</div>
					</div>
				</div>

				{/* Right Panel - Inspector (placeholder) */}
				<div className="w-64 border-l border-border flex flex-col bg-background/50">
					<div className="p-3 border-b border-border/50">
						<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
							Inspector
						</h2>
					</div>
					<div className="flex-1 overflow-y-auto p-4">
						<div className="text-xs text-muted-foreground text-center py-8 opacity-50">
							Select an annotation to inspect
						</div>
					</div>
				</div>
			</div>

			{/* Bottom - Timeline (includes minimap) */}
			<div className="border-t border-border" style={{ height: 500 }}>
				<Timeline />
			</div>
		</div>
	);
}
