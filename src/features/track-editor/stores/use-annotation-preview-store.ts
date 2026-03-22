import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { AnnotationPreview } from "@/bindings/schema";

type AnnotationPreviewStore = {
	bitmaps: Map<string, ImageBitmap>;
	dominantColors: Map<string, [number, number, number]>;
	loading: boolean;
	generation: number;

	loadPreviews: (trackId: string, venueId: string) => Promise<void>;
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
