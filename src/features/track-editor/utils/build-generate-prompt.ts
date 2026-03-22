import type { PatternArgDef, PatternSummary } from "@/bindings/schema";

/**
 * Build a system prompt for the LLM that describes the full DSL spec,
 * available patterns + args, tags, blend modes, and instructions.
 */
export function buildGeneratePrompt(
	patterns: PatternSummary[],
	patternArgs: Record<string, PatternArgDef[]>,
	totalBars: number,
	downbeats?: number[],
	groupNames?: string[],
): string {
	const sections: string[] = [];

	// ── DSL Syntax ───────────────────────────────────────────────
	sections.push(`You are a lighting designer. Given an audio track, produce a complete lighting score in Luma DSL format.

## DSL Syntax

A DSL score is a list of **annotations** grouped into **layers**. Each annotation is one line that applies a pattern to fixtures over a bar range.

### Annotation format
\`pattern_name(selection) @start-end arg1=value1 arg2=value2 blend=mode\`

- **pattern_name** — one of the available patterns listed below
- **selection** — a group expression in parentheses selecting which fixtures to target
- **@start-end** — bar range (half-open: start is inclusive, end is exclusive). \`@5\` is shorthand for \`@5-6\` (one bar). Sub-bar precision uses colon notation: \`@5:3\` means bar 5 beat 3, \`@5:3:2\` means bar 5 beat 3 subdivision 2. Beats and subdivisions are 1-indexed.
- **args** — key=value pairs (optional, defaults used if omitted)
- **blend** — blend mode (optional, defaults to replace)

### Layers
Annotations are grouped into layers separated by **blank lines**. The first group is layer 0 (bottom/lowest priority). Each subsequent group paints on top. Within a layer, annotations are listed in time order and should not overlap.

Think of it like painting: lay down the base wash first (layer 0), then add rhythmic hits on top (layer 1), then accents and strobes (layer 2).

### Colors
Colors are hex format: \`#rrggbb\` (e.g. \`#ff0000\` for red, \`#0000ff\` for blue).

### Numbers
Scalar values are plain numbers: \`0.5\`, \`1.0\`, \`0\`.

### Comments
Lines starting with \`#\` are comments and are ignored.`);

	// ── Group Selection ─────────────────────────────────────────
	const groupList =
		groupNames && groupNames.length > 0
			? groupNames.map((n) => `\`${n}\``).join(", ")
			: "_No groups defined yet_";
	sections.push(`## Group Selection

Selection expressions select which fixtures an annotation targets. The special name \`all\` matches everything.

### Available groups
${groupList}

### Operators (in precedence order, lowest first)
- \`>\` — fallback: try left, fall back to right if no fixtures match
- \`|\` — union (OR)
- \`^\` — exclusive or (XOR)
- \`&\` — intersection (AND)
- \`~\` — complement (NOT), prefix operator
- \`()\` — grouping

Examples: \`all\`, \`front_wash & left_movers\`, \`drum_uplighters | dj_booth\`, \`~strobes\`, \`front_movers > back_wash\``);

	// ── Blend Modes ──────────────────────────────────────────────
	sections.push(`## Blend Modes

Blend modes control how layers combine. Specify with \`blend=mode\` at end of an annotation line.

Available modes: \`replace\` (default), \`add\`, \`multiply\`, \`screen\`, \`max\`, \`min\`, \`lighten\`, \`value\`

If omitted, \`replace\` is used. Use \`add\` for additive layering (good for building up light), \`multiply\` for masking, \`screen\` for soft blending.`);

	// ── Available Patterns ───────────────────────────────────────
	const patternLines: string[] = [];
	for (const p of patterns) {
		const args = patternArgs[p.id] ?? [];
		const nonSelectionArgs = args.filter((a) => a.argType !== "Selection");

		let entry = `### \`${p.name}\``;
		if (p.description) {
			entry += `\n${p.description}`;
		}
		if (nonSelectionArgs.length === 0) {
			entry += "\nNo configurable args.";
		} else {
			const argParts = nonSelectionArgs.map((a) => {
				const dflt = formatDefaultValue(a.argType, a.defaultValue);
				return `  - \`${a.name}\`: ${a.argType.toLowerCase()}${dflt !== null ? ` (default: ${dflt})` : ""}`;
			});
			entry += `\n**Args:**\n${argParts.join("\n")}`;
		}
		patternLines.push(entry);
	}
	sections.push(`## Available Patterns\n\n${patternLines.join("\n\n")}`);

	// ── Examples ─────────────────────────────────────────────────
	sections.push(`## Example

\`\`\`
# Layer 0 — base wash for the whole track
solid_color(all) @1-17 color=#1a0033
solid_color(all) @17-33 color=#000044

# Layer 1 — rhythmic elements on specific sections
intensity_spikes(hit) @5-9 subdivision=2 blend=add
bass_strobe(hit) @9-17 rate=0.9 blend=add

# Layer 2 — accent details
random_dimmer_mask(accent) @9-17 subdivision=2 count=3 color=#ff4400 blend=add
\`\`\`

Layer 0 paints the base color across the whole track (dark purple bars 1–16, then dark blue bars 17–32). Layer 1 adds rhythmic intensity spikes on bars 5–8 and bass strobes on bars 9–16. Layer 2 adds random accent flashes over the drop.`);

	// ── Bar↔Timestamp Cheatsheet ────────────────────────────────
	if (downbeats && downbeats.length > 0) {
		const lines = downbeats.map((t, i) => {
			const m = Math.floor(t / 60);
			const s = t % 60;
			return `Bar ${i + 1} - ${m}:${s.toFixed(2).padStart(5, "0")}`;
		});
		sections.push(
			`## Bar↔Timestamp Cheatsheet\n\nUse this to orient yourself in the audio.\n\n${lines.join("\n")}`,
		);
	}

	// ── Instructions ─────────────────────────────────────────────
	sections.push(`## Instructions

- The track has ${totalBars} bars total. Bar ranges use half-open notation: \`@1-${totalBars + 1}\` covers the entire track.
- Listen to the audio carefully. Match the energy, structure, and mood of the music.
- Use contrasting sections for verses, choruses, bridges, builds, and drops.
- Think in layers: paint broad base washes first (layer 0), then add rhythmic patterns (layer 1), then accents on top (layer 2+).
- Within each layer, annotations should not overlap in time.
- Use the full range of available patterns, not just solid_color.
- Use blend modes (especially \`add\` and \`screen\`) to layer effects.
- Output ONLY the DSL text. No markdown fences, no explanation, no commentary.`);

	return sections.join("\n\n");
}

function formatDefaultValue(
	argType: string,
	defaultValue: Record<string, unknown>,
): string | null {
	if (defaultValue == null) return null;

	if (argType === "Color") {
		if (
			typeof defaultValue === "object" &&
			"r" in defaultValue &&
			"g" in defaultValue &&
			"b" in defaultValue
		) {
			const r = Math.round(Number(defaultValue.r))
				.toString(16)
				.padStart(2, "0");
			const g = Math.round(Number(defaultValue.g))
				.toString(16)
				.padStart(2, "0");
			const b = Math.round(Number(defaultValue.b))
				.toString(16)
				.padStart(2, "0");
			return `#${r}${g}${b}`;
		}
		if (typeof defaultValue === "string") return defaultValue;
		return null;
	}

	if (argType === "Scalar") {
		if (typeof defaultValue === "number") return String(defaultValue);
		return null;
	}

	return null;
}
