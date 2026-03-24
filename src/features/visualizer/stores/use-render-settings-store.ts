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
}

interface RenderSettingsStore extends RenderSettings {
	set: (partial: Partial<RenderSettings>) => void;
}

export const useRenderSettingsStore = create<RenderSettingsStore>()(
	persist(
		(set) => ({
			darkStage: true,
			volumetricHaze: true,
			hazeSteps: 6,
			hazeDensity: 0.5,
			fixtureSpotlights: true,
			spotlightCount: 6,
			shadows: true,
			bloom: false,
			set: (partial) => set(partial),
		}),
		{ name: "luma-render-settings" },
	),
);
