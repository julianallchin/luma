import { useRef, useState, useEffect, useCallback } from "react";
import {
	useTrackEditorStore,
	type TimelineAnnotation,
} from "@/useTrackEditorStore";

// CONFIGURATION
const MIN_ZOOM = 5;
const MAX_ZOOM = 500;
const ZOOM_SENSITIVITY = 0.002;
const HEADER_HEIGHT = 28;
const WAVEFORM_HEIGHT = 64;
const TRACK_HEIGHT = 50;
const ANNOTATION_LANE_HEIGHT = 180; // Increased significantly
const MINIMAP_HEIGHT = 60;

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

function getPatternColor(patternId: number): string {
	return patternColors[patternId % patternColors.length];
}

export function Timeline() {
	// STORE STATE (Data Source)
	const durationSeconds = useTrackEditorStore((s) => s.durationSeconds);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const waveform = useTrackEditorStore((s) => s.waveform);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
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
	const [zoomDisplay, setZoomDisplay] = useState(50);
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

	// Keep annotations ref in sync
	useEffect(() => {
		annotationsRef.current = annotations;
	}, [annotations]);

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
		// Annotation dragging
		annotation: null as TimelineAnnotation | null,
		startTime: 0,
		endTime: 0,
	});

	// Initialize spacer width
	useEffect(() => {
		if (spacerRef.current && durationMs > 0) {
			spacerRef.current.style.width = `${(durationMs / 1000) * zoomRef.current}px`;
		}
		draw();
	}, [durationMs]);

	// --- DRAWING LOGIC ---

	const drawMinimap = useCallback(() => {
		const canvas = minimapRef.current;
		const container = containerRef.current;
		if (!canvas || !container || durationMs <= 0) return;

		const ctx = canvas.getContext("2d", { alpha: false });
		if (!ctx) return;

		const dpr = window.devicePixelRatio || 1;
		const width = container.clientWidth;
		const height = MINIMAP_HEIGHT;

		if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
			canvas.width = width * dpr;
			canvas.height = height * dpr;
			ctx.scale(dpr, dpr);
			canvas.style.width = `${width}px`;
			canvas.style.height = `${height}px`;
		}

		ctx.fillStyle = "#0d0d0d";
		ctx.fillRect(0, 0, width, height);

		const timeToPixel = width / durationMs;
		const currentZoom = zoomRef.current;
		const scrollLeft = container.scrollLeft;

		// Draw waveform in minimap
		if (waveform?.previewSamples?.length) {
			const samples = waveform.previewSamples;
			const numBuckets = samples.length / 2;
			const centerY = height / 2;
			ctx.fillStyle = "#6366f1";
			ctx.globalAlpha = 0.5;
			for (let i = 0; i < width; i++) {
				const bucketIndex = Math.floor((i / width) * numBuckets) * 2;
				const min = samples[bucketIndex] ?? 0;
				const max = samples[bucketIndex + 1] ?? 0;
				const yTop = centerY - max * (height / 2) * 0.8;
				const yBottom = centerY - min * (height / 2) * 0.8;
				const h = Math.abs(yBottom - yTop) || 1;
				ctx.fillRect(i, Math.min(yTop, yBottom), 1, h);
			}
			ctx.globalAlpha = 1.0;
		}

		// Draw annotations in minimap
		annotationsRef.current.forEach((ann) => {
			const x = ann.startTime * 1000 * timeToPixel;
			const w = Math.max(2, (ann.endTime - ann.startTime) * 1000 * timeToPixel);
			ctx.fillStyle = ann.patternColor || "#8b5cf6";
			ctx.globalAlpha = 0.7;
			ctx.fillRect(x, height - 12, w, 10);
		});
		ctx.globalAlpha = 1.0;

		// Draw viewport lens
		const visibleTimeStart = (scrollLeft / currentZoom) * 1000;
		const visibleTimeEnd = ((scrollLeft + width) / currentZoom) * 1000;
		const lensX = visibleTimeStart * timeToPixel;
		const lensW = Math.max(
			4,
			(visibleTimeEnd - visibleTimeStart) * timeToPixel,
		);

		ctx.fillStyle = "rgba(255, 255, 255, 0.06)";
		ctx.fillRect(lensX, 0, lensW, height);

		ctx.strokeStyle = "rgba(255, 255, 255, 0.3)";
		ctx.lineWidth = 1;
		ctx.strokeRect(lensX + 0.5, 0.5, lensW - 1, height - 1);

		// Lens handles
		ctx.fillStyle = "rgba(255, 255, 255, 0.5)";
		ctx.fillRect(lensX, 0, 3, height);
		ctx.fillRect(lensX + lensW - 3, 0, 3, height);

		// Playhead in minimap
		const playheadX = playheadPosition * 1000 * timeToPixel;
		ctx.fillStyle = "#f59e0b";
		ctx.fillRect(playheadX - 0.5, 0, 1, height);
	}, [durationMs, waveform, playheadPosition]);

	const draw = useCallback(() => {
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

		// --- Draw Time Ruler Background ---
		ctx.fillStyle = "rgba(0, 0, 0, 0.4)";
		ctx.fillRect(0, 0, width, HEADER_HEIGHT);

		ctx.font = '10px "SF Mono", "Geist Mono", monospace';

		// --- Draw Beat Grid & Ruler ---
		if (beatGrid) {
			// Create a set of downbeat times for O(1) lookup
			// We store them as integer milliseconds to avoid float equality issues
			const downbeatSet = new Set(
				beatGrid.downbeats.map((t) => Math.round(t * 1000)),
			);

			// 1. Draw regular beats
			ctx.strokeStyle = "rgba(139, 92, 246, 0.1)"; // Fainter
			ctx.fillStyle = "#666"; // Text color for beat numbers
			ctx.lineWidth = 1;

			// Helper to find which beat of the measure this is
			// (Naively assumes 4/4 for labeling if we just count beats between downbeats)
			// But for now, let's just label them 2, 3, 4...

			// Optimization: finding the measure index for a beat is expensive if we search every time.
			// But since we render only visible range, it's okay-ish.
			// Better: Pre-calculate measure map?
			// Actually, let's just rely on the visual grid for now and label downbeats clearly.

			for (const beat of beatGrid.beats) {
				if (beat < startTime || beat > endTime) continue;

				const beatTimeMs = Math.round(beat * 1000);
				if (downbeatSet.has(beatTimeMs)) continue; // Handled by downbeat loop

				const x = Math.floor(beat * currentZoom - scrollLeft) + 0.5;
				ctx.beginPath();
				ctx.moveTo(x, HEADER_HEIGHT); // Start from bottom of header
				ctx.lineTo(x, height);
				ctx.stroke();

				// Optional: Draw sub-beat numbers if zoomed in
				if (currentZoom > 100) {
					// Simple visual marker for beat
					ctx.beginPath();
					ctx.moveTo(x, HEADER_HEIGHT - 5);
					ctx.lineTo(x, HEADER_HEIGHT);
					ctx.stroke();
				}
			}

			// 2. Draw Downbeats (Measure Starts)
			ctx.strokeStyle = "rgba(139, 92, 246, 0.4)"; // Stronger
			ctx.fillStyle = "#ddd"; // Brighter text for measure numbers

			beatGrid.downbeats.forEach((downbeat, index) => {
				if (downbeat < startTime || downbeat > endTime) return;

				const x = Math.floor(downbeat * currentZoom - scrollLeft) + 0.5;

				// Grid line
				ctx.beginPath();
				ctx.moveTo(x, HEADER_HEIGHT - 12); // Extend up into ruler
				ctx.lineTo(x, height);
				ctx.stroke();

				// Label: "1", "2", "3"...
				ctx.fillText(`${index + 1}`, x + 4, HEADER_HEIGHT - 10);
			});
		} else {
			// Fallback: Draw time ruler if no beat grid
			const tickInterval = currentZoom < 50 ? 5 : 1;
			const firstTick = Math.floor(startTime / tickInterval) * tickInterval;

			for (let t = firstTick; t <= endTime; t += tickInterval) {
				const x = Math.floor(t * currentZoom - scrollLeft) + 0.5;
				const isMajor = t % 10 === 0;

				ctx.strokeStyle = isMajor ? "#404040" : "#262626";
				ctx.beginPath();
				ctx.moveTo(x, HEADER_HEIGHT - (isMajor ? 10 : 5));
				ctx.lineTo(x, HEADER_HEIGHT);
				ctx.stroke();

				if (isMajor) {
					ctx.fillStyle = "#888888";
					ctx.fillText(
						`${Math.floor(t / 60)}:${(t % 60).toString().padStart(2, "0")}`,
						x + 3,
						HEADER_HEIGHT - 12,
					);
				}
			}
		}

		ctx.strokeStyle = "#333333";
		ctx.beginPath();
		ctx.moveTo(0, HEADER_HEIGHT);
		ctx.lineTo(width, HEADER_HEIGHT);
		ctx.stroke();

		// --- Draw Waveform (Zoomed) ---
		const waveformY = HEADER_HEIGHT;

		// Waveform Lane Background
		ctx.fillStyle = "#0f0f0f";
		ctx.fillRect(0, waveformY, width, WAVEFORM_HEIGHT);

		// Waveform Divider
		ctx.strokeStyle = "#333333";
		ctx.beginPath();
		ctx.moveTo(0, waveformY + WAVEFORM_HEIGHT);
		ctx.lineTo(width, waveformY + WAVEFORM_HEIGHT);
		ctx.stroke();

		if (waveform?.fullSamples) {
			const samples = waveform.fullSamples;
			const numBuckets = samples.length / 2;
			const bucketsPerSecond = numBuckets / durationSeconds;

			// Calculate visible range indices
			const startBucket = Math.floor(startTime * bucketsPerSecond);
			const endBucket = Math.min(
				numBuckets,
				Math.ceil(endTime * bucketsPerSecond),
			);

			// Drawing constants
			const centerY = waveformY + WAVEFORM_HEIGHT / 2;
			const halfHeight = (WAVEFORM_HEIGHT - 8) / 2; // 4px padding top/bottom

			ctx.fillStyle = "#6366f1"; // Indigo-500
			ctx.beginPath();

			for (let i = startBucket; i < endBucket; i++) {
				const time = i / bucketsPerSecond;
				const x = Math.floor(time * currentZoom - scrollLeft);

				// Only draw if within view (double check x)
				if (x < -1 || x > width + 1) continue;

				const min = samples[i * 2];
				const max = samples[i * 2 + 1];

				const yTop = centerY - max * halfHeight;
				const yBottom = centerY - min * halfHeight;
				const h = Math.max(1, yBottom - yTop);

				// Draw bar
				ctx.rect(x, yTop, Math.max(1, currentZoom / bucketsPerSecond), h);
			}
			ctx.fill();
		}

		// --- Draw Track Background ---
		const trackY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
		ctx.fillStyle = "rgba(0, 0, 0, 0.2)";
		ctx.fillRect(0, trackY, width, TRACK_HEIGHT);

		ctx.strokeStyle = "#222222";
		ctx.beginPath();
		ctx.moveTo(0, trackY + TRACK_HEIGHT);
		ctx.lineTo(width, trackY + TRACK_HEIGHT);
		ctx.stroke();

		// --- Draw Annotations ---
		const anns = annotationsRef.current;
		for (const ann of anns) {
			if (ann.endTime < startTime || ann.startTime > endTime) continue;

			const x = Math.floor(ann.startTime * currentZoom - scrollLeft);
			const w = Math.max(
				4,
				Math.floor((ann.endTime - ann.startTime) * currentZoom),
			);
			const y = trackY + TRACK_HEIGHT + 4;
			const h = ANNOTATION_LANE_HEIGHT - 8;

			const isSelected = ann.id === selectedAnnotationId;

			ctx.fillStyle = ann.patternColor || "#8b5cf6";
			ctx.globalAlpha = isSelected ? 1 : 0.85;
			ctx.fillRect(x, y, w, h);

			if (isSelected) {
				// Selected border
				ctx.strokeStyle = "rgba(255, 255, 255, 0.9)";
				ctx.lineWidth = 1;
				ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);

				// Resize Handles
				ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
				ctx.fillRect(x, y, 6, h); // Left handle
				ctx.fillRect(x + w - 6, y, 6, h); // Right handle

				// Handle grips
				ctx.fillStyle = "rgba(0, 0, 0, 0.4)";
				ctx.fillRect(x + 2, y + h / 2 - 4, 2, 8);
				ctx.fillRect(x + w - 4, y + h / 2 - 4, 2, 8);
			} else {
				ctx.strokeStyle = "rgba(255, 255, 255, 0.15)";
				ctx.lineWidth = 1;
				ctx.strokeRect(x, y, w, h);
			}

			if (w > 30) {
				ctx.fillStyle = "white";
				ctx.globalAlpha = 0.9;
				ctx.save();
				ctx.beginPath();
				ctx.rect(x + 4, y, w - 8, h);
				ctx.clip();
				ctx.font = "11px system-ui, sans-serif";
				ctx.fillText(
					ann.patternName || `Pattern ${ann.patternId}`,
					x + 8,
					y + h / 2 + 4,
				);
				ctx.restore();
			}
			ctx.globalAlpha = 1;
		}

		// --- Draw Drag Preview ---
		if (dragPreview) {
			const previewX = Math.floor(
				dragPreview.startTime * currentZoom - scrollLeft,
			);
			const previewW = Math.max(
				4,
				Math.floor((dragPreview.endTime - dragPreview.startTime) * currentZoom),
			);
			const previewY = trackY + TRACK_HEIGHT + 4;
			const previewH = ANNOTATION_LANE_HEIGHT - 8;

			// Dotted outline
			ctx.setLineDash([4, 4]);
			ctx.strokeStyle = dragPreview.color;
			ctx.lineWidth = 2;
			ctx.strokeRect(
				previewX + 0.5,
				previewY + 0.5,
				previewW - 1,
				previewH - 1,
			);
			ctx.setLineDash([]);

			// Semi-transparent fill
			ctx.fillStyle = dragPreview.color;
			ctx.globalAlpha = 0.2;
			ctx.fillRect(previewX, previewY, previewW, previewH);
			ctx.globalAlpha = 1;

			// Label
			if (previewW > 40) {
				ctx.fillStyle = dragPreview.color;
				ctx.font = "11px system-ui, sans-serif";
				ctx.fillText(
					dragPreview.name,
					previewX + 8,
					previewY + previewH / 2 + 4,
				);
			}
		}

		// --- Draw Playhead ---
		if (playheadPosition >= startTime && playheadPosition <= endTime) {
			const x = Math.floor(playheadPosition * currentZoom - scrollLeft) + 0.5;
			ctx.strokeStyle = "#f59e0b";
			ctx.lineWidth = 1;
			ctx.beginPath();
			ctx.moveTo(x, 0);
			ctx.lineTo(x, height);
			ctx.stroke();

			ctx.fillStyle = "#f59e0b";
			ctx.beginPath();
			ctx.moveTo(x - 6, 0);
			ctx.lineTo(x + 6, 0);
			ctx.lineTo(x, 8);
			ctx.closePath();
			ctx.fill();
		}

		drawMinimap();
	}, [
		durationMs,
		beatGrid,
		playheadPosition,
		selectedAnnotationId,
		dragPreview,
		drawMinimap,
	]);

	// --- MINIMAP INTERACTION ---

	const handleMinimapDown = useCallback(
		(e: React.MouseEvent) => {
			const canvas = minimapRef.current;
			const container = containerRef.current;
			if (!canvas || !container) return;

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
			if (Math.abs(x - lensX) < handleSize) {
				type = "resize-left";
			} else if (Math.abs(x - (lensX + lensW)) < handleSize) {
				type = "resize-right";
			} else if (x > lensX && x < lensX + lensW) {
				type = "move";
			} else {
				// Click outside lens - jump to position
				const clickTime = (x / width) * durationMs;
				const targetPixel = (clickTime / 1000) * currentZoom;
				container.scrollLeft = targetPixel - container.clientWidth / 2;
				draw();
				return;
			}

			dragRef.current = {
				...dragRef.current,
				active: true,
				type,
				startX: e.clientX,
				startScroll: scrollLeft,
				startZoom: currentZoom,
				startLensX: lensX,
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
					const clampedZoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, newZoom));
					const initialStartTime = (startScroll / startZoom) * 1000;
					const newScroll = (initialStartTime / 1000) * clampedZoom;

					zoomRef.current = clampedZoom;
					if (spacerRef.current) {
						spacerRef.current.style.width = `${(durationMs / 1000) * clampedZoom}px`;
					}
					if (containerRef.current) {
						containerRef.current.scrollLeft = newScroll;
					}
					setZoomDisplay(clampedZoom);
				} else if (type === "resize-left") {
					const newLensW = Math.max(10, startLensW - dx);
					const newLensX = startLensX + dx;
					const newVisibleDuration = newLensW / timeToPixel;
					const newZoom = containerWidth / (newVisibleDuration / 1000);
					const clampedZoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, newZoom));
					const newStartTime = newLensX / timeToPixel;
					const newScroll = (newStartTime / 1000) * clampedZoom;

					zoomRef.current = clampedZoom;
					if (spacerRef.current) {
						spacerRef.current.style.width = `${(durationMs / 1000) * clampedZoom}px`;
					}
					if (containerRef.current) {
						containerRef.current.scrollLeft = newScroll;
					}
					setZoomDisplay(clampedZoom);
				}
				draw();
			};

			const handleUp = () => {
				dragRef.current.active = false;
				window.removeEventListener("mousemove", handleMove);
				window.removeEventListener("mouseup", handleUp);
			};

			window.addEventListener("mousemove", handleMove);
			window.addEventListener("mouseup", handleUp);
		},
		[durationMs, draw],
	);

	const handleMinimapHover = useCallback(
		(e: React.MouseEvent) => {
			if (dragRef.current.active) return;
			const canvas = minimapRef.current;
			const container = containerRef.current;
			if (!canvas || !container) return;

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

	// --- MAIN WHEEL LOGIC (Synchronous Zoom) ---
	useEffect(() => {
		const container = containerRef.current;
		const spacer = spacerRef.current;
		if (!container || !spacer || durationMs <= 0) return;

		const handleWheel = (e: WheelEvent) => {
			if (e.metaKey || e.ctrlKey) {
				e.preventDefault();

				const rect = container.getBoundingClientRect();
				const mouseX = e.clientX - rect.left;
				const currentScrollLeft = container.scrollLeft;
				const currentZoom = zoomRef.current;

				// Time at cursor is invariant
				const timeAtCursor = (mouseX + currentScrollLeft) / currentZoom;

				// Calculate new zoom
				const delta = -e.deltaY;
				const scaleMultiplier = Math.exp(delta * ZOOM_SENSITIVITY);
				const newZoom = Math.max(
					MIN_ZOOM,
					Math.min(MAX_ZOOM, currentZoom * scaleMultiplier),
				);

				// SYNCHRONOUS UPDATES
				zoomRef.current = newZoom;

				// Resize spacer
				spacer.style.width = `${(durationMs / 1000) * newZoom}px`;

				// Move camera to keep cursor stable
				const newScrollLeft = timeAtCursor * newZoom - mouseX;
				container.scrollLeft = newScrollLeft;

				// Draw immediately
				draw();
				setZoomDisplay(newZoom);
			}
		};

		container.addEventListener("wheel", handleWheel, { passive: false });
		return () => container.removeEventListener("wheel", handleWheel);
	}, [durationMs, draw]);

	// --- SCROLL HANDLER ---
	const handleScroll = useCallback(() => {
		requestAnimationFrame(draw);
	}, [draw]);

	// --- RULER CLICK (Set Playhead) ---
	// Use handleCanvasMouseUp for this logic instead to avoid conflicts
	// Keeping this ref for potential future use if needed, but removing the unused function
	// const handleCanvasClick = ...

	// --- ANNOTATION CLICK/DRAG ---
	const handleCanvasMouseDown = useCallback(
		(e: React.MouseEvent) => {
			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const x = e.clientX - rect.left + container.scrollLeft;
			const y = e.clientY - rect.top;
			const currentZoom = zoomRef.current;

			// Check if clicking in annotation lane
			const annotationY = HEADER_HEIGHT + WAVEFORM_HEIGHT + TRACK_HEIGHT;
			if (y >= annotationY && y < annotationY + ANNOTATION_LANE_HEIGHT) {
				const clickTime = x / currentZoom;

				// Find clicked annotation
				const clicked = annotationsRef.current.find(
					(ann) => clickTime >= ann.startTime && clickTime <= ann.endTime,
				);

				if (clicked) {
					selectAnnotation(clicked.id);
					forceRender((n) => n + 1);

					// Check for resize handles
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

			// Deselect if clicking elsewhere
			selectAnnotation(null);
			forceRender((n) => n + 1);
		},
		[beatGrid, durationSeconds, selectAnnotation, updateAnnotation],
	);

	// Helper: Calculate 1 bar length from downbeats
	const getOneBarLength = useCallback(
		(_startTime: number): number => {
			if (!beatGrid?.downbeats.length || beatGrid.downbeats.length < 2) {
				// Fallback: ~2 seconds if no beat grid
				return 2;
			}
			// Find average bar length from downbeats
			let totalBarLength = 0;
			for (let i = 1; i < beatGrid.downbeats.length; i++) {
				totalBarLength += beatGrid.downbeats[i] - beatGrid.downbeats[i - 1];
			}
			return totalBarLength / (beatGrid.downbeats.length - 1);
		},
		[beatGrid],
	);

	// Helper: Snap time to nearest beat
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

	// --- GLOBAL MOUSE UP (Clear Dragging) ---
	useEffect(() => {
		const handleGlobalMouseUp = () => {
			if (draggingPatternId !== null) {
				console.log("[Timeline] Global mouse up - clearing drag state");
				setDraggingPatternId(null);
				setDragPreview(null);
			}
		};
		window.addEventListener("mouseup", handleGlobalMouseUp);
		return () => window.removeEventListener("mouseup", handleGlobalMouseUp);
	}, [draggingPatternId, setDraggingPatternId]);

	// --- CANVAS MOUSE INTERACTION (Drag & Drop + Playhead) ---

	const handleCanvasMouseMove = useCallback(
		(e: React.MouseEvent) => {
			// If we are NOT dragging a pattern, we might be dragging an existing annotation
			if (draggingPatternId === null) {
				// Use existing annotation drag logic if active
				if (dragRef.current.active) return;

				// Update cursor for resize handles
				const container = containerRef.current;
				const canvas = canvasRef.current;
				if (!container || !canvas) return;

				const rect = container.getBoundingClientRect();
				const x = e.clientX - rect.left + container.scrollLeft;
				const y = e.clientY - rect.top;
				const annotationY = HEADER_HEIGHT + WAVEFORM_HEIGHT + TRACK_HEIGHT;

				// Only check if we are in the annotation lane
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

						// Check handles relative to scroll
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

			// LOGIC FOR DRAGGING NEW PATTERN
			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const currentZoom = zoomRef.current;
			let startTime =
				(e.clientX - rect.left + container.scrollLeft) / currentZoom;

			// Snap to beat grid
			startTime = snapToGrid(startTime);

			// Calculate 1 bar length
			const barLength = getOneBarLength(startTime);
			let endTime = startTime + barLength;

			// Snap end to nearest downbeat if possible
			if (beatGrid?.downbeats.length) {
				const afterDownbeats = beatGrid.downbeats.filter((b) => b > startTime);
				if (afterDownbeats.length > 0) {
					endTime = afterDownbeats[0];
				}
			}

			startTime = Math.max(0, startTime);
			endTime = Math.min(durationSeconds, endTime);

			// Get color and name
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
		],
	);

	const handleCanvasMouseUp = useCallback(
		(e: React.MouseEvent) => {
			// 1. If we are dragging a NEW pattern
			if (draggingPatternId !== null && dragPreview) {
				e.stopPropagation(); // Prevent global clear from firing first (though it bubbles later)
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

			// 2. Normal Click (Set Playhead) - only if not dragging an existing annotation
			if (dragRef.current.active) {
				// Was dragging annotation, do nothing here (handled by window listeners)
				return;
			}

			// Reuse existing click logic for playhead
			const container = containerRef.current;
			if (!container) return;

			const rect = container.getBoundingClientRect();
			const y = e.clientY - rect.top;

			// Only set playhead if clicking in header area
			if (y < HEADER_HEIGHT) {
				const x = e.clientX - rect.left + container.scrollLeft;
				const time = x / zoomRef.current;
				const clamped = Math.max(0, Math.min(durationSeconds, time));

				seek(clamped);
				setPlayheadPosition(clamped);
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

	// --- KEYBOARD CONTROLS ---
	useEffect(() => {
		const handleKeyDown = (e: KeyboardEvent) => {
			// Delete
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

	// --- RESIZE HANDLER ---
	useEffect(() => {
		window.addEventListener("resize", draw);
		return () => window.removeEventListener("resize", draw);
	}, [draw]);

	// --- REDRAW ON DATA CHANGES ---
	useEffect(() => {
		draw();
	}, [annotations, playheadPosition, dragPreview, draw, waveform]);

	const totalHeight =
		HEADER_HEIGHT +
		WAVEFORM_HEIGHT +
		TRACK_HEIGHT +
		ANNOTATION_LANE_HEIGHT +
		20;

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
				{/* SPACER (drives scrollbar width) */}
				<div
					ref={spacerRef}
					style={{
						width: `${(durationMs / 1000) * zoomDisplay}px`,
						height: totalHeight,
						pointerEvents: "none",
					}}
				/>

				{/* CANVAS (sticky overlay) */}
				<canvas
					ref={canvasRef}
					className="sticky left-0 top-0 cursor-default"
					style={{
						marginTop: -totalHeight,
					}}
					// Replaced legacy drag events with mouse events for custom drag
					onMouseMove={handleCanvasMouseMove}
					onMouseUp={handleCanvasMouseUp}
					onMouseDown={handleCanvasMouseDown}
				/>
			</div>

			{/* ZOOM INDICATOR */}
			<div className="absolute bottom-2 right-2 px-2 py-1 bg-neutral-900/90 rounded text-[10px] text-neutral-400 font-mono backdrop-blur-sm border border-neutral-800">
				{zoomDisplay.toFixed(0)} px/s
			</div>
		</div>
	);
}
