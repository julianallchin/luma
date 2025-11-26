import * as React from "react";
import type { NodeProps } from "reactflow";
import { useHostAudioStore } from "@/features/patterns/stores/use-host-audio-store";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { BaseNode, computePlaybackState } from "./base-node";
import type { HarmonyColorVisualizerNodeData } from "./types";

// Color palette generation utilities
function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
	r /= 255;
	g /= 255;
	b /= 255;

	const max = Math.max(r, g, b);
	const min = Math.min(r, g, b);
	let h = 0;
	let s = 0;
	const l = (max + min) / 2;

	if (max !== min) {
		const d = max - min;
		s = l > 0.5 ? d / (2 - max - min) : d / (max + min);

		switch (max) {
			case r:
				h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
				break;
			case g:
				h = ((b - r) / d + 2) / 6;
				break;
			case b:
				h = ((r - g) / d + 4) / 6;
				break;
		}
	}

	return [h * 360, s, l];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
	h = h / 360;
	let r: number, g: number, b: number;

	if (s === 0) {
		r = g = b = l;
	} else {
		const hue2rgb = (p: number, q: number, t: number) => {
			if (t < 0) t += 1;
			if (t > 1) t -= 1;
			if (t < 1 / 6) return p + (q - p) * 6 * t;
			if (t < 1 / 2) return q;
			if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
			return p;
		};

		const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
		const p = 2 * l - q;
		r = hue2rgb(p, q, h + 1 / 3);
		g = hue2rgb(p, q, h);
		b = hue2rgb(p, q, h - 1 / 3);
	}

	return [Math.round(r * 255), Math.round(g * 255), Math.round(b * 255)];
}

function rgbToHex(r: number, g: number, b: number): string {
	return `#${Math.round(r).toString(16).padStart(2, "0")}${Math.round(g).toString(16).padStart(2, "0")}${Math.round(b).toString(16).padStart(2, "0")}`;
}

