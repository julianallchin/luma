import type { PatternDef, PatternRegistry } from "../types";

export const PATTERNS: PatternDef[] = [
	{
		name: "solid_color",
		args: [
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
		],
	},
	{
		name: "gradient",
		args: [
			{
				id: "start",
				name: "start",
				argType: "Color",
				defaultValue: "#000000",
			},
			{
				id: "end",
				name: "end",
				argType: "Color",
				defaultValue: "#ffffff",
			},
		],
	},
	{
		name: "intensity_spikes",
		args: [
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
			{
				id: "max_dimmer",
				name: "max_dimmer",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
		],
	},
	{
		name: "major_axis_chase",
		args: [
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
		],
	},
	{
		name: "major_axis_bounce_chase",
		args: [
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
		],
	},
	{
		name: "y_chase",
		args: [
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
		],
	},
	{
		name: "circle_pill",
		args: [
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
		],
	},
	{
		name: "symmetrical_circle_wipe",
		args: [
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
		],
	},
	{
		name: "two_color_alternating_wash",
		args: [
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "color_1",
				name: "color_1",
				argType: "Color",
				defaultValue: "#ff0000",
			},
			{
				id: "color_2",
				name: "color_2",
				argType: "Color",
				defaultValue: "#0000ff",
			},
		],
	},
	{
		name: "bass_strobe",
		args: [
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
			{
				id: "rate",
				name: "rate",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
		],
	},
	{
		name: "random_dimmer_mask",
		args: [
			{
				id: "subdivision",
				name: "subdivision",
				argType: "Scalar",
				defaultValue: 1,
			},
			{
				id: "count",
				name: "count",
				argType: "Scalar",
				defaultValue: 3,
			},
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
			{
				id: "color",
				name: "color",
				argType: "Color",
				defaultValue: "#ffffff",
			},
		],
	},
	{
		name: "linear_dimmer_fade",
		args: [
			{
				id: "start_value",
				name: "start_value",
				argType: "Scalar",
				defaultValue: 0,
			},
			{
				id: "end_value",
				name: "end_value",
				argType: "Scalar",
				defaultValue: 1,
			},
		],
	},
	{
		name: "chord_color",
		args: [
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
		],
	},
	{
		name: "kick_intensity",
		args: [
			{
				id: "selection",
				name: "selection",
				argType: "Selection",
				defaultValue: null,
			},
		],
	},
	{
		name: "smooth_dimmer_noise",
		args: [],
	},
	{
		name: "smooth_front_back_pulse",
		args: [],
	},
];

export function createTestRegistry(): PatternRegistry {
	return new Map(PATTERNS.map((p) => [p.name, p]));
}
