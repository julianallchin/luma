import type { PatternArgDef, PatternSummary } from "@/bindings/schema";

/**
 * Build a system prompt for the LLM that describes the full DSL spec,
 * available patterns + args, tags, blend modes, and instructions.
 */
export function buildGeneratePrompt(
	patterns: PatternSummary[],
	patternArgs: Record<number, PatternArgDef[]>,
	totalBars: number,
): string {
	const sections: string[] = [];

	// ── DSL Syntax ───────────────────────────────────────────────
	sections.push(`You are a lighting designer. Given an audio track, produce a complete lighting score in Luma DSL format.

## DSL Syntax

A DSL score is a sequence of **bar blocks**. Each block starts with a bar header and contains one or more layers.

### Bar headers
- \`@N\` — single bar N
- \`@N-M\` — bar range N through M (inclusive)

### Layers
Each line after the bar header is a layer. Layers stack bottom-to-top (first layer is lowest priority).

Format: \`pattern_name(selection) arg1=value1 arg2=value2 blend=mode\`

- **pattern_name** — one of the available patterns listed below
- **selection** — a tag expression in parentheses selecting which fixtures to target
- **args** — key=value pairs (optional, defaults used if omitted)
- **blend** — blend mode (optional, defaults to replace)

### hold
A special layer \`hold\` repeats the previous block's layers for the current bar range. Use it to extend a section without repeating all layers.

### Colors
Colors are hex format: \`#rrggbb\` (e.g. \`#ff0000\` for red, \`#0000ff\` for blue).

### Numbers
Scalar values are plain numbers: \`0.5\`, \`1.0\`, \`0\`.`);

	// ── Tag Expressions ──────────────────────────────────────────
	sections.push(`## Tag Expressions (Selection)

Tag expressions select which fixtures a layer targets. The special tag \`all\` matches everything.

### Predefined tags
- Spatial: \`left\`, \`right\`, \`high\`, \`low\`, \`circular\`
- Purpose: \`hit\`, \`wash\`, \`accent\`, \`chase\`

### Operators (in precedence order, lowest first)
- \`>\` — fallback: try left, fall back to right if no fixtures match
- \`|\` — union (OR)
- \`^\` — exclusive or (XOR)
- \`&\` — intersection (AND)
- \`~\` — complement (NOT), prefix operator
- \`()\` — grouping

Examples: \`all\`, \`left & wash\`, \`hit | accent\`, \`~high\`, \`left > wash\``);

	// ── Blend Modes ──────────────────────────────────────────────
	sections.push(`## Blend Modes

Blend modes control how layers combine. Specify with \`blend=mode\` at end of a layer line.

Available modes: \`replace\` (default), \`add\`, \`multiply\`, \`screen\`, \`max\`, \`min\`, \`lighten\`, \`value\`

If omitted, \`replace\` is used. Use \`add\` for additive layering (good for building up light), \`multiply\` for masking, \`screen\` for soft blending.`);

	// ── Available Patterns ───────────────────────────────────────
	const patternLines: string[] = [];
	for (const p of patterns) {
		const args = patternArgs[p.id] ?? [];
		const nonSelectionArgs = args.filter((a) => a.argType !== "Selection");

		if (nonSelectionArgs.length === 0) {
			patternLines.push(`- \`${p.name}\` — no args`);
		} else {
			const argDescs = nonSelectionArgs.map((a) => {
				const dflt = formatDefaultValue(a.argType, a.defaultValue);
				return `\`${a.name}\`: ${a.argType.toLowerCase()}${dflt !== null ? ` (default: ${dflt})` : ""}`;
			});
			patternLines.push(`- \`${p.name}\` — ${argDescs.join(", ")}`);
		}
	}
	sections.push(`## Available Patterns\n\n${patternLines.join("\n")}`);

	// ── Examples ─────────────────────────────────────────────────
	sections.push(`## Examples

\`\`\`
@1-4
solid_color(all) color=#1a0033

@5-8
solid_color(wash) color=#0000ff
strobe(hit) speed=0.8 blend=add

@9-16
hold
\`\`\`

This creates a dim purple wash for bars 1–4, then adds blue wash with strobe on hit fixtures for bars 5–8, and holds that look through bar 16.`);

	// ── Instructions ─────────────────────────────────────────────
	sections.push(`## Instructions

- The track has ${totalBars} bars total. Cover bars 1 through ${totalBars}.
- Listen to the audio carefully. Match the energy, structure, and mood of the music.
- Use contrasting sections for verses, choruses, bridges, builds, and drops.
- Layer patterns with different selections for visual depth (e.g. wash + hit + accent).
- Use \`hold\` to sustain sections without repeating layers.
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
