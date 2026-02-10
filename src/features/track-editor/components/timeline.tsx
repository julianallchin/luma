import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useAnnotationPreviewStore } from "../stores/use-annotation-preview-store";
import {
	type SelectionCursor,
	type TimelineAnnotation,
	useTrackEditorStore,
} from "../stores/use-track-editor-store";
import { useUndoStore } from "../stores/use-undo-store";
import type { RenderMetrics } from "../types/timeline-types";
import { getCanvasColor, getCanvasColorRgba } from "../utils/canvas-colors";
import {
	ALWAYS_DRAW,
	computeLayout,
	getPatternColor,
	HEADER_HEIGHT,
	MAX_ZOOM_Y,
	MIN_ZOOM_Y,
	MINIMAP_HEIGHT,
	TRACK_HEIGHT,
	WAVEFORM_HEIGHT,
} from "../utils/timeline-constants";
import {
	ANNOTATION_HEADER_H,
	drawAnnotations,
	drawBeatGrid,
	drawDragPreview,
	drawDragPreviewAtY,
	drawPlayhead,
	drawSelectionCursor,
	drawTimeRuler,
	drawWaveform,
} from "../utils/timeline-drawing";
import { useTimelineZoom } from "./hooks/use-timeline-zoom";
import { TimelineMetrics } from "./timeline-metrics";
import { useMinimapDrawing } from "./timeline-minimap";
import { TimelineShortcuts } from "./timeline-shortcuts";

const now = () =>
	typeof performance !== "undefined" ? performance.now() : Date.now();

// Ableton-style bracket resize cursors
const CURSOR_BRACKET_L = `url('data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><path d="M14 4H10V20H14" fill="none" stroke="white" stroke-width="3" stroke-linejoin="miter"/><path d="M14 4H10V20H14" fill="none" stroke="black" stroke-width="1.5" stroke-linejoin="miter"/></svg>') 12 12, col-resize`;
const CURSOR_BRACKET_R = `url('data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><path d="M10 4H14V20H10" fill="none" stroke="white" stroke-width="3" stroke-linejoin="miter"/><path d="M10 4H14V20H10" fill="none" stroke="black" stroke-width="1.5" stroke-linejoin="miter"/></svg>') 12 12, col-resize`;

