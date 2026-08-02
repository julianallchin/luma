import { useEffect, useRef } from "react";
import {
	computeLayout,
	MAX_ZOOM,
	MAX_ZOOM_Y,
	MIN_ZOOM,
	MIN_ZOOM_Y,
	ZOOM_SENSITIVITY,
	ZOOM_Y_SENSITIVITY,
} from "../../utils/timeline-constants";

export type VerticalZoomAnchor = {
	pixel: number;
	rowsFromBottom: number;
};

type WebKitGestureEvent = Event & {
	clientX: number;
	scale: number;
};

export function useTimelineZoom(
	containerRef: React.RefObject<HTMLDivElement | null>,
	spacerRef: React.RefObject<HTMLDivElement | null>,
	zoomRef: React.MutableRefObject<number>,
	durationMs: number,
	draw: () => void,
	onZoomChange?: (zoom: number) => void,
	zoomYRef?: React.MutableRefObject<number>,
	onZoomYChange?: (zoomY: number, anchor: VerticalZoomAnchor) => void,
) {
	const gestureRef = useRef<{
		startZoom: number;
		time: number;
		pixel: number;
	} | null>(null);
	const gestureScaleRef = useRef(1);
	const gestureFrameRef = useRef<number | null>(null);
	const wheelAnchorRef = useRef<{ time: number; pixel: number } | null>(null);
	const wheelEndTimeoutRef = useRef<number | null>(null);
	const commandZoomTargetRef = useRef<{
		time: number;
		pixel: number;
		isActive: boolean;
	} | null>(null);
	const commandWheelTimeoutRef = useRef<number | null>(null);

	useEffect(() => {
		const container = containerRef.current;
		const spacer = spacerRef.current;
		if (!container || !spacer || durationMs <= 0) return;

		const applyHorizontalZoom = (
			newZoom: number,
			targetTime: number,
			targetPixel: number,
		) => {
			if (Math.abs(newZoom - zoomRef.current) < 0.0001) return;
			zoomRef.current = newZoom;
			onZoomChange?.(newZoom);
			spacer.style.width = `${(durationMs / 1000) * newZoom}px`;
			void spacer.offsetWidth;
			container.scrollLeft = targetTime * newZoom - targetPixel;
			draw();
		};

		const horizontalAnchorAt = (clientX: number) => {
			const rect = container.getBoundingClientRect();
			const pixel = clientX - rect.left;
			return {
				pixel,
				time: (pixel + container.scrollLeft) / zoomRef.current,
			};
		};

		const handleWheel = (e: WheelEvent) => {
			if (gestureRef.current && e.ctrlKey && !e.metaKey) {
				e.preventDefault();
				return;
			}
			if (e.altKey && zoomYRef) {
				e.preventDefault();

				const rect = container.getBoundingClientRect();
				const pixel = e.clientY - rect.top;
				const currentLayout = computeLayout(zoomYRef.current);
				if (pixel < currentLayout.trackAreaY) return;
				const anchor = {
					pixel,
					rowsFromBottom:
						(container.scrollHeight - (container.scrollTop + pixel)) /
						currentLayout.trackHeight,
				};
				const delta = -e.deltaY;
				const scaleMultiplier = Math.exp(delta * ZOOM_Y_SENSITIVITY);
				const newZoomY = Math.max(
					MIN_ZOOM_Y,
					Math.min(MAX_ZOOM_Y, zoomYRef.current * scaleMultiplier),
				);

				zoomYRef.current = newZoomY;
				if (onZoomYChange) {
					onZoomYChange(newZoomY, anchor);
				} else {
					draw();
				}
			} else if (e.metaKey) {
				// Keep Command-scroll isolated on its original, stable path.
				e.preventDefault();

				const rect = container.getBoundingClientRect();
				const mouseX = e.clientX - rect.left;
				const currentScrollLeft = container.scrollLeft;
				const currentZoom = zoomRef.current;
				const timeAtCursor = (mouseX + currentScrollLeft) / currentZoom;

				if (!commandZoomTargetRef.current?.isActive) {
					commandZoomTargetRef.current = {
						time: timeAtCursor,
						pixel: mouseX,
						isActive: true,
					};
				}

				const targetTime = commandZoomTargetRef.current.time;
				const targetPixel = commandZoomTargetRef.current.pixel;
				const scaleMultiplier = Math.exp(-e.deltaY * ZOOM_SENSITIVITY);
				const newZoom = Math.max(
					MIN_ZOOM,
					Math.min(MAX_ZOOM, currentZoom * scaleMultiplier),
				);

				zoomRef.current = newZoom;
				onZoomChange?.(newZoom);
				spacer.style.width = `${(durationMs / 1000) * newZoom}px`;
				void spacer.offsetWidth;
				container.scrollLeft = targetTime * newZoom - targetPixel;

				if (commandWheelTimeoutRef.current !== null) {
					window.clearTimeout(commandWheelTimeoutRef.current);
				}
				commandWheelTimeoutRef.current = window.setTimeout(() => {
					if (commandZoomTargetRef.current) {
						commandZoomTargetRef.current.isActive = false;
					}
					commandWheelTimeoutRef.current = null;
				}, 100);

				draw();
			} else if (e.ctrlKey) {
				e.preventDefault();

				if (!wheelAnchorRef.current) {
					wheelAnchorRef.current = horizontalAnchorAt(e.clientX);
				}
				const anchor = wheelAnchorRef.current;
				const currentZoom = zoomRef.current;
				const delta = -e.deltaY;
				// Chromium-style trackpad pinch uses smaller deltas than a
				// modifier-wheel gesture.
				const scaleMultiplier = Math.exp(delta * 0.01);
				const newZoom = Math.max(
					MIN_ZOOM,
					Math.min(MAX_ZOOM, currentZoom * scaleMultiplier),
				);

				applyHorizontalZoom(newZoom, anchor.time, anchor.pixel);
				if (wheelEndTimeoutRef.current !== null) {
					window.clearTimeout(wheelEndTimeoutRef.current);
				}
				wheelEndTimeoutRef.current = window.setTimeout(() => {
					wheelAnchorRef.current = null;
					wheelEndTimeoutRef.current = null;
				}, 120);
			}
		};

		const handleGestureStart = (event: Event) => {
			const e = event as WebKitGestureEvent;
			e.preventDefault();
			if (wheelEndTimeoutRef.current !== null) {
				window.clearTimeout(wheelEndTimeoutRef.current);
				wheelEndTimeoutRef.current = null;
			}
			wheelAnchorRef.current = null;
			const anchor = horizontalAnchorAt(e.clientX);
			gestureRef.current = {
				startZoom: zoomRef.current,
				...anchor,
			};
			gestureScaleRef.current = 1;
		};

		const handleGestureChange = (event: Event) => {
			const e = event as WebKitGestureEvent;
			if (!gestureRef.current) return;
			e.preventDefault();
			gestureScaleRef.current = e.scale;
			if (gestureFrameRef.current !== null) return;

			gestureFrameRef.current = requestAnimationFrame(() => {
				gestureFrameRef.current = null;
				const gesture = gestureRef.current;
				if (!gesture) return;
				const newZoom = Math.max(
					MIN_ZOOM,
					Math.min(MAX_ZOOM, gesture.startZoom * gestureScaleRef.current),
				);
				applyHorizontalZoom(newZoom, gesture.time, gesture.pixel);
			});
		};

		const handleGestureEnd = (event: Event) => {
			event.preventDefault();
			if (gestureFrameRef.current !== null) {
				cancelAnimationFrame(gestureFrameRef.current);
				gestureFrameRef.current = null;
				const gesture = gestureRef.current;
				if (gesture) {
					const newZoom = Math.max(
						MIN_ZOOM,
						Math.min(MAX_ZOOM, gesture.startZoom * gestureScaleRef.current),
					);
					applyHorizontalZoom(newZoom, gesture.time, gesture.pixel);
				}
			}
			gestureRef.current = null;
		};

		container.addEventListener("wheel", handleWheel, { passive: false });
		container.addEventListener("gesturestart", handleGestureStart, {
			passive: false,
		});
		container.addEventListener("gesturechange", handleGestureChange, {
			passive: false,
		});
		container.addEventListener("gestureend", handleGestureEnd, {
			passive: false,
		});
		return () => {
			if (gestureFrameRef.current !== null) {
				cancelAnimationFrame(gestureFrameRef.current);
				gestureFrameRef.current = null;
			}
			if (wheelEndTimeoutRef.current !== null) {
				window.clearTimeout(wheelEndTimeoutRef.current);
				wheelEndTimeoutRef.current = null;
			}
			wheelAnchorRef.current = null;
			container.removeEventListener("wheel", handleWheel);
			container.removeEventListener("gesturestart", handleGestureStart);
			container.removeEventListener("gesturechange", handleGestureChange);
			container.removeEventListener("gestureend", handleGestureEnd);
		};
	}, [
		durationMs,
		draw,
		containerRef,
		spacerRef,
		zoomRef,
		onZoomChange,
		zoomYRef,
		onZoomYChange,
	]);
}
