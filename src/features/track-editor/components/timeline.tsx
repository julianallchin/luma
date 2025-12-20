import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
	type SelectionCursor,
	type TimelineAnnotation,
	useTrackEditorStore,
} from "../stores/use-track-editor-store";
import type { RenderMetrics } from "../types/timeline-types";
import {
	ALWAYS_DRAW,
	getPatternColor,
	HEADER_HEIGHT,
	MINIMAP_HEIGHT,
	TRACK_HEIGHT,
	WAVEFORM_HEIGHT,
} from "../utils/timeline-constants";
import {
	drawAnnotations,
	drawBeatGrid,
	drawDragPreview,
	drawPlayhead,
	drawSelectionCursor,
	drawTimeRuler,
	drawWaveform,
} from "../utils/timeline-drawing";
import { useTimelineZoom } from "./hooks/use-timeline-zoom";
import { TimelineMetrics } from "./timeline-metrics";
import { useMinimapDrawing } from "./timeline-minimap";

const now = () =>
	typeof performance !== "undefined" ? performance.now() : Date.now();

export function Timeline() {
	// STORE STATE (Data Source)
	const durationSeconds = useTrackEditorStore((s) => s.durationSeconds);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const waveform = useTrackEditorStore((s) => s.waveform);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
	const isPlaying = useTrackEditorStore((s) => s.isPlaying);
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
	const copySelection = useTrackEditorStore((s) => s.copySelection);
	const cutSelection = useTrackEditorStore((s) => s.cutSelection);
	const paste = useTrackEditorStore((s) => s.paste);
	const duplicate = useTrackEditorStore((s) => s.duplicate);
	const draggingPatternId = useTrackEditorStore((s) => s.draggingPatternId);
	const setDraggingPatternId = useTrackEditorStore(
		(s) => s.setDraggingPatternId,
	);
	const seek = useTrackEditorStore((s) => s.seek);

	const durationMs = durationSeconds * 1000;

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
	const zoomRef = useRef(50); // pixels per second
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

	useEffect(() => {
		lastSyncPlayheadRef.current = playheadPosition;
		lastSyncTsRef.current = now();
		needsDrawRef.current = true;
		minimapDirtyRef.current = true;
	}, [playheadPosition]);

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

	const getBeatMetrics = useCallback(
		(startTime: number, endTime: number) => {
			if (!beatGrid?.beats.length || beatGrid.beats.length < 2) return null;
			const beats = beatGrid.beats;
			const avgBeat = getAverageBeatDuration();

			let precedingIndex = -1;
			for (let i = 0; i < beats.length; i++) {
				if (beats[i] <= startTime) {
					precedingIndex = i;
				} else {
					break;
				}
			}

			const prevBeatTime =
				precedingIndex >= 0 ? beats[precedingIndex] : beats[0];
			const nextBeatTime =
				precedingIndex + 1 < beats.length
					? beats[precedingIndex + 1]
					: prevBeatTime + avgBeat;
			const beatLength = Math.max(nextBeatTime - prevBeatTime, avgBeat || 0.25);

			const offsetBeats = (startTime - prevBeatTime) / beatLength;
			const startBeatNumber = Math.max(1, precedingIndex + 1 + offsetBeats);

			const beatsInside = beats.filter(
				(b) => b >= startTime && b < endTime,
			).length;
			const beatCount =
				beatsInside > 0
					? beatsInside
					: Math.max(1, Math.round((endTime - startTime) / beatLength));

			return { startBeatNumber, beatCount };
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
				Math.min(durationSeconds, lastSyncPlayheadRef.current + deltaSeconds),
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

		ctx.fillStyle = "#111111";
		ctx.fillRect(0, 0, width, height);

		const currentZoom = zoomRef.current;
		const scrollLeft = container.scrollLeft;
		const startTime = scrollLeft / currentZoom;
		const endTime = (scrollLeft + width) / currentZoom;

		// Draw Time Ruler Background
		ctx.fillStyle = "rgba(0, 0, 0, 0.4)";
		ctx.fillRect(0, 0, width, HEADER_HEIGHT);

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
			);
		} else {
			drawTimeRuler(ctx, startTime, endTime, currentZoom, scrollLeft);
		}

		ctx.strokeStyle = "#333333";
		ctx.beginPath();
		ctx.moveTo(0, HEADER_HEIGHT);
		ctx.lineTo(width, HEADER_HEIGHT);
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
		);
		sections.waveform = now() - waveformStart;

		const annotationsStart = now();
		// Use ref for insertionData to avoid closure staleness in draw loop
		const currentInsertionData = insertionDataRef.current;

		// TRACK RENDERING (SCROLLABLE)
		const trackStartY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
		ctx.save();
		// Clip to track area so we don't draw over header
		ctx.beginPath();
		ctx.rect(0, trackStartY, width, height - trackStartY);
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
			getBeatMetrics,
			rowMapRef.current,
			currentInsertionData,
		);

		// Draw Drag Preview (only in "add" mode, not "insert" mode)
		if (
			dragPreview &&
			currentInsertionData?.type === "add" &&
			currentInsertionData.row !== undefined
		) {
			const previewRow = currentInsertionData.row;
			drawDragPreview(ctx, dragPreview, currentZoom, scrollLeft, previewRow);
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
		getBeatMetrics,
		drawMinimap,
		isPlaying,
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
	useTimelineZoom(containerRef, spacerRef, zoomRef, durationMs, draw);

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
		[durationMs],
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
		requestAnimationFrame(draw);
	}, [draw]);

	const snapToGrid = useCallback(
		(time: number): number => {
			if (!beatGrid?.beats.length) return time;
			const nearest = beatGrid.beats.reduce((best, beat) =>
				Math.abs(beat - time) < Math.abs(best - time) ? beat : best,
			);
			return Math.abs(nearest - time) * zoomRef.current < 15 ? nearest : time;
		},
		[beatGrid],
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

			// Playhead dragging in header (Header is fixed at Screen Y=0)
			// But mouse Y is World Y.
			// Screen Y = Y - scrollTop.
			const screenY = y - container.scrollTop;
			if (screenY < HEADER_HEIGHT) {
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
			const trackStartY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
			const totalHeight =
				trackStartY + Math.max(1, sortedZRef.current.length) * TRACK_HEIGHT;

			if (y >= trackStartY && y < totalHeight) {
				const clickTime = x / currentZoom;
				const relativeY = y - trackStartY;
				const laneIdx = Math.floor(relativeY / TRACK_HEIGHT);

				// Find annotation in this lane
				const clicked = annotationsRef.current.find((ann) => {
					const annLane = rowMapRef.current.get(ann.id) ?? 0;
					return (
						annLane === laneIdx &&
						clickTime >= ann.startTime &&
						clickTime <= ann.endTime
					);
				});

				if (clicked) {
					// Check if clicked annotation is already in selection
					const alreadySelected = selectedAnnotationIds.includes(clicked.id);

					if (!alreadySelected) {
						// Clicking unselected annotation - select only this one
						selectAnnotation(clicked.id);
					}
					// If already selected, keep the current multi-selection

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

					const handleMove = (ev: MouseEvent) => {
						if (!dragRef.current.active || !dragRef.current.annotation) return;
						const dx = ev.clientX - dragRef.current.startX;
						const deltaTime = dx / zoomRef.current;

						const snapToGrid = (time: number) => {
							if (!beatGrid?.beats.length) return time;
							const nearest = beatGrid.beats.reduce((best, beat) =>
								Math.abs(beat - time) < Math.abs(best - time) ? beat : best,
							);
							return Math.abs(nearest - time) * zoomRef.current < 12
								? nearest
								: time;
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
					const trackStartY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
					const totalTracks = Math.max(1, sortedZRef.current.length);
					const relativeY = moveY - trackStartY;
					const currentRow = Math.max(
						0,
						Math.min(totalTracks - 1, Math.floor(relativeY / TRACK_HEIGHT)),
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
			selectAnnotation,
			selectedAnnotationIds,
			setSelectionCursor,
			setSelectedAnnotationIds,
			updateAnnotationsLocal,
			persistAnnotations,
			seek,
			setPlayheadPosition,
			snapToGrid,
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
				if (dragRef.current.active) return;

				const container = containerRef.current;
				const canvas = canvasRef.current;
				if (!container || !canvas) return;

				const rect = container.getBoundingClientRect();
				const x = e.clientX - rect.left + container.scrollLeft;
				const y = e.clientY - rect.top + container.scrollTop; // World Y
				const trackStartY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
				const totalHeight =
					trackStartY + Math.max(1, sortedZRef.current.length) * TRACK_HEIGHT;

				if (
					y >= trackStartY &&
					y < totalHeight &&
					selectedAnnotationIds.length > 0
				) {
					// Show resize cursor if hovering over selected annotation edges
					const selectedAnn = annotationsRef.current.find((a) =>
						selectedAnnotationIds.includes(a.id),
					);
					if (selectedAnn) {
						const startX = selectedAnn.startTime * zoomRef.current;
						const endX = selectedAnn.endTime * zoomRef.current;
						const handleSize = 8;

						if (
							Math.abs(x - startX) < handleSize ||
							Math.abs(x - endX) < handleSize
						) {
							canvas.style.cursor = "ew-resize";
							return;
						}
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
			const trackStartY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
			const zOrderAsc = sortedZRef.current;
			const zRowsDesc = [...zOrderAsc].sort((a, b) => b - a); // Row 0 = highest z
			const totalTracks = zRowsDesc.length;

			if (y > trackStartY) {
				const relativeY = y - trackStartY;
				const floatRow = relativeY / TRACK_HEIGHT;
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
							// Above the top track
							return zRowsDesc[0] + 1;
						}
						// Insert below the track above this boundary
						const aboveIdx = Math.min(
							nearestBoundary - 1,
							zRowsDesc.length - 1,
						);
						return zRowsDesc[aboveIdx];
					})();

					const lineY = trackStartY + nearestBoundary * TRACK_HEIGHT;
					setInsertionData({
						type: "insert",
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
						// Dragging into empty space below -> Insert at bottom
						// Use lowestZ so that all existing patterns shift up by 1
						const lowestZ =
							zRowsDesc.length > 0 ? zRowsDesc[zRowsDesc.length - 1] : 0;
						const targetZ = lowestZ;
						const lineY = trackStartY + totalTracks * TRACK_HEIGHT;
						setInsertionData({
							type: "insert",
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

			let color = "#8b5cf6";
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
					// Shift mode: Insert at targetZ, push others up
					const toShift = annotationsRef.current.filter(
						(a) => a.zIndex >= zIndex,
					);
					Promise.all(
						toShift.map((a) =>
							updateAnnotation({
								id: a.id,
								zIndex: a.zIndex + 1,
							}),
						),
					).then(() => {
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

			if (screenY < HEADER_HEIGHT) {
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
			const isMod = e.metaKey || e.ctrlKey;

			// Delete selected annotations
			if (
				(e.key === "Delete" || e.key === "Backspace") &&
				selectedAnnotationIds.length > 0
			) {
				e.preventDefault();
				deleteAnnotations(selectedAnnotationIds);
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
		};
		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [
		selectedAnnotationIds,
		deleteAnnotations,
		copySelection,
		cutSelection,
		paste,
		duplicate,
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

	const totalHeight =
		HEADER_HEIGHT +
		WAVEFORM_HEIGHT +
		Math.max(1, sortedZ.length + 1) * TRACK_HEIGHT +
		20;

	const metrics = metricsDisplay ?? metricsRef.current;

	return (
		<div className="flex flex-col h-full bg-neutral-950 overflow-hidden select-none">
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
					onMouseMove={handleCanvasMouseMove}
					onMouseUp={handleCanvasMouseUp}
					onMouseDown={handleCanvasMouseDown}
				/>
			</div>

			{/* METRICS */}
			<TimelineMetrics metrics={metrics} />
		</div>
	);
}
