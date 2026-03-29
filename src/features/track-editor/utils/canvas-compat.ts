/**
 * Creates an offscreen canvas, falling back to a hidden HTMLCanvasElement
 * on older WebKit (e.g. macOS Ventura) that lacks OffscreenCanvas.
 */
export function createOffscreenCanvas(
	width: number,
	height: number,
): HTMLCanvasElement | OffscreenCanvas {
	if (typeof OffscreenCanvas !== "undefined") {
		return new OffscreenCanvas(width, height);
	}
	const canvas = document.createElement("canvas");
	canvas.width = width;
	canvas.height = height;
	return canvas;
}
