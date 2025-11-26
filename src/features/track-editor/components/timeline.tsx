import { useCallback, useEffect, useRef, useState } from "react";
import {
	type TimelineAnnotation,
	useTrackEditorStore,
} from "../stores/use-track-editor-store";
import type { RenderMetrics } from "../types/timeline-types";
import {
	ALWAYS_DRAW,
	ANNOTATION_LANE_HEIGHT,
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
	const deleteAnnotation = useTrackEditorStore((s) => s.deleteAnnotation);
	const selectedAnnotationId = useTrackEditorStore(
		(s) => s.selectedAnnotationId,
	);
	const selectAnnotation = useTrackEditorStore((s) => s.selectAnnotation);
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

	// Keep annotations ref in sync
	useEffect(() => {
		annotationsRef.current = annotations;
	}, [annotations]);

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
		annotations,
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

		// Draw Track Background
		const trackY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
		ctx.fillStyle = "rgba(0, 0, 0, 0.2)";
		ctx.fillRect(0, trackY, width, TRACK_HEIGHT);

		ctx.strokeStyle = "#222222";
		ctx.beginPath();
		ctx.moveTo(0, trackY + TRACK_HEIGHT);
		ctx.lineTo(width, trackY + TRACK_HEIGHT);
		ctx.stroke();

		// Draw Annotations
		const annotationsStart = now();
		drawAnnotations(
			ctx,
			annotationsRef.current,
			startTime,
			endTime,
			currentZoom,
			scrollLeft,
			width,
			selectedAnnotationId,
			getBeatMetrics,
		);

		// Draw Drag Preview
		if (dragPreview) {
			drawDragPreview(ctx, dragPreview, currentZoom, scrollLeft);
		}

		sections.annotations = now() - annotationsStart;

		// Draw Playhead
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
		selectedAnnotationId,
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
				type: type!,
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

	// ANNOTATION CLICK/DRAG
	const handleCanvasMouseDown = useCallback(
		(e: React.MouseEvent) => {
			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const x = e.clientX - rect.left + container.scrollLeft;
			const y = e.clientY - rect.top;
			const currentZoom = zoomRef.current;

			// Playhead dragging in header
			if (y < HEADER_HEIGHT) {
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
			const annotationY = HEADER_HEIGHT + WAVEFORM_HEIGHT + TRACK_HEIGHT;
			if (y >= annotationY && y < annotationY + ANNOTATION_LANE_HEIGHT) {
				const clickTime = x / currentZoom;

				const clicked = annotationsRef.current.find(
					(ann) => clickTime >= ann.startTime && clickTime <= ann.endTime,
				);

				if (clicked) {
					selectAnnotation(clicked.id);
					forceRender((n) => n + 1);

					const annStartX = clicked.startTime * currentZoom;
					const annEndX = clicked.endTime * currentZoom;
					const handleSize = 8;

					let dragType: "move" | "resize-left" | "resize-right" = "move";
					if (x - annStartX < handleSize) dragType = "resize-left";
					else if (annEndX - x < handleSize) dragType = "resize-right";

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
							const duration =
								dragRef.current.endTime - dragRef.current.startTime;
							let newStart = snapToGrid(dragRef.current.startTime + deltaTime);
							newStart = Math.max(0, newStart);
							updateAnnotation({
								id: clicked.id,
								startTime: newStart,
								endTime: newStart + duration,
							});
						} else if (dragType === "resize-left") {
							const newStart = snapToGrid(
								dragRef.current.startTime + deltaTime,
							);
							if (newStart < dragRef.current.endTime - 0.1) {
								updateAnnotation({
									id: clicked.id,
									startTime: Math.max(0, newStart),
								});
							}
						} else if (dragType === "resize-right") {
							const newEnd = snapToGrid(dragRef.current.endTime + deltaTime);
							if (newEnd > dragRef.current.startTime + 0.1) {
								updateAnnotation({
									id: clicked.id,
									endTime: Math.min(durationSeconds, newEnd),
								});
							}
						}
					};

					const handleUp = () => {
						dragRef.current.active = false;
						dragRef.current.annotation = null;
						window.removeEventListener("mousemove", handleMove);
						window.removeEventListener("mouseup", handleUp);
					};

					window.addEventListener("mousemove", handleMove);
					window.addEventListener("mouseup", handleUp);
					return;
				}
			}

			selectAnnotation(null);
			forceRender((n) => n + 1);
		},
		[
			beatGrid,
			durationSeconds,
			selectAnnotation,
			updateAnnotation,
			seek,
			setPlayheadPosition,
		],
	);

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

	// GLOBAL MOUSE UP
	useEffect(() => {
		const handleGlobalMouseUp = () => {
			if (draggingPatternId !== null) {
				console.log("[Timeline] Global mouse up - clearing drag state");
				setDraggingPatternId(null);
				setDragPreview(null);
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
				const y = e.clientY - rect.top;
				const annotationY = HEADER_HEIGHT + WAVEFORM_HEIGHT + TRACK_HEIGHT;

				if (
					y >= annotationY &&
					y < annotationY + ANNOTATION_LANE_HEIGHT &&
					selectedAnnotationId !== null
				) {
					const ann = annotationsRef.current.find(
						(a) => a.id === selectedAnnotationId,
					);
					if (ann) {
						const startX = ann.startTime * zoomRef.current;
						const endX = ann.endTime * zoomRef.current;
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
				Math.abs(dragPreview.startTime - startTime) > 0.01
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
			selectedAnnotationId,
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

			if (draggingPatternId !== null && dragPreview) {
				e.stopPropagation();
				console.log("[Timeline] Mouse Up - Dropping Pattern", {
					patternId: draggingPatternId,
					startTime: dragPreview.startTime,
					endTime: dragPreview.endTime,
				});

				createAnnotation({
					patternId: draggingPatternId,
					startTime: dragPreview.startTime,
					endTime: dragPreview.endTime,
					zIndex: annotations.length,
				});

				setDraggingPatternId(null);
				setDragPreview(null);
				return;
			}

			if (dragRef.current.active) {
				return;
			}

			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const y = e.clientY - rect.top;

			if (y < HEADER_HEIGHT) {
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
			createAnnotation,
			annotations.length,
			setDraggingPatternId,
			seek,
			setPlayheadPosition,
			durationSeconds,
		],
	);

	// KEYBOARD CONTROLS
	useEffect(() => {
		const handleKeyDown = (e: KeyboardEvent) => {
			if (
				(e.key === "Delete" || e.key === "Backspace") &&
				selectedAnnotationId !== null
			) {
				deleteAnnotation(selectedAnnotationId);
			}
		};
		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [selectedAnnotationId, deleteAnnotation]);

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
		TRACK_HEIGHT +
		ANNOTATION_LANE_HEIGHT +
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
				className="flex-1 overflow-x-auto overflow-y-hidden relative"
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
