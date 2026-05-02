import type { BeatGrid, BlendMode } from "@/bindings/schema";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";

const DEFAULT_THRESHOLD = 0.5;

export type BarClassification = {
	bar_idx: number;
	start: number;
	end: number;
	predictions: Record<string, number>;
};

export type BarClassificationsPayload = {
	classifications: BarClassification[];
	tagOrder: string[];
};

/**
 * Convert a bar-classifications payload into a compact bar-by-bar tag list,
 * keeping only tags above their per-tag suggestion threshold (model-tuned;
 * falls back to 0.5 for any tag missing from the threshold map).
 * `intensity` is rendered separately since it's a continuous (clipped 0..5)
 * value, not a sigmoid prob.
 */
export function formatBarTags(
	payload: BarClassificationsPayload | null,
	thresholds: Record<string, number> = {},
): string {
	if (!payload || payload.classifications.length === 0) {
		return "<no bar classifications available>";
	}

	const lines: string[] = [];
	for (const bar of payload.classifications) {
		const start = formatTime(bar.start);
		const end = formatTime(bar.end);

		const predictions = bar.predictions ?? {};
		const intensity = predictions.intensity;
		const intensityStr =
			typeof intensity === "number" ? `intensity=${intensity.toFixed(2)}` : "";

		const tags = Object.entries(predictions)
			.filter(
				([k, v]) =>
					k !== "intensity" && v >= (thresholds[k] ?? DEFAULT_THRESHOLD),
			)
			.sort((a, b) => b[1] - a[1])
			.map(([k, v]) => `${k}(${v.toFixed(2)})`);

		const tagsStr = tags.length > 0 ? tags.join(" ") : "—";
		const intensityPrefix = intensityStr ? `${intensityStr}  ` : "";
		lines.push(
			`Bar ${bar.bar_idx + 1}  ${start}-${end}  ${intensityPrefix}${tagsStr}`,
		);
	}
	return lines.join("\n");
}

/** Format the current annotations as a readable, line-per-annotation listing. */
export function formatScore(
	annotations: TimelineAnnotation[],
	beatGrid: BeatGrid | null,
): string {
	if (annotations.length === 0) return "<empty score>";

	const sorted = [...annotations].sort((a, b) => {
		if (a.startTime !== b.startTime) return a.startTime - b.startTime;
		return b.zIndex - a.zIndex;
	});

	const idWidth = Math.min(
		20,
		sorted.reduce((m, a) => Math.max(m, a.id.length), 0),
	);
	const nameWidth = Math.min(
		28,
		sorted.reduce(
			(m, a) => Math.max(m, (a.patternName ?? a.patternId).length),
			0,
		),
	);

	const lines = sorted.map((a) => {
		const id = a.id.padEnd(idWidth);
		const name = (a.patternName ?? a.patternId).padEnd(nameWidth);
		const start = formatTime(a.startTime);
		const end = formatTime(a.endTime);
		const startBar = beatToBar(a.startTime, beatGrid);
		const endBar = beatToBar(a.endTime, beatGrid);
		const barRange =
			startBar !== null && endBar !== null
				? ` bar ${startBar.toFixed(2)}-${endBar.toFixed(2)}`
				: "";
		const blend: BlendMode = a.blendMode ?? "replace";
		return `  ${id}  ${name}  z=${a.zIndex}  ${start}-${end}${barRange}  blend=${blend}`;
	});

	return lines.join("\n");
}

/** Build the system prompt header that frames the agent's role. */
export function buildSystemPrompt(args: {
	trackName: string;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	patternsCount: number;
	venueName: string | null;
	annotationsCount: number;
}): string {
	const { trackName, durationSeconds, beatGrid, patternsCount, venueName } =
		args;
	const bpm = beatGrid?.bpm ?? null;
	const bars = beatGrid?.downbeats.length ?? null;
	return `You are a lighting design copilot embedded in the Luma track editor. You help the user reason about a track's musical structure and place lighting annotations on its timeline.

## Track
- Name: ${trackName || "<untitled>"}
- Duration: ${formatTime(durationSeconds)}
- BPM: ${bpm !== null ? bpm.toFixed(1) : "unknown"}
- Bars: ${bars ?? "unknown"}
- Venue: ${venueName ?? "<unknown>"}
- Patterns available: ${patternsCount}
- Existing annotations: ${args.annotationsCount}

## Annotations
An "annotation" is a placement of a pattern on the track timeline. It has:
  id, patternId, startTime (sec), endTime (sec), zIndex (higher = on top), blendMode, args.
Use the place_annotation, update_annotation, delete_annotation tools to edit the score.
Only modify the score when the user asks you to. When proposing a sweeping change, consider asking first.

## Patterns
Use search_patterns to find candidate patterns by name/category, then read_pattern to inspect the pattern's node graph (text form) and its args.

## Bar tag schema
Each bar carries one ordinal intensity plus tags from six multi-label heads. Tag values are independent sigmoid probabilities; only tags above their per-tag threshold are shown. An empty tag list is valid (e.g. silent / breakdown bars).

Intensity is an ABSOLUTE, genre-anchored 0–5 scale — not the track's local max. A chill track may legitimately top out at 3. Anchors:
  0 silent — true silence, dead air, the cut before a drop
  1 atmospheric — pads / textures / ambient, no rhythm
  2 mid groove — standard verse, head-nod tier, no climax
  3 hard buildup / drop fill — active riser, full-band verse, sustained 4/4 (solo piano caps here)
  4 drop — full-energy main moment, festival main stage, full orchestra fortissimo
  5 absurd — acoustically unhinged peak: hardstyle, peak Excision dubstep, death-metal blasts. Rare; most tracks never reach this.

Heads and their options:
  drums:    hats, kick, snare, perc, fill, impact
  rhythm:   four_four, halftime, breakbeat
  bass:     pluck, sustain
  synths:   arp, pad, lead, riser
  acoustic: piano, acoustic_guitar, electric_guitar, other
  vocals:   vocal_lead, vocal_chop

Use intensity to pick how hard a pattern should hit; use heads to pick what to react to (e.g. impact → strobe hit, riser → buildup sweep, halftime → slower movement).

## Style
Be concise. Reference bars and timestamps. When you take an action, briefly state what you did.
Do not use code blocks, fenced code, or inline backticks in your replies — write everything as plain prose.`;
}

function formatTime(seconds: number): string {
	if (!Number.isFinite(seconds)) return "?";
	const m = Math.floor(seconds / 60);
	const s = seconds - m * 60;
	return `${m}:${s.toFixed(2).padStart(5, "0")}`;
}

function beatToBar(timeSec: number, beatGrid: BeatGrid | null): number | null {
	if (!beatGrid || beatGrid.downbeats.length === 0) return null;
	const downbeats = beatGrid.downbeats;
	let i = 0;
	while (i + 1 < downbeats.length && downbeats[i + 1] <= timeSec) i++;
	if (i + 1 >= downbeats.length) return i + 1;
	const span = downbeats[i + 1] - downbeats[i];
	if (span <= 0) return i + 1;
	const frac = (timeSec - downbeats[i]) / span;
	return i + 1 + frac;
}
