import { create } from "zustand";
import { persist } from "zustand/middleware";

export interface RenderSettings {
	/** Dark stage mode — black background, no ambient, only fixture lights */
	darkStage: boolean;
	/** Volumetric haze enabled */
	volumetricHaze: boolean;
	/** Equiangular samples per beam (shader cap 32) */
	hazeSteps: number;
	/** Haze render-target scale (0.5–1). 1 keeps beam edges razor-crisp. */
	hazeResolution: number;
	/** Haze density (0-1) */
	hazeDensity: number;
	/** Spatial denoise (Gaussian blur) on the haze buffer */
	hazeDenoise: boolean;
	/** Scene SpotLights from fixtures (cast light on geometry) */
	fixtureSpotlights: boolean;
	/** Bloom post-process */
	bloom: boolean;
	/** Max device pixel ratio (1-2). Lower = less GPU work on Retina displays. */
	maxDpr: number;
	/** Camera field of view in degrees (20-120). */
	fov: number;
}

interface RenderSettingsStore extends RenderSettings {
	set: (partial: Partial<RenderSettings>) => void;
}

export const useRenderSettingsStore = create<RenderSettingsStore>()(
	persist(
		(set) => ({
			darkStage: true,
			volumetricHaze: true,
			hazeSteps: 8,
			hazeResolution: 1,
			hazeDensity: 0.8,
			hazeDenoise: true,
			fixtureSpotlights: true,
			bloom: false,
			maxDpr: 1.5,
			fov: 50,
			set: (partial) => set(partial),
		}),
		{
			name: "luma-render-settings",
			version: 1,
			// v0 hazeSteps were uniform march steps (default 24); they are now
			// equiangular samples per beam, where 8 ≈ the old 24-step look.
			migrate: (persisted) => ({
				...(persisted as RenderSettings),
				hazeSteps: 8,
				hazeResolution: 1,
			}),
		},
	),
);
