import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { AnnotationPreview } from "@/bindings/schema";

/** One annotation's live editor state, sent for a targeted preview regen. */
export type LivePreviewInput = {
	id: string;
	patternId: string;
	startTime: number;
	endTime: number;
	args: Record<string, unknown>;
};

// Per-annotation coalescing: while a live preview is rendering, keep only the
// latest pending input so a fast drag doesn't queue a backlog of regenerations.
const inflight = new Set<string>();
const pending = new Map<string, LivePreviewInput>();

type AnnotationPreviewStore = {
	bitmaps: Map<string, ImageBitmap>;
	dominantColors: Map<string, [number, number, number]>;
	loading: boolean;
	generation: number;

	loadPreviews: (trackId: string, venueId: string) => Promise<void>;
	updatePreview: (
		trackId: string,
		venueId: string,
		annotation: LivePreviewInput,
	) => Promise<void>;
	invalidateAndReload: (trackId: string, venueId: string) => Promise<void>;
	clear: () => void;
};

export const useAnnotationPreviewStore = create<AnnotationPreviewStore>(
	(set, get) => ({
		bitmaps: new Map(),
		dominantColors: new Map(),
		loading: false,
		generation: 0,

		loadPreviews: async (trackId: string, venueId: string) => {
			set({ loading: true });
			try {
				const previews = await invoke<AnnotationPreview[]>(
					"generate_annotation_previews",
					{ trackId, venueId },
				);

				const newBitmaps = new Map<string, ImageBitmap>();
				const newColors = new Map<string, [number, number, number]>();

				for (const preview of previews) {
					const arr = new Uint8ClampedArray(preview.pixels);
					const imageData = new ImageData(arr, preview.width, preview.height);
					const bitmap = await createImageBitmap(imageData);
					newBitmaps.set(preview.annotationId, bitmap);
					newColors.set(preview.annotationId, preview.dominantColor);
				}

				// Dispose old bitmaps before replacing to prevent GPU memory leak
				for (const bitmap of get().bitmaps.values()) {
					bitmap.close();
				}

				set({
					bitmaps: newBitmaps,
					dominantColors: newColors,
					loading: false,
					generation: get().generation + 1,
				});
			} catch (err) {
				console.error("[annotation-previews] Failed to load:", err);
				set({ loading: false });
			}
		},

		// Regenerate ONE annotation's preview from its live args (mid-drag), with
		// coalescing so a fast drag never backs up.
		updatePreview: async (trackId, venueId, annotation) => {
			if (inflight.has(annotation.id)) {
				pending.set(annotation.id, annotation);
				return;
			}
			inflight.add(annotation.id);
			try {
				const preview = await invoke<AnnotationPreview>("preview_annotation", {
					trackId,
					venueId,
					annotation,
				});
				const arr = new Uint8ClampedArray(preview.pixels);
				const imageData = new ImageData(arr, preview.width, preview.height);
				const bitmap = await createImageBitmap(imageData);
				set((state) => {
					const newBitmaps = new Map(state.bitmaps);
					newBitmaps.get(preview.annotationId)?.close();
					newBitmaps.set(preview.annotationId, bitmap);
					const newColors = new Map(state.dominantColors);
					newColors.set(preview.annotationId, preview.dominantColor);
					return { bitmaps: newBitmaps, dominantColors: newColors };
				});
			} catch (err) {
				console.error("[annotation-previews] live update failed:", err);
			} finally {
				inflight.delete(annotation.id);
				const next = pending.get(annotation.id);
				if (next) {
					pending.delete(annotation.id);
					get().updatePreview(trackId, venueId, next);
				}
			}
		},

		invalidateAndReload: async (trackId: string, venueId: string) => {
			await invoke("invalidate_annotation_previews");
			await get().loadPreviews(trackId, venueId);
		},

		clear: () => {
			// Dispose old bitmaps
			for (const bitmap of get().bitmaps.values()) {
				bitmap.close();
			}
			set({
				bitmaps: new Map(),
				dominantColors: new Map(),
				generation: 0,
			});
		},
	}),
);
