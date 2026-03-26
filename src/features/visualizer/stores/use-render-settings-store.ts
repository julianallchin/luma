import { create } from "zustand";
import { persist } from "zustand/middleware";

export interface RenderSettings {
	/** Dark stage mode — black background, no ambient, only fixture lights */
	darkStage: boolean;
	/** Volumetric haze enabled */
	volumetricHaze: boolean;
	/** Raymarch step count (4-24) */
	hazeSteps: number;
	/** Haze density (0-1) */
	hazeDensity: number;
	/** Scene SpotLights from fixtures (cast light on geometry) */
	fixtureSpotlights: boolean;
	/** Number of pooled spotlights (1-8) */
	spotlightCount: number;
	/** Shadows from spotlights */
	shadows: boolean;
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
			hazeSteps: 4,
			hazeDensity: 0.8,
			fixtureSpotlights: true,
			spotlightCount: 8,
			shadows: true,
			bloom: false,
			maxDpr: 1.5,
			fov: 50,
			set: (partial) => set(partial),
		}),
		{ name: "luma-render-settings" },
	),
);
