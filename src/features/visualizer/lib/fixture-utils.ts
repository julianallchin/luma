import type { FixtureDefinition } from "../../../bindings/fixtures";

export interface FixtureState {
	color: { r: number; g: number; b: number };
	intensity: number;
	strobe: number; // Hz, 0 = open
	zoom: number; // 0-1 or degrees
	pan: number; // degrees
	tilt: number; // degrees
}

// Cache mapping of HeadIndex -> Channel Indices
export interface DmxMapping {
	red: number | null;
	green: number | null;
	blue: number | null;
	white: number | null;
	amber: number | null;
	cyan: number | null;
	magenta: number | null;
	yellow: number | null;
	dimmer: number | null;
	masterDimmer: number | null;
	strobe: number | null;
	pan: number | null;
	tilt: number | null;
	zoom: number | null;
}

/**
 * Pre-calculates the DMX channel offsets for a specific head.
 * This should be called ONCE when the fixture loads, not every frame.
 */
export function getDmxMapping(
	definition: FixtureDefinition,
	modeName: string,
	headIndex: number,
): DmxMapping {
	const activeMode = definition.Mode.find((m) => m["@Name"] === modeName);
	const globalChannelList = definition.Channel;
	const modeChannelList = activeMode?.Channel || [];

	// 1. Identify Channel Indices available in this Mode
	// Map ModeIndex -> GlobalDefinition
	const modeChannels = modeChannelList.map((mc) => {
		// biome-ignore lint/complexity/useLiteralKeys: $value is not a valid JS identifier
		return globalChannelList.find((gc) => gc["@Name"] === mc["$value"]);
	});

	// 2. Identify which channels belong to this Head
	// If mode has heads, restrict search to head channels.
	// If mode has NO heads, all channels apply to "Head 0".
	let headChannelIndices: number[] = [];

	if (activeMode?.Head && activeMode.Head.length > 0) {
		if (headIndex < activeMode.Head.length) {
			headChannelIndices = activeMode.Head[headIndex].Channel;
		}
	} else {
		// Single head mode - use all indices
		headChannelIndices = modeChannelList.map((_, i) => i);
	}

	const mapping: DmxMapping = {
		red: null,
		green: null,
		blue: null,
		white: null,
		amber: null,
		cyan: null,
		magenta: null,
		yellow: null,
		dimmer: null,
		masterDimmer: null,
		strobe: null,
		pan: null,
		tilt: null,
		zoom: null,
	};

	// 3. Search for Master Dimmer (Global)
	// Master dimmer is usually NOT in the Head definition.
	for (let i = 0; i < modeChannels.length; i++) {
		const ch = modeChannels[i];
		if (!ch) continue;

		const preset = ch["@Preset"];
		// Explicit Master
		if (preset === "IntensityMasterDimmer") {
			mapping.masterDimmer = i;
		}
		// Implicit Master (named Dimmer, not assigned to any head)
		else if (
			!mapping.masterDimmer &&
			(preset === "IntensityDimmer" ||
				ch["@Name"].toLowerCase().includes("dimmer"))
		) {
			// Verify it's not used in ANY head
			let usedInHead = false;
			if (activeMode?.Head) {
				for (const h of activeMode.Head) {
					if (h.Channel.includes(i)) {
						usedInHead = true;
						break;
					}
				}
			}
			if (!usedInHead) {
				mapping.masterDimmer = i;
			}
		}

		// Global Controls (Strobe/Pan/Tilt/Zoom often global)
		// TODO: Handle per-head Pan/Tilt (e.g. multiple moving heads in one bar)
		if (preset?.includes("Strobe") && mapping.strobe === null)
			mapping.strobe = i;
		if (preset?.includes("Pan") && mapping.pan === null) mapping.pan = i;
		if (preset?.includes("Tilt") && mapping.tilt === null) mapping.tilt = i;
		if (preset?.includes("Zoom") && mapping.zoom === null) mapping.zoom = i;
	}

	// 4. Search for Head Channels
	for (const idx of headChannelIndices) {
		const ch = modeChannels[idx];
		if (!ch) continue;

		const preset = ch["@Preset"];
		const name = ch["@Name"].toLowerCase();
		const group = ch.Group;

		// Color Mixing
		if (preset === "IntensityRed" || name.includes("red")) mapping.red = idx;
		else if (preset === "IntensityGreen" || name.includes("green"))
			mapping.green = idx;
		else if (preset === "IntensityBlue" || name.includes("blue"))
			mapping.blue = idx;
		else if (preset === "IntensityWhite" || name.includes("white"))
			mapping.white = idx;
		else if (preset === "IntensityAmber" || name.includes("amber"))
			mapping.amber = idx;
		else if (preset === "IntensityCyan" || name.includes("cyan"))
			mapping.cyan = idx;
		else if (preset === "IntensityMagenta" || name.includes("magenta"))
			mapping.magenta = idx;
		else if (preset === "IntensityYellow" || name.includes("yellow"))
			mapping.yellow = idx;
		// Local Dimmer
		else if (
			preset === "IntensityDimmer" ||
			// biome-ignore lint/complexity/useLiteralKeys: $value is not a valid JS identifier
			(group?.["$value"] === "Intensity" && name.includes("dimmer"))
		) {
			mapping.dimmer = idx;
		}
	}

	return mapping;
}

/**
 * Reads DMX values and calculates the final visual state for a head.
 * Call this every frame.
 */
export function getHeadState(
	mapping: DmxMapping,
	universeData: Uint8Array,
	startAddress: number, // 0-based absolute address
): FixtureState {
	const getVal = (offset: number | null) =>
		offset !== null && startAddress + offset < universeData.length
			? universeData[startAddress + offset]
			: 0;

	// 1. Dimmer
	const masterVal =
		mapping.masterDimmer !== null ? getVal(mapping.masterDimmer) : 255;
	const localVal = mapping.dimmer !== null ? getVal(mapping.dimmer) : 255;
	const intensity = (localVal / 255.0) * (masterVal / 255.0);

	// 2. Color Mixing
	let r = 0,
		g = 0,
		b = 0;

	// CMY (Subtractive) -> RGB
	if (
		mapping.cyan !== null ||
		mapping.magenta !== null ||
		mapping.yellow !== null
	) {
		const c = getVal(mapping.cyan);
		const m = getVal(mapping.magenta);
		const y = getVal(mapping.yellow);
		r = (255 - c) / 255.0;
		g = (255 - m) / 255.0;
		b = (255 - y) / 255.0;
	}
	// RGB (Additive)
	else {
		r = getVal(mapping.red) / 255.0;
		g = getVal(mapping.green) / 255.0;
		b = getVal(mapping.blue) / 255.0;
	}

	// White/Amber Addition (Simplified: desaturate/tint)
	if (mapping.white !== null) {
		const w = getVal(mapping.white) / 255.0;
		r = Math.min(1, r + w);
		g = Math.min(1, g + w);
		b = Math.min(1, b + w);
	}
	if (mapping.amber !== null) {
		const a = getVal(mapping.amber) / 255.0;
		r = Math.min(1, r + a);
		g = Math.min(1, g + a * 0.75); // Amber is reddish-yellow
		b = Math.min(1, b);
	}

	return {
		color: { r, g, b },
		intensity,
		strobe: mapping.strobe !== null ? getVal(mapping.strobe) : 0, // TODO: Parse strobe Hz
		zoom: mapping.zoom !== null ? getVal(mapping.zoom) : 0,
		pan: mapping.pan !== null ? getVal(mapping.pan) : 0,
		tilt: mapping.tilt !== null ? getVal(mapping.tilt) : 0,
	};
}
