import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { AnnotationPreview } from "@/bindings/schema";

type AnnotationPreviewStore = {
	bitmaps: Map<number, ImageBitmap>;
	dominantColors: Map<number, [number, number, number]>;
	loading: boolean;
	generation: number;

	loadPreviews: (trackId: number, venueId: number) => Promise<void>;
	invalidateAndReload: (trackId: number, venueId: number) => Promise<void>;
	clear: () => void;
};

export const useAnnotationPreviewStore = create<AnnotationPreviewStore>(
	(set, get) => ({
		bitmaps: new Map(),
		dominantColors: new Map(),
		loading: false,
		generation: 0,

		loadPreviews: async (trackId: number, venueId: number) => {
			set({ loading: true });
			try {
				const previews = await invoke<AnnotationPreview[]>(
					"generate_annotation_previews",
					{ trackId, venueId },
				);

				const newBitmaps = new Map<number, ImageBitmap>();
				const newColors = new Map<number, [number, number, number]>();

				for (const preview of previews) {
					const arr = new Uint8ClampedArray(preview.pixels);
					const imageData = new ImageData(arr, preview.width, preview.height);
					const bitmap = await createImageBitmap(imageData);
					newBitmaps.set(preview.annotationId, bitmap);
					newColors.set(preview.annotationId, preview.dominantColor);
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

		invalidateAndReload: async (trackId: number, venueId: number) => {
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
