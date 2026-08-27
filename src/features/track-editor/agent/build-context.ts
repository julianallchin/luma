import type { BeatGrid } from "@/bindings/schema";
import { skillsListing } from "@/shared/lib/agent/skills";
// The one copy of the track copilot's prose, shared with the Rust agent loop,
// which reads the same file through `include_str!`. Judgment and invariants
// live there; the Python objects own their concrete API documentation and
// reprs, and this file adds only what the Rust loop resolves for itself.
import PROSE from "../../../../src-tauri/src/agent/prompts/track.md?raw";

/** The track agent's system prompt: the shared prose, the skills the registry
 * found, then this thread's track.
 *
 * The prompt is a cached prefix (Anthropic prompt caching), so the two stable
 * parts come first and the per-thread block last, and everything here must stay
 * byte-stable for the thread's lifetime. State that changes as the agent works
 * (clip counts, analysis results) belongs in Python, not here. */
export async function buildSystemPrompt(args: {
	trackName: string;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	venueName: string | null;
}): Promise<string> {
	const bpm = args.beatGrid?.bpm ?? null;
	const bars = args.beatGrid?.downbeats.length ?? null;
	const track = `## Current track
- Name: ${args.trackName || "<untitled>"}
- Duration: ${formatTime(args.durationSeconds)}
- BPM: ${bpm !== null ? bpm.toFixed(1) : "unknown"}
- Bars: ${bars ?? "unknown"}
- Venue: ${args.venueName ?? "<unknown>"}`;

	return [PROSE.trimEnd(), await skillsListing(), track]
		.filter(Boolean)
		.join("\n\n");
}

function formatTime(seconds: number): string {
	if (!Number.isFinite(seconds)) return "?";
	const minutes = Math.floor(seconds / 60);
	const remainder = seconds - minutes * 60;
	return `${minutes}:${remainder.toFixed(2).padStart(5, "0")}`;
}