function hexToRgb(hex: string): [number, number, number] {
	const normalized = hex.replace("#", "");
	const value = parseInt(normalized, 16);
	return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function adjustHexLightness(hex: string, brightness: number): string {
	const [r, g, b] = hexToRgb(hex);
	const [h, s, l] = rgbToHsl(r, g, b);
	const delta = (brightness - 0.5) * 0.4;
	const newL = Math.max(0, Math.min(1, l + delta));
	const [newR, newG, newB] = hslToRgb(h, s, newL);
	return rgbToHex(newR, newG, newB);
}

function parseColorJson(colorJson: string): [number, number, number] {
	try {
		const parsed = JSON.parse(colorJson);
		if (
			typeof parsed.r === "number" &&
			typeof parsed.g === "number" &&
			typeof parsed.b === "number"
		) {
			return [parsed.r, parsed.g, parsed.b];
		}
	} catch {
		// Invalid JSON, use default
	}
	return [255, 0, 0]; // Default red
}

function generateColorPalette(baseColorJson: string, n: number): string[] {
	const [r, g, b] = parseColorJson(baseColorJson);
	const [h, s, l] = rgbToHsl(r, g, b);

	const center = (n - 1) / 2;
	const lStep = 0.1; // Lightness step
	const hStep = 6; // Hue step in degrees

	const palette: string[] = [];

	for (let i = 0; i < n; i++) {
		const offset = i - center;
		const newL = Math.max(0, Math.min(1, l + offset * lStep));
		const newH = (((h + offset * hStep) % 360) + 360) % 360;
		const [newR, newG, newB] = hslToRgb(newH, s, newL);
		palette.push(rgbToHex(newR, newG, newB));
	}

	return palette;
}

export function HarmonyColorVisualizerNode(
	props: NodeProps<HarmonyColorVisualizerNodeData>,
) {
	const { data, id } = props;
	const isLoaded = useHostAudioStore((state) => state.isLoaded);
	const currentTime = useHostAudioStore((state) => state.currentTime);
	const durationSeconds = useHostAudioStore((state) => state.durationSeconds);
	const isPlaying = useHostAudioStore((state) => state.isPlaying);
	const playback = React.useMemo(
		() =>
			computePlaybackState({
				isLoaded,
				currentTime,
				durationSeconds,
				isPlaying,
			}),
		[isLoaded, currentTime, durationSeconds, isPlaying],
	);
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);

	const paletteSize = Math.max(
		2,
		Math.round((params.palette_size as number) ?? 4),
	);

	const palette = React.useMemo(() => {
		if (!data.baseColor) return null;
		return generateColorPalette(data.baseColor, paletteSize);
	}, [data.baseColor, paletteSize]);

	const currentColor = React.useMemo(() => {
		// data.seriesData now contains a color time series with palette indices
		if (!data.seriesData?.samples.length || !palette) {
			return null;
		}

		const samples = data.seriesData.samples;
		// Use currentTime if playback is active, otherwise use 0
		const currentTime = playback.hasActive ? playback.currentTime : 0;

		// Find the two samples to interpolate between
		let beforeIdx = -1;
		let afterIdx = -1;

		for (let i = 0; i < samples.length; i++) {
			if (samples[i].time <= currentTime) {
				beforeIdx = i;
			}
			if (samples[i].time >= currentTime && afterIdx === -1) {
				afterIdx = i;
				break;
			}
		}

		const getSampleValues = (idx: number) => {
			const values = samples[idx]?.values ?? [];
			return {
				paletteValue: values[0] ?? 0,
				brightnessValue: values[1] ?? 0.5,
			};
		};

		let paletteIdx = 0;
		let brightnessValue = 0.5;

		// If before the first sample, use first sample
		if (beforeIdx === -1) {
			const { paletteValue, brightnessValue: bright } = getSampleValues(0);
			paletteIdx = Math.round(paletteValue);
			brightnessValue = bright;
		}
		// If after the last sample, use last sample
		else if (afterIdx === -1) {
			const { paletteValue, brightnessValue: bright } = getSampleValues(
				samples.length - 1,
			);
			paletteIdx = Math.round(paletteValue);
			brightnessValue = bright;
		}
		// Interpolate between samples
		else {
			const beforeSample = samples[beforeIdx];
			const afterSample = samples[afterIdx];
			const timeRange = afterSample.time - beforeSample.time;
			const t =
				timeRange > 0 ? (currentTime - beforeSample.time) / timeRange : 0;

			const beforeIdxValue = beforeSample.values[0] ?? 0;
			const afterIdxValue = afterSample.values[0] ?? 0;
			const interpolatedIdx =
				beforeIdxValue + (afterIdxValue - beforeIdxValue) * t;
			paletteIdx = Math.round(interpolatedIdx);

			const beforeBrightness = beforeSample.values[1] ?? 0.5;
			const afterBrightness = afterSample.values[1] ?? 0.5;
			brightnessValue =
				beforeBrightness + (afterBrightness - beforeBrightness) * t;
		}

		// Clamp palette index and map to color
		const clampedIdx = Math.max(0, Math.min(palette.length - 1, paletteIdx));
		const clampedBrightness = Math.max(0, Math.min(1, brightnessValue ?? 0.5));
		return adjustHexLightness(palette[clampedIdx], clampedBrightness);
	}, [data.seriesData, palette, playback.currentTime, playback.hasActive]);

	const displayColor =
		currentColor ??
		(palette && palette.length > 0
			? palette[Math.floor(palette.length / 2)]
			: "#808080");

	const body = (
		<div className="px-2 pb-2 space-y-2">
			<div
				className="w-full aspect-square rounded border-2 border-border"
				style={{
					backgroundColor: displayColor,
					minHeight: "120px",
					transition: "background-color 0.1s ease-out",
				}}
			/>
			{!data.seriesData && (
				<p className="text-[10px] text-slate-500 text-center">
					Connect harmony series input
				</p>
			)}
			{!data.baseColor && (
				<p className="text-[10px] text-slate-500 text-center">
					Connect base color input
				</p>
			)}
			{data.seriesData && data.baseColor && !playback.hasActive && (
				<p className="text-[10px] text-slate-400 text-center">
					Play audio to see color changes
				</p>
			)}
			{palette && palette.length > 0 && (
				<div className="flex gap-1">
					{palette.map((color) => (
						<div
							key={color}
							className="flex-1 h-4 rounded border border-border"
							style={{ backgroundColor: color }}
							title={color}
						/>
					))}
				</div>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}
