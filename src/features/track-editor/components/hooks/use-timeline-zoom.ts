import { useEffect, useRef } from "react";
import {
	MAX_ZOOM,
	MAX_ZOOM_Y,
	MIN_ZOOM,
	MIN_ZOOM_Y,
	ZOOM_SENSITIVITY,
	ZOOM_Y_SENSITIVITY,
} from "../../utils/timeline-constants";

export function useTimelineZoom(
	containerRef: React.RefObject<HTMLDivElement | null>,
	spacerRef: React.RefObject<HTMLDivElement | null>,
	zoomRef: React.MutableRefObject<number>,
	durationMs: number,
	draw: () => void,
	onZoomChange?: (zoom: number) => void,
	zoomYRef?: React.MutableRefObject<number>,
	onZoomYChange?: (zoomY: number) => void,
) {
	const zoomTargetRef = useRef<{
		time: number;
		pixel: number;
		isActive: boolean;
	} | null>(null);
	const wheelTimeoutRef = useRef<number | null>(null);

	useEffect(() => {
		const container = containerRef.current;
		const spacer = spacerRef.current;
		if (!container || !spacer || durationMs <= 0) return;

		const handleWheel = (e: WheelEvent) => {
			if (e.altKey && zoomYRef) {
				e.preventDefault();

				const delta = -e.deltaY;
				const scaleMultiplier = Math.exp(delta * ZOOM_Y_SENSITIVITY);
				const newZoomY = Math.max(
					MIN_ZOOM_Y,
					Math.min(MAX_ZOOM_Y, zoomYRef.current * scaleMultiplier),
				);

				zoomYRef.current = newZoomY;
				onZoomYChange?.(newZoomY);
				draw();
			} else if (e.metaKey || e.ctrlKey) {
				e.preventDefault();

				const rect = container.getBoundingClientRect();
				const mouseX = e.clientX - rect.left;
				const currentScrollLeft = container.scrollLeft;
				const currentZoom = zoomRef.current;

				const timeAtCursor = (mouseX + currentScrollLeft) / currentZoom;

				if (!zoomTargetRef.current?.isActive) {
					zoomTargetRef.current = {
						time: timeAtCursor,
						pixel: mouseX,
						isActive: true,
					};
				}

				const targetTime = zoomTargetRef.current.time;
				const targetPixel = zoomTargetRef.current.pixel;

				const delta = -e.deltaY;
				const scaleMultiplier = Math.exp(delta * ZOOM_SENSITIVITY);
				const newZoom = Math.max(
					MIN_ZOOM,
					Math.min(MAX_ZOOM, currentZoom * scaleMultiplier),
				);

				zoomRef.current = newZoom;
				onZoomChange?.(newZoom);
				spacer.style.width = `${(durationMs / 1000) * newZoom}px`;
				void spacer.offsetWidth;

				const newScrollLeft = targetTime * newZoom - targetPixel;
				container.scrollLeft = newScrollLeft;

				if (wheelTimeoutRef.current) {
					window.clearTimeout(wheelTimeoutRef.current);
				}
				wheelTimeoutRef.current = window.setTimeout(() => {
					if (zoomTargetRef.current) {
						zoomTargetRef.current.isActive = false;
					}
				}, 100);

				draw();
			}
		};

		container.addEventListener("wheel", handleWheel, { passive: false });
		return () => container.removeEventListener("wheel", handleWheel);
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
