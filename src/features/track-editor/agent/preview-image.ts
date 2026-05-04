import type { AnnotationPreview } from "@/bindings/schema";

/**
 * Encode an AnnotationPreview's RGBA pixel buffer to a base64 PNG. Heatmaps
 * are tiny (≤ 512×32) so the cost is trivial.
 *
 * Heatmaps are also visually dense at native resolution, so we upscale with
 * nearest-neighbour to give the vision model larger pixels to reason about.
 */
export async function previewToPngBase64(
	preview: AnnotationPreview,
	scale = 4,
): Promise<string> {
	const { width, height, pixels } = preview;
	const arr = new Uint8ClampedArray(pixels);
	const imageData = new ImageData(arr, width, height);

	const src = new OffscreenCanvas(width, height);
	const srcCtx = src.getContext("2d");
	if (!srcCtx) throw new Error("Failed to get 2d context for source canvas");
	srcCtx.putImageData(imageData, 0, 0);

	const outW = Math.max(1, Math.round(width * scale));
	const outH = Math.max(1, Math.round(height * scale));
	const dst = new OffscreenCanvas(outW, outH);
	const dstCtx = dst.getContext("2d");
	if (!dstCtx) throw new Error("Failed to get 2d context for output canvas");
	dstCtx.imageSmoothingEnabled = false;
	dstCtx.drawImage(src, 0, 0, outW, outH);

	const blob = await dst.convertToBlob({ type: "image/png" });
	const buf = await blob.arrayBuffer();
	return arrayBufferToBase64(buf);
}

function arrayBufferToBase64(buf: ArrayBuffer): string {
	const bytes = new Uint8Array(buf);
	let binary = "";
	const chunk = 0x8000;
	for (let i = 0; i < bytes.length; i += chunk) {
		binary += String.fromCharCode.apply(
			null,
			Array.from(bytes.subarray(i, i + chunk)),
		);
	}
	return btoa(binary);
}