export function Timeline() {
	// STORE STATE (Data Source)
	const trackId = useTrackEditorStore((s) => s.trackId);
	const durationSeconds = useTrackEditorStore((s) => s.durationSeconds);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const trackName = useTrackEditorStore((s) => s.trackName);
	const waveform = useTrackEditorStore((s) => s.waveform);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
	const isPlaying = useTrackEditorStore((s) => s.isPlaying);
	const playbackRate = useTrackEditorStore((s) => s.playbackRate);
	const setPlayheadPosition = useTrackEditorStore((s) => s.setPlayheadPosition);
	const createAnnotation = useTrackEditorStore((s) => s.createAnnotation);
	const updateAnnotation = useTrackEditorStore((s) => s.updateAnnotation);
	const updateAnnotationsLocal = useTrackEditorStore(
		(s) => s.updateAnnotationsLocal,
	);
	const persistAnnotations = useTrackEditorStore((s) => s.persistAnnotations);
	const deleteAnnotations = useTrackEditorStore((s) => s.deleteAnnotations);
	const selectionCursor = useTrackEditorStore((s) => s.selectionCursor);
	const setSelectionCursor = useTrackEditorStore((s) => s.setSelectionCursor);
	const selectedAnnotationIds = useTrackEditorStore(
		(s) => s.selectedAnnotationIds,
	);
	const setSelectedAnnotationIds = useTrackEditorStore(
		(s) => s.setSelectedAnnotationIds,
	);
	const selectAnnotation = useTrackEditorStore((s) => s.selectAnnotation);
	const splitAtCursor = useTrackEditorStore((s) => s.splitAtCursor);
	const deleteInRegion = useTrackEditorStore((s) => s.deleteInRegion);
	const moveAnnotationsVertical = useTrackEditorStore(
		(s) => s.moveAnnotationsVertical,
	);
	const copySelection = useTrackEditorStore((s) => s.copySelection);
	const cutSelection = useTrackEditorStore((s) => s.cutSelection);
	const paste = useTrackEditorStore((s) => s.paste);
	const duplicate = useTrackEditorStore((s) => s.duplicate);
	const draggingPatternId = useTrackEditorStore((s) => s.draggingPatternId);
	const setDraggingPatternId = useTrackEditorStore(
		(s) => s.setDraggingPatternId,
	);
	const setIsDraggingAnnotation = useTrackEditorStore(
		(s) => s.setIsDraggingAnnotation,
	);
	const seek = useTrackEditorStore((s) => s.seek);
	const storeZoom = useTrackEditorStore((s) => s.zoom);
	const storeScrollX = useTrackEditorStore((s) => s.scrollX);
	const storeZoomY = useTrackEditorStore((s) => s.zoomY);
	const setZoom = useTrackEditorStore((s) => s.setZoom);
	const setScrollX = useTrackEditorStore((s) => s.setScrollX);
	const setZoomY = useTrackEditorStore((s) => s.setZoomY);

	const previewBitmaps = useAnnotationPreviewStore((s) => s.bitmaps);
	const previewGeneration = useAnnotationPreviewStore((s) => s.generation);

	const getPreviewBitmap = useCallback(
		(id: number) => previewBitmaps.get(id),
		[previewBitmaps],
	);

	const durationMs = durationSeconds * 1000;
	const navigate = useNavigate();
	const location = useLocation();

	// UI STATE (Display only)
	const [metricsDisplay, setMetricsDisplay] = useState<RenderMetrics | null>(
		null,
	);
	const [, forceRender] = useState(0);

	// Updated Insertion State: tracks more detail
	const [insertionData, setInsertionData] = useState<{
		type: "insert" | "add";
		zIndex: number; // Logical Z to target
		y?: number; // Pixel Y for line (if insert)
		row?: number; // Visual Row index (if add)
	} | null>(null);

	// DRAG PREVIEW STATE
	const [dragPreview, setDragPreview] = useState<{
		startTime: number;
		endTime: number;
		color: string;
		name: string;
	} | null>(null);

	// REFS (Source of Truth for Physics)
	const containerRef = useRef<HTMLDivElement>(null);
	const canvasRef = useRef<HTMLCanvasElement>(null);
	const minimapRef = useRef<HTMLCanvasElement>(null);
	const spacerRef = useRef<HTMLDivElement>(null);
	const zoomRef = useRef(storeZoom); // pixels per second, initialized from store
	const zoomYRef = useRef(storeZoomY); // vertical zoom factor
	const annotationsRef = useRef<TimelineAnnotation[]>([]);
	const drawRef = useRef<() => void>(() => {});
	const rafIdRef = useRef<number | null>(null);
	const lastDrawTsRef = useRef<number | null>(null);
	const lastRafTsRef = useRef<number | null>(null);
	const rafFpsRef = useRef(0);
	const rafDeltaRef = useRef(0);
	const blockedAvgRef = useRef(0);
	const blockedPeakRef = useRef(0);
	const needsDrawRef = useRef(true);
	const minimapDirtyRef = useRef(true);
	const lastSyncPlayheadRef = useRef(0);
	const lastSyncTsRef = useRef(now());
	const playheadDragRef = useRef(false);
	const cursorDragRef = useRef<{
		active: boolean;
		startTime: number;
		trackRow: number;
	} | null>(null);
	const selectionCursorRef = useRef<SelectionCursor | null>(null);
	const metricsRef = useRef<RenderMetrics>({
		drawFps: 0,
		rafFps: 0,
		rafDelta: 0,
		blockedAvg: 0,
		blockedPeak: 0,
		totalMs: 0,
		sections: { ruler: 0, waveform: 0, annotations: 0, minimap: 0 },
		frame: 0,
		avg: {
			ruler: 0,
			waveform: 0,
			annotations: 0,
			minimap: 0,
			totalMs: 0,
		},
		peak: {
			ruler: 0,
			waveform: 0,
			annotations: 0,
			minimap: 0,
			totalMs: 0,
		},
	});

	// CALCULATE LAYERS
	const sortedZ = useMemo(() => {
		const z = Array.from(new Set(annotations.map((a) => a.zIndex)));
		return z.sort((a, b) => a - b);
	}, [annotations]);

	const rowMap = useMemo(() => {
		const map = new Map<number, number>();
		const maxRow = Math.max(0, sortedZ.length - 1);
		annotations.forEach((a) => {
			const idx = sortedZ.indexOf(a.zIndex);
			// Invert order: Higher Z = Lower Row Index (Visually Higher)
			// idx 0 (Lowest Z) -> maxRow
			// idx max (Highest Z) -> 0
			const row = idx >= 0 ? maxRow - idx : maxRow;
			map.set(a.id, row);
		});
		return map;
	}, [annotations, sortedZ]);

	// Keep refs in sync
	const rowMapRef = useRef(rowMap);
	const sortedZRef = useRef(sortedZ);
	const insertionDataRef = useRef(insertionData);

	useEffect(() => {
		annotationsRef.current = annotations;
		rowMapRef.current = rowMap;
		sortedZRef.current = sortedZ;
	}, [annotations, rowMap, sortedZ]);

	// Track if we've restored scroll position
	const scrollRestoredRef = useRef(false);

	// Restore scroll position from store once spacer is sized
	useEffect(() => {
		if (scrollRestoredRef.current || durationMs <= 0) return;
		const container = containerRef.current;
		const spacer = spacerRef.current;
		if (container && spacer) {
			// Ensure spacer is sized first
			spacer.style.width = `${(durationMs / 1000) * zoomRef.current}px`;
			// Then restore scroll
			if (storeScrollX > 0) {
				container.scrollLeft = storeScrollX;
			}
			scrollRestoredRef.current = true;
		}
	}, [durationMs, storeScrollX]);

	// Sync zoomRef from store on mount
	useEffect(() => {
		zoomRef.current = storeZoom;
		zoomYRef.current = storeZoomY;
	}, []); // eslint-disable-line react-hooks/exhaustive-deps

	// Save zoom and scroll position to store on unmount
	useEffect(() => {
		return () => {
			setZoom(zoomRef.current);
			setZoomY(zoomYRef.current);
			if (containerRef.current) {
				setScrollX(containerRef.current.scrollLeft);
			}
		};
	}, [setZoom, setZoomY, setScrollX]);

	useEffect(() => {
		insertionDataRef.current = insertionData;
		needsDrawRef.current = true;
	}, [insertionData]);

	useEffect(() => {
		selectionCursorRef.current = selectionCursor;
		needsDrawRef.current = true;
	}, [selectionCursor]);

	useEffect(() => {
		drawRef.current();
	}, []);

	useEffect(() => {
		minimapDirtyRef.current = true;
		needsDrawRef.current = true;
	}, []);

	// Force redraw when relevant data for minimap/timeline changes
	useEffect(() => {
		needsDrawRef.current = true;
		minimapDirtyRef.current = true;
	}, [durationSeconds, waveform, beatGrid, annotations]);

	// Redraw when annotation previews arrive
	useEffect(() => {
		needsDrawRef.current = true;
	}, [previewGeneration]);

	useEffect(() => {
		lastSyncPlayheadRef.current = playheadPosition;
		lastSyncTsRef.current = now();
		needsDrawRef.current = true;
		minimapDirtyRef.current = true;
	}, [playheadPosition]);

	useEffect(() => {
		if (!isPlaying) return;
		lastSyncPlayheadRef.current = playheadPosition;
		lastSyncTsRef.current = now();
	}, [isPlaying, playbackRate, playheadPosition]);

	// DRAG STATE
	const dragRef = useRef({
		active: false,
		type: null as string | null,
		startX: 0,
		startScroll: 0,
		startZoom: 0,
		startLensX: 0,
		startLensW: 0,
		minimapWidth: 0,
		containerWidth: 0,
		annotation: null as TimelineAnnotation | null,
		startTime: 0,
		endTime: 0,
	});

	// Initialize spacer width
	useEffect(() => {
		if (spacerRef.current && durationMs > 0) {
			spacerRef.current.style.width = `${
				(durationMs / 1000) * zoomRef.current
			}px`;
		}
	}, [durationMs]);

	// Helper: Average beat duration (seconds)
	const getAverageBeatDuration = useCallback((): number => {
		if (!beatGrid?.beats.length || beatGrid.beats.length < 2) return 0.5;
		const beats = beatGrid.beats;
		return (beats[beats.length - 1] - beats[0]) / (beats.length - 1);
	}, [beatGrid]);

	// Helper: Calculate 1 bar length from downbeats
	const getOneBarLength = useCallback(
		(_startTime: number): number => {
			if (!beatGrid?.downbeats.length || beatGrid.downbeats.length < 2) {
				return getAverageBeatDuration() * (beatGrid?.beatsPerBar || 4);
			}
			let totalBarLength = 0;
			for (let i = 1; i < beatGrid.downbeats.length; i++) {
				totalBarLength += beatGrid.downbeats[i] - beatGrid.downbeats[i - 1];
			}
			return totalBarLength / (beatGrid.downbeats.length - 1);
		},
		[beatGrid, getAverageBeatDuration],
	);

	const getQuarterBeatSnap = useCallback(
		(time: number): number => {
			if (!beatGrid?.beats.length) return time;
			const beats = beatGrid.beats;
			if (beats.length === 1) return beats[0];

			let index = 0;
			while (index + 1 < beats.length && beats[index + 1] <= time) {
				index += 1;
			}

			const prevBeat = beats[index];
			const nextBeat = beats[index + 1];
			if (nextBeat === undefined) return prevBeat;

			const rawBeatLength = nextBeat - prevBeat;
			const beatLength =
				rawBeatLength > 0 ? rawBeatLength : getAverageBeatDuration();

			if (!Number.isFinite(beatLength) || beatLength <= 0) return prevBeat;

			const offset = (time - prevBeat) / beatLength;
			const quarterIndex = Math.max(0, Math.min(4, Math.round(offset * 4)));
			const snapped = prevBeat + (quarterIndex / 4) * beatLength;
			return Math.min(nextBeat, Math.max(prevBeat, snapped));
		},
		[beatGrid, getAverageBeatDuration],
	);

	const getEighthBeatSnap = useCallback(
		(time: number): number => {
			if (!beatGrid?.beats.length) return time;
			const beats = beatGrid.beats;
			if (beats.length === 1) return beats[0];

			let index = 0;
			while (index + 1 < beats.length && beats[index + 1] <= time) {
				index += 1;
			}

			const prevBeat = beats[index];
			const nextBeat = beats[index + 1];
			if (nextBeat === undefined) return prevBeat;

			const rawBeatLength = nextBeat - prevBeat;
			const beatLength =
				rawBeatLength > 0 ? rawBeatLength : getAverageBeatDuration();

			if (!Number.isFinite(beatLength) || beatLength <= 0) return prevBeat;

			const offset = (time - prevBeat) / beatLength;
			const eighthIndex = Math.max(0, Math.min(2, Math.round(offset * 2)));
			const snapped = prevBeat + (eighthIndex / 2) * beatLength;
			return Math.min(nextBeat, Math.max(prevBeat, snapped));
		},
		[beatGrid, getAverageBeatDuration],
	);

	const getSixteenthBeatSnap = useCallback(
		(time: number): number => {
			if (!beatGrid?.beats.length) return time;
			const beats = beatGrid.beats;
			if (beats.length === 1) return beats[0];

			let index = 0;
			while (index + 1 < beats.length && beats[index + 1] <= time) {
				index += 1;
			}

			const prevBeat = beats[index];
			const nextBeat = beats[index + 1];
			if (nextBeat === undefined) return prevBeat;

			const rawBeatLength = nextBeat - prevBeat;
			const beatLength =
				rawBeatLength > 0 ? rawBeatLength : getAverageBeatDuration();

			if (!Number.isFinite(beatLength) || beatLength <= 0) return prevBeat;

			const offset = (time - prevBeat) / beatLength;
			const sixteenthIndex = Math.max(0, Math.min(4, Math.round(offset * 4)));
			const snapped = prevBeat + (sixteenthIndex / 4) * beatLength;
			return Math.min(nextBeat, Math.max(prevBeat, snapped));
		},
		[beatGrid, getAverageBeatDuration],
	);

	const getTripletBeatSnap = useCallback(
		(time: number): number => {
			if (!beatGrid?.beats.length) return time;
			const beats = beatGrid.beats;
			if (beats.length === 1) return beats[0];

			let index = 0;
			while (index + 1 < beats.length && beats[index + 1] <= time) {
				index += 1;
			}

			const prevBeat = beats[index];
			const nextBeat = beats[index + 1];
			if (nextBeat === undefined) return prevBeat;

			const rawBeatLength = nextBeat - prevBeat;
			const beatLength =
				rawBeatLength > 0 ? rawBeatLength : getAverageBeatDuration();

			if (!Number.isFinite(beatLength) || beatLength <= 0) return prevBeat;

			const offset = (time - prevBeat) / beatLength;
			const tripletIndex = Math.max(0, Math.min(3, Math.round(offset * 3)));
			const snapped = prevBeat + (tripletIndex / 3) * beatLength;
			return Math.min(nextBeat, Math.max(prevBeat, snapped));
		},
		[beatGrid, getAverageBeatDuration],
	);

	// Minimap drawing hook
	const drawMinimap = useMinimapDrawing({
		minimapRef,
		durationMs,
		waveform,
		playheadPosition,
		zoomRef,
		containerRef,
	});

	// Main draw function
	const draw = useCallback(() => {
		const frameStart = now();
		const sections = { ruler: 0, waveform: 0, annotations: 0, minimap: 0 };
		let playheadForRender = playheadPosition;
		if (isPlaying) {
			const deltaSeconds = (frameStart - lastSyncTsRef.current) / 1000;
			playheadForRender = Math.max(
				0,
				Math.min(
					durationSeconds,
					lastSyncPlayheadRef.current + deltaSeconds * playbackRate,
				),
			);
		}
		const canvas = canvasRef.current;
		const container = containerRef.current;
		if (!canvas || !container || durationMs <= 0) return;

		const ctx = canvas.getContext("2d", { alpha: false });
		if (!ctx) return;

		const dpr = window.devicePixelRatio || 1;
		const width = container.clientWidth;
		const height = container.clientHeight;
		const scrollTop = container.scrollTop;

		if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
			canvas.width = width * dpr;
			canvas.height = height * dpr;
			ctx.scale(dpr, dpr);
			canvas.style.width = `${width}px`;
			canvas.style.height = `${height}px`;
		}

		ctx.fillStyle = getCanvasColor("--background");
		ctx.fillRect(0, 0, width, height);

		const currentZoom = zoomRef.current;
		const layout = computeLayout(zoomYRef.current);
		const scrollLeft = container.scrollLeft;
		const startTime = scrollLeft / currentZoom;
		const endTime = (scrollLeft + width) / currentZoom;

		// Draw Time Ruler Background
		ctx.fillStyle = getCanvasColorRgba("--background", 0.4);
		ctx.fillRect(0, 0, width, layout.headerHeight);

		ctx.font = '10px "SF Mono", "Geist Mono", monospace';

		// Draw Beat Grid & Ruler
		const rulerStart = now();
		if (beatGrid) {
			drawBeatGrid(
				ctx,
				beatGrid,
				startTime,
				endTime,
				currentZoom,
				scrollLeft,
				height,
				layout,
			);
		} else {
			drawTimeRuler(ctx, startTime, endTime, currentZoom, scrollLeft, layout);
		}

		ctx.strokeStyle = getCanvasColor("--border");
		ctx.beginPath();
		ctx.moveTo(0, layout.headerHeight);
		ctx.lineTo(width, layout.headerHeight);
		ctx.stroke();

		sections.ruler = now() - rulerStart;

		// Draw Waveform
		const waveformStart = now();
		drawWaveform(
			ctx,
			waveform,
			startTime,
			endTime,
			durationSeconds,
			currentZoom,
			scrollLeft,
			width,
			layout,
		);
		sections.waveform = now() - waveformStart;

		const annotationsStart = now();
		// Use ref for insertionData to avoid closure staleness in draw loop
		const currentInsertionData = insertionDataRef.current;

		// TRACK RENDERING (SCROLLABLE)
		ctx.save();
		// Clip to track area so we don't draw over header
		ctx.beginPath();
		ctx.rect(0, layout.trackAreaY, width, height - layout.trackAreaY);
		ctx.clip();

		// Translate for scrolling
		ctx.translate(0, -scrollTop);

		drawAnnotations(
			ctx,
			annotationsRef.current,
			startTime,
			endTime,
			currentZoom,
			scrollLeft,
			width,
			selectedAnnotationIds,
			rowMapRef.current,
			currentInsertionData,
			layout,
			getPreviewBitmap,
		);

		// Draw Drag Preview
		if (dragPreview && currentInsertionData) {
			// For "add" mode use the explicit row; for boundary adds derive from y
			if (currentInsertionData.row !== undefined) {
				drawDragPreview(
					ctx,
					dragPreview,
					currentZoom,
					scrollLeft,
					currentInsertionData.row,
					layout,
				);
			} else if (
				currentInsertionData.type === "add" &&
				currentInsertionData.y !== undefined
			) {
				// Boundary add: draw ghost at the insertion line position
				drawDragPreviewAtY(
					ctx,
					dragPreview,
					currentZoom,
					scrollLeft,
					currentInsertionData.y,
					layout,
				);
			}
		}

		// Draw Selection Cursor
		const currentSelectionCursor = selectionCursorRef.current;
		if (currentSelectionCursor) {
			drawSelectionCursor(
				ctx,
				currentSelectionCursor,
				startTime,
				endTime,
				currentZoom,
				scrollLeft,
				layout,
			);
		}

		ctx.restore();

		sections.annotations = now() - annotationsStart;

		// Draw Playhead (Screen Space, over everything)
		drawPlayhead(
			ctx,
			playheadForRender,
			startTime,
			endTime,
			currentZoom,
			scrollLeft,
			height,
			layout,
		);

		// Draw Minimap
		const minimapStart = now();
		if (minimapDirtyRef.current || ALWAYS_DRAW || isPlaying) {
			drawMinimap(playheadForRender);
			minimapDirtyRef.current = false;
			sections.minimap = now() - minimapStart;
		} else {
			sections.minimap = 0;
		}

		const totalMs = now() - frameStart;
		const fpsFromFrame =
			totalMs > 0 ? 1000 / totalMs : metricsRef.current.drawFps;
		const smoothedDrawFps =
			metricsRef.current.drawFps > 0
				? metricsRef.current.drawFps * 0.85 + fpsFromFrame * 0.15
				: fpsFromFrame;
		const nextFrame = metricsRef.current.frame + 1;

		const lerp = (prev: number, curr: number) =>
			prev === 0 ? curr : prev * 0.9 + curr * 0.1;
		const avgRuler = lerp(metricsRef.current.avg.ruler, sections.ruler);
		const avgWaveform = lerp(
			metricsRef.current.avg.waveform,
			sections.waveform,
		);
		const avgAnnotations = lerp(
			metricsRef.current.avg.annotations,
			sections.annotations,
		);
		const avgMinimap = lerp(metricsRef.current.avg.minimap, sections.minimap);
		const avgTotal = lerp(metricsRef.current.avg.totalMs, totalMs);

		metricsRef.current = {
			drawFps: smoothedDrawFps,
			rafFps: rafFpsRef.current,
			rafDelta: rafDeltaRef.current,
			blockedAvg: blockedAvgRef.current,
			blockedPeak: blockedPeakRef.current,
			totalMs,
			sections,
			frame: nextFrame,
			avg: {
				ruler: avgRuler,
				waveform: avgWaveform,
				annotations: avgAnnotations,
				minimap: avgMinimap,
				totalMs: avgTotal,
			},
			peak: {
				ruler: Math.max(metricsRef.current.peak.ruler, sections.ruler),
				waveform: Math.max(metricsRef.current.peak.waveform, sections.waveform),
				annotations: Math.max(
					metricsRef.current.peak.annotations,
					sections.annotations,
				),
				minimap: Math.max(metricsRef.current.peak.minimap, sections.minimap),
				totalMs: Math.max(metricsRef.current.peak.totalMs, totalMs),
			},
		};

		lastDrawTsRef.current = frameStart;
		needsDrawRef.current = false;

		if (nextFrame % 5 === 0) {
			setMetricsDisplay(metricsRef.current);
		}
	}, [
		durationMs,
		durationSeconds,
		beatGrid,
		waveform,
		playheadPosition,
		selectedAnnotationIds,
		selectionCursor,
		dragPreview,
		drawMinimap,
		isPlaying,
		playbackRate,
		getPreviewBitmap,
		previewGeneration,
	]);

	// Keep draw ref in sync
	useEffect(() => {
		drawRef.current = draw;
	}, [draw]);

	// MAIN RAF LOOP
	useEffect(() => {
		const tick = (ts: number) => {
			if (lastRafTsRef.current !== null) {
				const delta = ts - lastRafTsRef.current;
				if (delta > 0) {
					const rafFps = 1000 / delta;
					rafFpsRef.current =
						rafFpsRef.current === 0
							? rafFps
							: rafFpsRef.current * 0.9 + rafFps * 0.1;
					rafDeltaRef.current = delta;
					const blocked = Math.max(0, delta - 6.9);
					blockedAvgRef.current =
						blockedAvgRef.current === 0
							? blocked
							: blockedAvgRef.current * 0.9 + blocked * 0.1;
					blockedPeakRef.current = Math.max(blockedPeakRef.current, blocked);
				}
			}
			lastRafTsRef.current = ts;

			if (ALWAYS_DRAW) {
				needsDrawRef.current = true;
			} else if (isPlaying) {
				needsDrawRef.current = true;
			}

			if (needsDrawRef.current) {
				drawRef.current();
			} else if (metricsRef.current.frame % 5 === 0) {
				setMetricsDisplay(metricsRef.current);
			}

			rafIdRef.current = requestAnimationFrame(tick);
		};

		rafIdRef.current = requestAnimationFrame(tick);
		return () => {
			if (rafIdRef.current !== null) {
				cancelAnimationFrame(rafIdRef.current);
			}
		};
	}, [isPlaying]);

	// Zoom hook
	useTimelineZoom(
		containerRef,
		spacerRef,
		zoomRef,
		durationMs,
		draw,
		setZoom,
		zoomYRef,
		(zy: number) => {
			setZoomY(zy);
			needsDrawRef.current = true;
		},
	);

	// MINIMAP INTERACTION
	const handleMinimapDown = useCallback(
		(e: React.MouseEvent) => {
			e.preventDefault();
			const canvas = minimapRef.current;
			const container = containerRef.current;
			if (!canvas || !container) return;
			minimapDirtyRef.current = true;

			const rect = canvas.getBoundingClientRect();
			const x = e.clientX - rect.left;
			const width = rect.width;

			const timeToPixel = width / durationMs;
			const currentZoom = zoomRef.current;
			const scrollLeft = container.scrollLeft;

			const visibleTimeStart = (scrollLeft / currentZoom) * 1000;
			const visibleTimeEnd =
				((scrollLeft + container.clientWidth) / currentZoom) * 1000;
			const lensX = visibleTimeStart * timeToPixel;
			const lensW = (visibleTimeEnd - visibleTimeStart) * timeToPixel;
			const handleSize = 8;

			let type: string | null = null;
			let initialScroll = scrollLeft;
			let initialLensX = lensX;

			if (Math.abs(x - lensX) < handleSize) {
				type = "resize-left";
			} else if (Math.abs(x - (lensX + lensW)) < handleSize) {
				type = "resize-right";
			} else if (x > lensX && x < lensX + lensW) {
				type = "move";
			} else {
				// Click outside lens: Snap view AND start dragging
				type = "move";
				const clickTime = (x / width) * durationMs;
				const targetPixel = (clickTime / 1000) * currentZoom;
				const newScroll = targetPixel - container.clientWidth / 2;

				container.scrollLeft = newScroll;
				drawRef.current();

				// Update initial values for the drag session
				initialScroll = newScroll;
				const newVisibleTimeStart = (newScroll / currentZoom) * 1000;
				initialLensX = newVisibleTimeStart * timeToPixel;
			}

			dragRef.current = {
				...dragRef.current,
				active: true,
				type: type,
				startX: e.clientX,
				startScroll: initialScroll,
				startZoom: currentZoom,
				startLensX: initialLensX,
				startLensW: lensW,
				minimapWidth: width,
				containerWidth: container.clientWidth,
			};

			const handleMove = (ev: MouseEvent) => {
				if (!dragRef.current.active) return;
				const {
					type,
					startX,
					startScroll,
					startZoom,
					startLensX,
					startLensW,
					minimapWidth,
					containerWidth,
				} = dragRef.current;

				const dx = ev.clientX - startX;
				const timeToPixel = minimapWidth / durationMs;

				if (type === "move") {
					const pixelToTime = durationMs / minimapWidth;
					const timeDelta = dx * pixelToTime;
					const initialStartTime = (startScroll / startZoom) * 1000;
					const newStartTime = initialStartTime + timeDelta;
					const newScroll = (newStartTime / 1000) * startZoom;
					if (containerRef.current) {
						containerRef.current.scrollLeft = newScroll;
					}
				} else if (type === "resize-right") {
					const newLensW = Math.max(10, startLensW + dx);
					const newVisibleDuration = newLensW / timeToPixel;
					const newZoom = containerWidth / (newVisibleDuration / 1000);
					const clampedZoom = Math.max(5, Math.min(500, newZoom));
					const initialStartTime = (startScroll / startZoom) * 1000;
					const newScroll = (initialStartTime / 1000) * clampedZoom;

					zoomRef.current = clampedZoom;
					setZoom(clampedZoom);
					if (spacerRef.current) {
						spacerRef.current.style.width = `${
							(durationMs / 1000) * clampedZoom
						}px`;
					}
					if (containerRef.current) {
						containerRef.current.scrollLeft = newScroll;
					}
				} else if (type === "resize-left") {
					const newLensW = Math.max(10, startLensW - dx);
					const newLensX = startLensX + dx;
					const newVisibleDuration = newLensW / timeToPixel;
					const newZoom = containerWidth / (newVisibleDuration / 1000);
					const clampedZoom = Math.max(5, Math.min(500, newZoom));
					const newStartTime = newLensX / timeToPixel;
					const newScroll = (newStartTime / 1000) * clampedZoom;

					zoomRef.current = clampedZoom;
					setZoom(clampedZoom);
					if (spacerRef.current) {
						spacerRef.current.style.width = `${
							(durationMs / 1000) * clampedZoom
						}px`;
					}
					if (containerRef.current) {
						containerRef.current.scrollLeft = newScroll;
					}
				}
				drawRef.current();
			};

			const handleUp = () => {
				dragRef.current.active = false;
				window.removeEventListener("mousemove", handleMove);
				window.removeEventListener("mouseup", handleUp);
			};

			window.addEventListener("mousemove", handleMove);
			window.addEventListener("mouseup", handleUp);
		},
		[durationMs, setZoom],
	);

	const handleMinimapHover = useCallback(
		(e: React.MouseEvent) => {
			if (dragRef.current.active) return;
			const canvas = minimapRef.current;
			const container = containerRef.current;
			if (!canvas || !container) return;
			minimapDirtyRef.current = true;

			const rect = canvas.getBoundingClientRect();
			const x = e.clientX - rect.left;
			const width = rect.width;

			const timeToPixel = width / durationMs;
			const currentZoom = zoomRef.current;
			const scrollLeft = container.scrollLeft;

			const visibleTimeStart = (scrollLeft / currentZoom) * 1000;
			const visibleTimeEnd =
				((scrollLeft + container.clientWidth) / currentZoom) * 1000;
			const lensX = visibleTimeStart * timeToPixel;
			const lensW = (visibleTimeEnd - visibleTimeStart) * timeToPixel;
			const handleSize = 8;

			if (
				Math.abs(x - lensX) < handleSize ||
				Math.abs(x - (lensX + lensW)) < handleSize
			) {
				canvas.style.cursor = "ew-resize";
			} else if (x > lensX && x < lensX + lensW) {
				canvas.style.cursor = "grab";
			} else {
				canvas.style.cursor = "pointer";
			}
		},
		[durationMs],
	);

	// SCROLL HANDLER
	const handleScroll = useCallback(() => {
		minimapDirtyRef.current = true;
		needsDrawRef.current = true;
		// Save scroll position to store
		if (containerRef.current) {
			setScrollX(containerRef.current.scrollLeft);
		}
		requestAnimationFrame(draw);
	}, [draw, setScrollX]);

	const snapToGrid = useCallback(
		(time: number): number => {
			// Progressive snapping based on zoom level:
			// - Zoom < 100: quarter notes (1 division per beat, snaps to beats only)
			// - Zoom >= 100 and < 200: eighth notes (2 divisions per beat)
			// - Zoom >= 200: sixteenth notes (4 divisions per beat) or triplets (3 divisions per beat)
			const zoom = zoomRef.current;
			let snapped: number;
			if (zoom >= 200) {
				// At very high zoom, prefer sixteenth notes, but also allow triplets
				// For now, use sixteenth notes
				snapped = getSixteenthBeatSnap(time);
			} else if (zoom >= 100) {
				snapped = getEighthBeatSnap(time);
			} else {
				snapped = getQuarterBeatSnap(time);
			}
			return Math.abs(snapped - time) * zoom < 15 ? snapped : time;
		},
		[
			getQuarterBeatSnap,
			getEighthBeatSnap,
			getSixteenthBeatSnap,
			getTripletBeatSnap,
		],
	);

	// ANNOTATION CLICK/DRAG
	const handleCanvasMouseDown = useCallback(
		(e: React.MouseEvent) => {
			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const x = e.clientX - rect.left + container.scrollLeft;
			const y = e.clientY - rect.top + container.scrollTop; // World Y
			const currentZoom = zoomRef.current;
			const layout = computeLayout(zoomYRef.current);

			// Playhead dragging in header (Header is fixed at Screen Y=0)
			// But mouse Y is World Y.
			// Screen Y = Y - scrollTop.
			const screenY = y - container.scrollTop;
			if (screenY < layout.headerHeight) {
				const time = Math.max(0, Math.min(durationSeconds, x / currentZoom));
				seek(time);
				setPlayheadPosition(time);
				playheadDragRef.current = true;

				const handleUp = () => {
					playheadDragRef.current = false;
					window.removeEventListener("mouseup", handleUp);
				};

				window.addEventListener("mouseup", handleUp);
				return;
			}

			// Check if clicking in annotation lane
			const totalHeight =
				layout.trackStartY +
				Math.max(1, sortedZRef.current.length) * layout.trackHeight;

			if (y >= layout.trackStartY && y < totalHeight) {
				const clickTime = x / currentZoom;
				const relativeY = y - layout.trackStartY;
				const laneIdx = Math.floor(relativeY / layout.trackHeight);

				// Find annotation whose header bar was clicked in this lane
				const clicked = annotationsRef.current.find((ann) => {
					const annLane = rowMapRef.current.get(ann.id) ?? 0;
					if (annLane !== laneIdx) return false;
					if (clickTime < ann.startTime || clickTime > ann.endTime)
						return false;
					// Only count as clicked if in the header bar area
					const annTopY = layout.trackStartY + annLane * layout.trackHeight + 1;
					return y < annTopY + ANNOTATION_HEADER_H;
				});

				if (clicked) {
					// Check if clicked annotation is already in selection
					const alreadySelected = selectedAnnotationIds.includes(clicked.id);

					if (!alreadySelected) {
						if (e.shiftKey) {
							// Shift-click: add to current selection
							setSelectedAnnotationIds([...selectedAnnotationIds, clicked.id]);
						} else {
							// Plain click: select only this one
							selectAnnotation(clicked.id);
						}
					} else if (e.shiftKey) {
						// Shift-click on already-selected: remove from selection
						setSelectedAnnotationIds(
							selectedAnnotationIds.filter((id) => id !== clicked.id),
						);
					}
					// If already selected without shift, keep the current multi-selection

					// Always update selection cursor to the clicked annotation
					const newCursor = {
						trackRow: laneIdx,
						trackRowEnd: null,
						startTime: clicked.startTime,
						endTime: clicked.endTime,
					};
					setSelectionCursor(newCursor);
					// Update ref immediately so drag handlers use the correct cursor
					selectionCursorRef.current = newCursor;

					forceRender((n) => n + 1);

					const annStartX = clicked.startTime * currentZoom;
					const annEndX = clicked.endTime * currentZoom;
					const handleSize = 8;

					let dragType: "move" | "resize-left" | "resize-right" = "move";
					if (x - annStartX < handleSize) dragType = "resize-left";
					else if (annEndX - x < handleSize) dragType = "resize-right";

					// Capture the current selection cursor for moving
					const cursorAtDragStart = selectionCursorRef.current;

					// Capture all selected annotations' initial positions
					// If clicking an unselected annotation, only drag that one
					// If clicking an already selected annotation, drag all selected
					const selectedAnns = alreadySelected
						? annotationsRef.current.filter((a) =>
								selectedAnnotationIds.includes(a.id),
							)
						: [clicked];
					const initialPositions = new Map(
						selectedAnns.map((a) => [
							a.id,
							{ startTime: a.startTime, endTime: a.endTime },
						]),
					);

					dragRef.current = {
						...dragRef.current,
						active: true,
						type: `annotation-${dragType}`,
						startX: e.clientX,
						annotation: clicked,
						startTime: clicked.startTime,
						endTime: clicked.endTime,
					};

					// Capture pre-drag snapshot for undo
					useTrackEditorStore.getState().captureBeforeDrag();

					// Mark that we're dragging to prevent composite during resize
					setIsDraggingAnnotation(true);

					const handleMove = (ev: MouseEvent) => {
						if (!dragRef.current.active || !dragRef.current.annotation) return;
						const dx = ev.clientX - dragRef.current.startX;
						const deltaTime = dx / zoomRef.current;

						const snapToGrid = (time: number) => {
							// Progressive snapping based on zoom level:
							// - Zoom < 100: quarter notes (1 division per beat, snaps to beats only)
							// - Zoom >= 100 and < 200: eighth notes (2 divisions per beat)
							// - Zoom >= 200: sixteenth notes (4 divisions per beat)
							const zoom = zoomRef.current;
							let snapped: number;
							if (zoom >= 200) {
								snapped = getSixteenthBeatSnap(time);
							} else if (zoom >= 100) {
								snapped = getEighthBeatSnap(time);
							} else {
								snapped = getQuarterBeatSnap(time);
							}
							return Math.abs(snapped - time) * zoom < 12 ? snapped : time;
						};

						if (dragType === "move") {
							// Calculate snapped delta based on the clicked annotation
							const clickedInitial = initialPositions.get(clicked.id);
							if (!clickedInitial) return;
							let newStart = snapToGrid(clickedInitial.startTime + deltaTime);
							newStart = Math.max(0, newStart);
							const snappedDelta = newStart - clickedInitial.startTime;

							// Build batch update for ALL selected annotations
							const updates: {
								id: number;
								startTime: number;
								endTime: number;
							}[] = [];
							for (const [annId, initial] of initialPositions) {
								const newAnnStart = Math.max(
									0,
									initial.startTime + snappedDelta,
								);
								const duration = initial.endTime - initial.startTime;
								updates.push({
									id: annId,
									startTime: newAnnStart,
									endTime: newAnnStart + duration,
								});
							}
							// Single batched local update
							updateAnnotationsLocal(updates);

							// Move the selection cursor by the same delta
							if (cursorAtDragStart) {
								const cursorStart = cursorAtDragStart.startTime + snappedDelta;
								const cursorEnd =
									cursorAtDragStart.endTime !== null
										? cursorAtDragStart.endTime + snappedDelta
										: null;
								setSelectionCursor({
									trackRow: cursorAtDragStart.trackRow,
									trackRowEnd: cursorAtDragStart.trackRowEnd ?? null,
									startTime: Math.max(0, cursorStart),
									endTime: cursorEnd !== null ? Math.max(0, cursorEnd) : null,
								});
							}
						} else if (dragType === "resize-left") {
							// Resize all selected annotations from the left
							const newStart = snapToGrid(
								dragRef.current.startTime + deltaTime,
							);
							if (newStart < dragRef.current.endTime - 0.1) {
								const startDelta = newStart - dragRef.current.startTime;
								const updates: {
									id: number;
									startTime: number;
									endTime: number;
								}[] = [];
								for (const [annId, initial] of initialPositions) {
									const newAnnStart = Math.max(
										0,
										initial.startTime + startDelta,
									);
									// Don't let start go past end
									if (newAnnStart < initial.endTime - 0.1) {
										updates.push({
											id: annId,
											startTime: newAnnStart,
											endTime: initial.endTime,
										});
									}
								}
								if (updates.length > 0) {
									updateAnnotationsLocal(updates);
									// Update cursor
									if (cursorAtDragStart) {
										setSelectionCursor({
											trackRow: cursorAtDragStart.trackRow,
											trackRowEnd: cursorAtDragStart.trackRowEnd ?? null,
											startTime: Math.max(
												0,
												cursorAtDragStart.startTime + startDelta,
											),
											endTime: cursorAtDragStart.endTime,
										});
									}
								}
							}
						} else if (dragType === "resize-right") {
							// Resize all selected annotations from the right
							const newEnd = snapToGrid(dragRef.current.endTime + deltaTime);
							if (newEnd > dragRef.current.startTime + 0.1) {
								const endDelta = newEnd - dragRef.current.endTime;
								const updates: {
									id: number;
									startTime: number;
									endTime: number;
								}[] = [];
								for (const [annId, initial] of initialPositions) {
									const newAnnEnd = Math.min(
										durationSeconds,
										initial.endTime + endDelta,
									);
									// Don't let end go past start
									if (newAnnEnd > initial.startTime + 0.1) {
										updates.push({
											id: annId,
											startTime: initial.startTime,
											endTime: newAnnEnd,
										});
									}
								}
								if (updates.length > 0) {
									updateAnnotationsLocal(updates);
									// Update cursor
									if (cursorAtDragStart && cursorAtDragStart.endTime !== null) {
										setSelectionCursor({
											trackRow: cursorAtDragStart.trackRow,
											trackRowEnd: cursorAtDragStart.trackRowEnd ?? null,
											startTime: cursorAtDragStart.startTime,
											endTime: Math.min(
												durationSeconds,
												cursorAtDragStart.endTime + endDelta,
											),
										});
									}
								}
							}
						}
					};

					const handleUp = () => {
						// Mark drag complete so composite can run
						setIsDraggingAnnotation(false);

						// Persist all moved annotations to backend
						const idsToSave = Array.from(initialPositions.keys());
						persistAnnotations(idsToSave);

						dragRef.current.active = false;
						dragRef.current.annotation = null;
						window.removeEventListener("mousemove", handleMove);
						window.removeEventListener("mouseup", handleUp);
					};

					window.addEventListener("mousemove", handleMove);
					window.addEventListener("mouseup", handleUp);
					return;
				}

				// No annotation clicked - set selection cursor at click position
				const snappedTime = snapToGrid(clickTime);
				setSelectionCursor({
					trackRow: laneIdx,
					trackRowEnd: null,
					startTime: snappedTime,
					endTime: null,
				});
				setSelectedAnnotationIds([]);

				// Start cursor drag to create range selection
				cursorDragRef.current = {
					active: true,
					startTime: snappedTime,
					trackRow: laneIdx,
				};

				const handleCursorMove = (ev: MouseEvent) => {
					if (!cursorDragRef.current?.active || !containerRef.current) return;
					const moveRect = containerRef.current.getBoundingClientRect();
					const moveX =
						ev.clientX - moveRect.left + containerRef.current.scrollLeft;
					const moveY =
						ev.clientY - moveRect.top + containerRef.current.scrollTop;
					const moveTime = moveX / zoomRef.current;
					const snappedMoveTime = snapToGrid(moveTime);

					// Calculate current row from Y position
					const cursorLayout = computeLayout(zoomYRef.current);
					const totalTracks = Math.max(1, sortedZRef.current.length);
					const relativeY = moveY - cursorLayout.trackStartY;
					const currentRow = Math.max(
						0,
						Math.min(
							totalTracks - 1,
							Math.floor(relativeY / cursorLayout.trackHeight),
						),
					);

					const rangeStart = Math.min(
						cursorDragRef.current.startTime,
						snappedMoveTime,
					);
					const rangeEnd = Math.max(
						cursorDragRef.current.startTime,
						snappedMoveTime,
					);

					// Calculate row range
					const startRow = cursorDragRef.current.trackRow;
					const minRow = Math.min(startRow, currentRow);
					const maxRow = Math.max(startRow, currentRow);

					// Find annotations fully within the time range AND within the row range
					// Use small epsilon for floating point precision tolerance
					const EPSILON = 0.001; // 1ms tolerance
					const fullyContained = annotationsRef.current.filter((ann) => {
						const annRow = rowMapRef.current.get(ann.id) ?? -1;
						return (
							annRow >= minRow &&
							annRow <= maxRow &&
							ann.startTime >= rangeStart - EPSILON &&
							ann.endTime <= rangeEnd + EPSILON
						);
					});

					setSelectionCursor({
						trackRow: cursorDragRef.current.trackRow,
						trackRowEnd: currentRow !== startRow ? currentRow : null,
						startTime: cursorDragRef.current.startTime,
						endTime: snappedMoveTime,
					});
					setSelectedAnnotationIds(fullyContained.map((a) => a.id));
				};

				const handleCursorUp = () => {
					if (cursorDragRef.current?.active) {
						cursorDragRef.current.active = false;
					}
					window.removeEventListener("mousemove", handleCursorMove);
					window.removeEventListener("mouseup", handleCursorUp);
				};

				window.addEventListener("mousemove", handleCursorMove);
				window.addEventListener("mouseup", handleCursorUp);
				return;
			}

			// Clicked outside track area
			selectAnnotation(null);
			setSelectionCursor(null);
			forceRender((n) => n + 1);
		},
		[
			beatGrid,
			durationSeconds,
			getQuarterBeatSnap,
			getEighthBeatSnap,
			getSixteenthBeatSnap,
			getTripletBeatSnap,
			selectAnnotation,
			selectedAnnotationIds,
			setSelectionCursor,
			setSelectedAnnotationIds,
			setIsDraggingAnnotation,
			updateAnnotationsLocal,
			persistAnnotations,
			seek,
			setPlayheadPosition,
			snapToGrid,
		],
	);

	const handleCanvasDoubleClick = useCallback(
		(e: React.MouseEvent) => {
			if (dragRef.current.active || draggingPatternId !== null) return;

			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const x = e.clientX - rect.left + container.scrollLeft;
			const y = e.clientY - rect.top + container.scrollTop; // World Y
			const currentZoom = zoomRef.current;
			const dblLayout = computeLayout(zoomYRef.current);
			const totalHeight =
				dblLayout.trackStartY +
				Math.max(1, sortedZRef.current.length) * dblLayout.trackHeight;

			if (y < dblLayout.trackStartY || y >= totalHeight) return;

			const clickTime = x / currentZoom;
			const laneIdx = Math.floor(
				(y - dblLayout.trackStartY) / dblLayout.trackHeight,
			);
			const clicked = annotationsRef.current.find((ann) => {
				const annLane = rowMapRef.current.get(ann.id) ?? 0;
				return (
					annLane === laneIdx &&
					clickTime >= ann.startTime &&
					clickTime <= ann.endTime
				);
			});

			if (!clicked) return;

			e.preventDefault();
			e.stopPropagation();

			const pattern = patterns.find((p) => p.id === clicked.patternId);
			navigate(`/pattern/${clicked.patternId}`, {
				state: {
					name: pattern?.name ?? `Pattern ${clicked.patternId}`,
					from: `${location.pathname}${location.search}`,
					backLabel: trackName || "Track",
					instanceId: clicked.id,
				},
			});
		},
		[
			draggingPatternId,
			navigate,
			location.pathname,
			location.search,
			patterns,
			trackName,
		],
	);

	// GLOBAL MOUSE UP
	useEffect(() => {
		const handleGlobalMouseUp = () => {
			if (draggingPatternId !== null) {
				console.log("[Timeline] Global mouse up - clearing drag state");
				setDraggingPatternId(null);
				setDragPreview(null);
				setInsertionData(null);
			}
			if (playheadDragRef.current) {
				playheadDragRef.current = false;
			}
		};
		window.addEventListener("mouseup", handleGlobalMouseUp);
		return () => window.removeEventListener("mouseup", handleGlobalMouseUp);
	}, [draggingPatternId, setDraggingPatternId]);

	const handleCanvasMouseMove = useCallback(
		(e: React.MouseEvent) => {
			const container = containerRef.current;
			if (playheadDragRef.current && container) {
				const rect = container.getBoundingClientRect();
				const x = e.clientX - rect.left + container.scrollLeft;
				const time = Math.max(
					0,
					Math.min(durationSeconds, x / zoomRef.current),
				);
				seek(time);
				setPlayheadPosition(time);
				return;
			}

			if (draggingPatternId === null) {
				if (dragRef.current.active) {
					// During annotation drag, show appropriate cursor
					const canvas = canvasRef.current;
					if (canvas && dragRef.current.type?.startsWith("annotation-")) {
						if (dragRef.current.type === "annotation-move") {
							canvas.style.cursor = "grabbing";
						} else if (dragRef.current.type === "annotation-resize-left") {
							canvas.style.cursor = CURSOR_BRACKET_L;
						} else {
							canvas.style.cursor = CURSOR_BRACKET_R;
						}
					}
					return;
				}

				const container = containerRef.current;
				const canvas = canvasRef.current;
				if (!container || !canvas) return;

				const rect = container.getBoundingClientRect();
				const x = e.clientX - rect.left + container.scrollLeft;
				const y = e.clientY - rect.top + container.scrollTop; // World Y
				const moveLayout = computeLayout(zoomYRef.current);
				const totalHeight =
					moveLayout.trackStartY +
					Math.max(1, sortedZRef.current.length) * moveLayout.trackHeight;

				if (y >= moveLayout.trackStartY && y < totalHeight) {
					const clickTime = x / zoomRef.current;
					const relativeY = y - moveLayout.trackStartY;
					const laneIdx = Math.floor(relativeY / moveLayout.trackHeight);
					const handleSize = 8;

					// Check ALL annotations in this lane for header hover
					// Match the mousedown hit-test exactly: only inside annotation bounds
					for (const ann of annotationsRef.current) {
						const annRow = rowMapRef.current.get(ann.id) ?? 0;
						if (annRow !== laneIdx) continue;
						if (clickTime < ann.startTime || clickTime > ann.endTime) continue;

						const annTopY =
							moveLayout.trackStartY + annRow * moveLayout.trackHeight + 1;
						const inHeader = y >= annTopY && y < annTopY + ANNOTATION_HEADER_H;
						if (!inHeader) continue;

						const startX = ann.startTime * zoomRef.current;
						const endX = ann.endTime * zoomRef.current;

						// Check edges in header — bracket cursors like Ableton
						if (x - startX < handleSize) {
							canvas.style.cursor = CURSOR_BRACKET_L;
							return;
						}
						if (endX - x < handleSize) {
							canvas.style.cursor = CURSOR_BRACKET_R;
							return;
						}

						// Header body (grab cursor)
						canvas.style.cursor = "grab";
						return;
					}
				}

				canvas.style.cursor = "default";
				return;
			}

			const patternContainer = containerRef.current;
			if (!patternContainer) return;

			const rect = patternContainer.getBoundingClientRect();
			const currentZoom = zoomRef.current;
			let startTime =
				(e.clientX - rect.left + patternContainer.scrollLeft) / currentZoom;

			startTime = snapToGrid(startTime);

			const barLength = getOneBarLength(startTime);
			let endTime = startTime + barLength;

			if (beatGrid?.downbeats.length) {
				const afterDownbeats = beatGrid.downbeats.filter((b) => b > startTime);
				if (afterDownbeats.length > 0) {
					endTime = afterDownbeats[0];
				}
			}

			startTime = Math.max(0, startTime);
			endTime = Math.min(durationSeconds, endTime);

			const y = e.clientY - rect.top + patternContainer.scrollTop; // World Y
			const dragLayout = computeLayout(zoomYRef.current);
			const zOrderAsc = sortedZRef.current;
			const zRowsDesc = [...zOrderAsc].sort((a, b) => b - a); // Row 0 = highest z
			const totalTracks = zRowsDesc.length;

			if (y > dragLayout.trackAreaY) {
				// Treat the empty top lane as "above row 0"
				const relativeY = Math.max(0, y - dragLayout.trackStartY);
				const floatRow = relativeY / dragLayout.trackHeight;
				const visualRow = Math.floor(floatRow);

				// Determine if near boundary (Insert mode)
				const nearestBoundary = Math.round(floatRow);
				const distToBoundary = Math.abs(floatRow - nearestBoundary);
				const isBoundary =
					distToBoundary < 0.25 &&
					nearestBoundary >= 0 &&
					nearestBoundary <= totalTracks;

				if (isBoundary) {
					// INSERT MODE: position new z so it lands at this boundary after shift
					const targetZ = (() => {
						if (totalTracks === 0) return 0;
						if (nearestBoundary === 0) {
							// Above the top track — no shift needed
							return zRowsDesc[0] + 1;
						}
						if (nearestBoundary >= totalTracks) {
							// Below the bottom track — no shift needed
							return zRowsDesc[zRowsDesc.length - 1] - 1;
						}
						// Insert between two existing tracks
						const aboveIdx = nearestBoundary - 1;
						return zRowsDesc[aboveIdx];
					})();

					const lineY =
						dragLayout.trackStartY + nearestBoundary * dragLayout.trackHeight;
					// Above/below extremes don't need shifting — use "add"
					const needsShift =
						nearestBoundary > 0 && nearestBoundary < totalTracks;
					setInsertionData({
						type: needsShift ? "insert" : "add",
						zIndex: targetZ,
						y: lineY,
					});
				} else {
					// ADD MODE (Inside Lane)
					// Only valid if inside existing track range
					if (visualRow >= 0 && visualRow < totalTracks) {
						const targetZ = zRowsDesc[visualRow];
						setInsertionData({
							type: "add",
							zIndex: targetZ,
							row: visualRow,
						});
					} else if (visualRow >= totalTracks) {
						// Dragging into empty space below -> add at new bottom lane
						const lowestZ =
							zRowsDesc.length > 0 ? zRowsDesc[zRowsDesc.length - 1] : 0;
						const targetZ = lowestZ - 1;
						const lineY =
							dragLayout.trackStartY + totalTracks * dragLayout.trackHeight;
						setInsertionData({
							type: "add",
							zIndex: targetZ,
							y: lineY,
						});
					} else {
						setInsertionData(null);
					}
				}
			} else {
				setInsertionData(null);
			}

			let color = getCanvasColor("--chart-5");
			let name = "Pattern";

			if (draggingPatternId !== null) {
				const pattern = patterns.find((p) => p.id === draggingPatternId);
				if (pattern) {
					color = getPatternColor(pattern.id);
					name = pattern.name;
				}
			}

			if (
				dragPreview === null ||
				Math.abs(dragPreview.startTime - startTime) > 0.01 ||
				dragPreview.color !== color // Update color if changed (rare)
			) {
				setDragPreview((prev) => ({
					startTime,
					endTime,
					color: prev?.color || color,
					name: prev?.name || name,
				}));
			}
		},
		[
			draggingPatternId,
			beatGrid,
			durationSeconds,
			snapToGrid,
			getOneBarLength,
			dragPreview,
			patterns,
			selectedAnnotationIds,
			seek,
			setPlayheadPosition,
		],
	);

	const handleCanvasMouseUp = useCallback(
		(e: React.MouseEvent) => {
			if (playheadDragRef.current) {
				playheadDragRef.current = false;
				return;
			}

			if (draggingPatternId !== null && dragPreview && insertionData) {
				e.stopPropagation();

				const { type, zIndex } = insertionData;

				console.log("[Timeline] Mouse Up - Dropping Pattern", {
					patternId: draggingPatternId,
					startTime: dragPreview.startTime,
					endTime: dragPreview.endTime,
					type,
					zIndex,
				});

				if (type === "insert") {
					// Shift mode: Insert at targetZ, push others up in one batch
					const toShift = annotationsRef.current.filter(
						(a) => a.zIndex >= zIndex,
					);
					// Batch local update for instant visual feedback
					updateAnnotationsLocal(
						toShift.map((a) => ({
							id: a.id,
							zIndex: a.zIndex + 1,
						})),
					);
					// Persist shifts then create
					persistAnnotations(toShift.map((a) => a.id)).then(() => {
						createAnnotation({
							patternId: draggingPatternId,
							startTime: dragPreview.startTime,
							endTime: dragPreview.endTime,
							zIndex: zIndex,
						});
					});
				} else {
					// Add mode: Just place at targetZ
					createAnnotation({
						patternId: draggingPatternId,
						startTime: dragPreview.startTime,
						endTime: dragPreview.endTime,
						zIndex: zIndex,
					});
				}

				setDraggingPatternId(null);
				setDragPreview(null);
				setInsertionData(null);
				return;
			}

			if (dragRef.current.active) {
				return;
			}

			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const screenY = e.clientY - rect.top; // Screen Y

			if (screenY < computeLayout(zoomYRef.current).headerHeight) {
				const x = e.clientX - rect.left + container.scrollLeft;
				const time = x / zoomRef.current;
				const clamped = Math.max(0, Math.min(durationSeconds, time));

				seek(clamped);
				setPlayheadPosition(clamped);
				lastSyncPlayheadRef.current = clamped;
				lastSyncTsRef.current = now();
			}
		},
		[
			draggingPatternId,
			dragPreview,
			insertionData,
			createAnnotation,
			updateAnnotation,
			setDraggingPatternId,
			seek,
			setPlayheadPosition,
			durationSeconds,
		],
	);

	// KEYBOARD CONTROLS
	useEffect(() => {
		const handleKeyDown = (e: KeyboardEvent) => {
			// Ignore when typing in input elements
			const target = e.target as HTMLElement;
			if (
				target.tagName === "INPUT" ||
				target.tagName === "TEXTAREA" ||
				target.isContentEditable
			) {
				return;
			}

			const isMod = e.metaKey || e.ctrlKey;

			// Undo (Cmd+Z)
			if (isMod && e.key === "z" && !e.shiftKey) {
				e.preventDefault();
				if (trackId !== null) {
					void useUndoStore.getState().undo(trackId);
				}
				return;
			}

			// Redo (Cmd+Shift+Z)
			if (isMod && e.key === "z" && e.shiftKey) {
				e.preventDefault();
				if (trackId !== null) {
					void useUndoStore.getState().redo(trackId);
				}
				return;
			}

			// Split at cursor (Cmd+E)
			if (isMod && e.key === "e") {
				e.preventDefault();
				void splitAtCursor();
				return;
			}

			// Delete: range selection → deleteInRegion, else delete selected annotations
			if (e.key === "Delete" || e.key === "Backspace") {
				e.preventDefault();
				if (selectionCursor?.endTime !== null && selectionCursor !== null) {
					void deleteInRegion();
				} else if (selectedAnnotationIds.length > 0) {
					deleteAnnotations(selectedAnnotationIds);
				}
				return;
			}

			// Move annotations up/down (Alt+Up/Down)
			if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
				e.preventDefault();
				if (selectedAnnotationIds.length > 0) {
					void moveAnnotationsVertical(e.key === "ArrowUp" ? "up" : "down");
				}
				return;
			}

			// Copy (Cmd+C)
			if (isMod && e.key === "c") {
				e.preventDefault();
				copySelection();
				return;
			}

			// Cut (Cmd+X)
			if (isMod && e.key === "x") {
				e.preventDefault();
				void cutSelection();
				return;
			}

			// Paste (Cmd+V)
			if (isMod && e.key === "v") {
				e.preventDefault();
				paste();
				return;
			}

			// Duplicate (Cmd+D)
			if (isMod && e.key === "d") {
				e.preventDefault();
				duplicate();
				return;
			}

			// Auto-fit vertical zoom (H key)
			if (e.key === "h" || e.key === "H") {
				e.preventDefault();
				const container = containerRef.current;
				if (!container) return;
				const availableHeight = container.clientHeight;
				const numTracks = Math.max(1, sortedZRef.current.length);
				const idealZoomY =
					(availableHeight - HEADER_HEIGHT - 20) /
					(WAVEFORM_HEIGHT + numTracks * TRACK_HEIGHT);
				const clamped = Math.max(MIN_ZOOM_Y, Math.min(MAX_ZOOM_Y, idealZoomY));
				zoomYRef.current = clamped;
				setZoomY(clamped);
				needsDrawRef.current = true;
				return;
			}
		};
		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [
		trackId,
		selectedAnnotationIds,
		selectionCursor,
		deleteAnnotations,
		splitAtCursor,
		deleteInRegion,
		moveAnnotationsVertical,
		copySelection,
		cutSelection,
		paste,
		duplicate,
		setZoomY,
	]);

	// RESIZE HANDLER
	useEffect(() => {
		window.addEventListener("resize", draw);
		return () => window.removeEventListener("resize", draw);
	}, [draw]);

	// REDRAW ON DATA CHANGES
	useEffect(() => {
		draw();
	}, [draw]);

	const reactiveLayout = computeLayout(storeZoomY);
	const totalHeight =
		reactiveLayout.trackStartY +
		Math.max(1, sortedZ.length + 1) * reactiveLayout.trackHeight +
		20;

	const metrics = metricsDisplay ?? metricsRef.current;

	return (
		<div className="relative flex flex-col h-full bg-neutral-950 overflow-hidden select-none">
			{/* MINIMAP */}
			<div
				className="shrink-0 border-b border-neutral-800"
				style={{ height: MINIMAP_HEIGHT }}
			>
				<canvas
					ref={minimapRef}
					className="block w-full h-full"
					onMouseDown={handleMinimapDown}
					onMouseMove={handleMinimapHover}
				/>
			</div>

			{/* SCROLL CONTAINER */}
			<div
				ref={containerRef}
				onScroll={handleScroll}
				className="flex-1 overflow-x-auto overflow-y-auto relative"
				style={{ overscrollBehavior: "none" }}
			>
				{/* SPACER */}
				<div
					ref={spacerRef}
					style={{
						height: totalHeight,
						pointerEvents: "none",
					}}
				/>

				{/* CANVAS */}
				<canvas
					ref={canvasRef}
					className="sticky left-0 top-0 cursor-default"
					style={{
						marginTop: -totalHeight,
					}}
					onDoubleClick={handleCanvasDoubleClick}
					onMouseMove={handleCanvasMouseMove}
					onMouseUp={handleCanvasMouseUp}
					onMouseDown={handleCanvasMouseDown}
				/>
			</div>

			{/* BOTTOM BAR */}
			<TimelineShortcuts />
			<TimelineMetrics metrics={metrics} />
		</div>
	);
}
