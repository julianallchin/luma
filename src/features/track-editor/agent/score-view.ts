import type { BeatGrid, BlendMode } from "@/bindings/schema";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";

/** Convert a real time (seconds) to a fractional bar number (1-indexed). */
export function timeToBar(time: number, beatGrid: BeatGrid | null): number {
	if (!beatGrid || beatGrid.downbeats.length === 0) return 0;
	const db = beatGrid.downbeats;
	if (time <= db[0]) {
		// Before first downbeat: extrapolate using first span if available.
		if (db.length >= 2) {
			const span = db[1] - db[0];
			return span > 0 ? 1 + (time - db[0]) / span : 1;
		}
		return 1;
	}
	for (let i = 0; i + 1 < db.length; i++) {
		if (time >= db[i] && time < db[i + 1]) {
			const span = db[i + 1] - db[i];
			return span > 0 ? i + 1 + (time - db[i]) / span : i + 1;
		}
	}
	// After last downbeat: extrapolate using last span.
	const last = db.length - 1;
	if (last >= 1) {
		const span = db[last] - db[last - 1];
		return span > 0 ? last + 1 + (time - db[last]) / span : last + 1;
	}
	return last + 1;
}

/** Convert a fractional bar number (1-indexed) to a real time (seconds). */
export function barToTime(bar: number, beatGrid: BeatGrid | null): number {
	if (!beatGrid || beatGrid.downbeats.length === 0) return 0;
	const db = beatGrid.downbeats;
	const idx = Math.floor(bar - 1);
	const frac = bar - 1 - idx;
	if (idx < 0) {
		if (db.length >= 2) return db[0] + (idx + frac) * (db[1] - db[0]);
		return db[0];
	}
	if (idx + 1 < db.length) {
		return db[idx] + frac * (db[idx + 1] - db[idx]);
	}
	// Past last downbeat: extrapolate.
	const last = db.length - 1;
	if (last >= 1) {
		const span = db[last] - db[last - 1];
		return db[last] + (idx - last + frac) * span;
	}
	return db[last];
}

/** Format a bar number compactly: integer when bar-aligned, else 2 decimals. */
export function formatBar(bar: number): string {
	if (!Number.isFinite(bar)) return "?";
	const rounded = Math.round(bar);
	if (Math.abs(bar - rounded) < 0.005) return String(rounded);
	return bar.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}

/** Format a bar range using `end` token when the clip extends to track end. */
export function formatBarRange(
	startBar: number,
	endBar: number,
	totalBars: number | null,
): string {
	const s = formatBar(startBar);
	const e =
		totalBars !== null && Math.abs(endBar - totalBars) < 0.05
			? "end"
			: formatBar(endBar);
	return `${s}–${e}`;
}

/** Stable, compact composition key for a clip — pattern + blend + z. */
function compKey(c: TimelineAnnotation): string {
	const blend: BlendMode = c.blendMode ?? "replace";
	return `${c.zIndex}|${c.patternId}|${blend}`;
}

export type Region = {
	startTime: number;
	endTime: number;
	clips: TimelineAnnotation[]; // sorted by zIndex desc (top first)
};

/**
 * Partition the timeline into maximal regions where the active clip set is
 * constant (or has constant composition, depending on `mergeBy`).
 * Regions are returned in time order; gaps (no active clips) are emitted as
 * empty regions only when `includeGaps` is true.
 */
export function partitionRegions(
	annotations: TimelineAnnotation[],
	mergeBy: "ids" | "composition",
	options: { startTime?: number; endTime?: number; includeGaps?: boolean } = {},
): Region[] {
	if (annotations.length === 0) return [];

	const viewStart = options.startTime ?? -Infinity;
	const viewEnd = options.endTime ?? Infinity;

	// Collect breakpoints, clipped to view window.
	const points = new Set<number>();
	for (const a of annotations) {
		if (a.endTime <= viewStart || a.startTime >= viewEnd) continue;
		points.add(Math.max(a.startTime, viewStart));
		points.add(Math.min(a.endTime, viewEnd));
	}
	if (points.size === 0) return [];

	const sorted = [...points].sort((a, b) => a - b);
	const out: Region[] = [];

	for (let i = 0; i + 1 < sorted.length; i++) {
		const start = sorted[i];
		const end = sorted[i + 1];
		if (end - start < 1e-6) continue;
		const mid = (start + end) / 2;
		const active = annotations
			.filter((a) => a.startTime <= mid && a.endTime > mid)
			.sort((a, b) => b.zIndex - a.zIndex);
		if (active.length === 0) {
			if (options.includeGaps) {
				out.push({ startTime: start, endTime: end, clips: [] });
			}
			continue;
		}
		out.push({ startTime: start, endTime: end, clips: active });
	}

	// Merge adjacent regions whose merge-key matches.
	const merged: Region[] = [];
	for (const r of out) {
		const prev = merged[merged.length - 1];
		if (
			prev &&
			prev.endTime >= r.startTime - 1e-6 &&
			sameKey(prev, r, mergeBy)
		) {
			prev.endTime = r.endTime;
			continue;
		}
		merged.push({ ...r });
	}
	return merged;
}

function sameKey(
	a: Region,
	b: Region,
	mergeBy: "ids" | "composition",
): boolean {
	if (a.clips.length !== b.clips.length) return false;
	if (a.clips.length === 0) return true;
	if (mergeBy === "ids") {
		const ai = a.clips.map((c) => c.id).sort();
		const bi = b.clips.map((c) => c.id).sort();
		return ai.every((v, i) => v === bi[i]);
	}
	const ak = a.clips.map(compKey).sort();
	const bk = b.clips.map(compKey).sort();
	return ak.every((v, i) => v === bk[i]);
}

/** Glyph indicating how a clip relates to a view window. */
export function relationGlyph(
	clipStart: number,
	clipEnd: number,
	viewStart: number,
	viewEnd: number,
): "•" | "←" | "→" | "↔" {
	const startsBefore = clipStart < viewStart - 1e-6;
	const endsAfter = clipEnd > viewEnd + 1e-6;
	if (startsBefore && endsAfter) return "↔";
	if (startsBefore) return "←";
	if (endsAfter) return "→";
	return "•";
}

type FormatContext = {
	beatGrid: BeatGrid | null;
	durationSeconds: number;
	totalBars: number | null;
};

function ctxFromState(
	beatGrid: BeatGrid | null,
	durationSeconds: number,
): FormatContext {
	const totalBars = beatGrid ? timeToBar(durationSeconds, beatGrid) : null;
	return { beatGrid, durationSeconds, totalBars };
}

/**
 * Full-track summary view: regions merged by composition (pattern + blend + z),
 * so back-to-back clones of the same pattern collapse into one region.
 * Cheap; intended to live in the system prompt every turn.
 */
export function formatSummary(
	annotations: TimelineAnnotation[],
	beatGrid: BeatGrid | null,
	durationSeconds: number,
): string {
	if (annotations.length === 0) return "<empty score>";
	const ctx = ctxFromState(beatGrid, durationSeconds);
	const regions = partitionRegions(annotations, "composition");
	const lines: string[] = [];
	lines.push(
		`SUMMARY  full track${ctx.totalBars ? ` (${formatBar(ctx.totalBars)} bars)` : ""}`,
	);
	for (const r of regions) {
		const sb = ctx.beatGrid
			? timeToBar(r.startTime, ctx.beatGrid)
			: r.startTime;
		const eb = ctx.beatGrid ? timeToBar(r.endTime, ctx.beatGrid) : r.endTime;
		const range = ctx.beatGrid
			? `bars ${formatBarRange(sb, eb, ctx.totalBars)}`
			: `${r.startTime.toFixed(2)}–${r.endTime.toFixed(2)}s`;
		const stack = r.clips
			.map((c) => {
				const name = c.patternName ?? c.patternId;
				const blend = c.blendMode ?? "replace";
				return `z${c.zIndex} ${name}(${blend})`;
			})
			.join(" / ");
		lines.push(`  ${range.padEnd(18)} ${stack}`);
	}
	return lines.join("\n");
}

/**
 * Detailed view of a bar range. Lists clips per layer with full args, marks
 * how each clip relates to the view window, and describes the stack regions
 * inside the window. Regions are merged by exact id-set so individual clips
 * stay addressable.
 */
export function formatNormal(
	annotations: TimelineAnnotation[],
	beatGrid: BeatGrid | null,
	durationSeconds: number,
	startBar: number,
	endBar: number,
): string {
	const ctx = ctxFromState(beatGrid, durationSeconds);
	const startTime = ctx.beatGrid ? barToTime(startBar, ctx.beatGrid) : startBar;
	const endTime = ctx.beatGrid ? barToTime(endBar, ctx.beatGrid) : endBar;

	const touching = annotations
		.filter((a) => a.endTime > startTime && a.startTime < endTime)
		.sort((a, b) => b.zIndex - a.zIndex || a.startTime - b.startTime);

	if (touching.length === 0) {
		return `VIEW bars ${formatBarRange(startBar, endBar, ctx.totalBars)}\n<no clips in range>`;
	}

	const lines: string[] = [];
	lines.push(`VIEW bars ${formatBarRange(startBar, endBar, ctx.totalBars)}`);

	// Group by zIndex desc.
	const byZ = new Map<number, TimelineAnnotation[]>();
	for (const c of touching) {
		const list = byZ.get(c.zIndex) ?? [];
		list.push(c);
		byZ.set(c.zIndex, list);
	}
	const zs = [...byZ.keys()].sort((a, b) => b - a);

	lines.push("");
	lines.push("LAYERS (top → bottom):");
	for (const z of zs) {
		const layerClips = (byZ.get(z) ?? []).sort(
			(a, b) => a.startTime - b.startTime,
		);
		lines.push(`  z=${z}`);
		for (const c of layerClips) {
			const sb = ctx.beatGrid
				? timeToBar(c.startTime, ctx.beatGrid)
				: c.startTime;
			const eb = ctx.beatGrid ? timeToBar(c.endTime, ctx.beatGrid) : c.endTime;
			const rel = relationGlyph(c.startTime, c.endTime, startTime, endTime);
			const blend = c.blendMode ?? "replace";
			const name = c.patternName ?? c.patternId;
			const range = ctx.beatGrid
				? `bars ${formatBarRange(sb, eb, ctx.totalBars)}`
				: `${c.startTime.toFixed(2)}–${c.endTime.toFixed(2)}s`;
			const args = formatArgsCompact(c.args);
			lines.push(
				`    ${rel} ${range.padEnd(16)} ${name}  blend=${blend}  #${c.id}${args ? `  args=${args}` : ""}`,
			);
		}
	}

	const regions = partitionRegions(annotations, "ids", {
		startTime,
		endTime,
	});
	if (regions.length > 0) {
		lines.push("");
		lines.push("REGIONS:");
		for (const r of regions) {
			const sb = ctx.beatGrid
				? timeToBar(r.startTime, ctx.beatGrid)
				: r.startTime;
			const eb = ctx.beatGrid ? timeToBar(r.endTime, ctx.beatGrid) : r.endTime;
			const range = ctx.beatGrid
				? `bars ${formatBarRange(sb, eb, ctx.totalBars)}`
				: `${r.startTime.toFixed(2)}–${r.endTime.toFixed(2)}s`;
			const stack = r.clips
				.map((c) => {
					const name = c.patternName ?? c.patternId;
					const blend = c.blendMode ?? "replace";
					return `z${c.zIndex} ${name}(${blend})`;
				})
				.join(" + ");
			lines.push(`  ${range.padEnd(16)} ${stack}`);
		}
	}

	return lines.join("\n");
}

/** Instantaneous stack at a given bar. */
export function formatAt(
	annotations: TimelineAnnotation[],
	beatGrid: BeatGrid | null,
	durationSeconds: number,
	bar: number,
): string {
	const ctx = ctxFromState(beatGrid, durationSeconds);
	const t = ctx.beatGrid ? barToTime(bar, ctx.beatGrid) : bar;
	const active = annotations
		.filter((a) => a.startTime <= t && a.endTime > t)
		.sort((a, b) => a.zIndex - b.zIndex); // bottom → top
	if (active.length === 0) {
		return `AT bar ${formatBar(bar)}\n<no clips active>`;
	}
	const lines: string[] = [];
	lines.push(`AT bar ${formatBar(bar)}  (bottom → top)`);
	for (const c of active) {
		const sb = ctx.beatGrid
			? timeToBar(c.startTime, ctx.beatGrid)
			: c.startTime;
		const eb = ctx.beatGrid ? timeToBar(c.endTime, ctx.beatGrid) : c.endTime;
		const blend = c.blendMode ?? "replace";
		const name = c.patternName ?? c.patternId;
		const range = ctx.beatGrid
			? `bars ${formatBarRange(sb, eb, ctx.totalBars)}`
			: `${c.startTime.toFixed(2)}–${c.endTime.toFixed(2)}s`;
		lines.push(`  z=${c.zIndex}  ${name}  blend=${blend}  ${range}  #${c.id}`);
	}
	return lines.join("\n");
}

/** Compact one-line stringification of args; truncated if very long. */
function formatArgsCompact(args: unknown): string {
	if (args == null) return "";
	if (typeof args !== "object") return JSON.stringify(args);
	const keys = Object.keys(args as Record<string, unknown>);
	if (keys.length === 0) return "";
	const flat: string[] = [];
	for (const k of keys) {
		const v = (args as Record<string, unknown>)[k];
		flat.push(`${k}:${formatArgValue(v)}`);
	}
	const joined = flat.join(", ");
	return joined.length > 200 ? `${joined.slice(0, 197)}…` : joined;
}

function formatArgValue(v: unknown): string {
	if (v == null) return "null";
	if (typeof v === "number")
		return Number.isInteger(v) ? String(v) : v.toFixed(3);
	if (typeof v === "string")
		return v.length > 40 ? `"${v.slice(0, 37)}…"` : `"${v}"`;
	if (typeof v === "boolean") return String(v);
	if (typeof v === "object") {
		// Special-case RGB color objects { r, g, b } / { r, g, b, a }
		const obj = v as Record<string, unknown>;
		if (
			typeof obj.r === "number" &&
			typeof obj.g === "number" &&
			typeof obj.b === "number"
		) {
			const r = Math.round(obj.r as number);
			const g = Math.round(obj.g as number);
			const b = Math.round(obj.b as number);
			const hex = `#${[r, g, b]
				.map((n) => n.toString(16).padStart(2, "0"))
				.join("")}`;
			return hex;
		}
		const s = JSON.stringify(v);
		return s.length > 60 ? `${s.slice(0, 57)}…` : s;
	}
	return String(v);
}

/**
 * Find the lowest existing zIndex where placing a clip in [start, end] would
 * not overlap any existing clip on that layer. If every existing layer is
 * occupied at this range, returns max(z) + 1 (a fresh top layer).
 * For an empty score, returns 0.
 */
export function lowestFreeZ(
	annotations: TimelineAnnotation[],
	startTime: number,
	endTime: number,
): number {
	if (annotations.length === 0) return 0;
	const zs = Array.from(new Set(annotations.map((a) => a.zIndex))).sort(
		(a, b) => a - b,
	);
	for (const z of zs) {
		const overlaps = annotations.some(
			(a) => a.zIndex === z && a.startTime < endTime && a.endTime > startTime,
		);
		if (!overlaps) return z;
	}
	return zs[zs.length - 1] + 1;
}

/** Whether placing a clip in [start, end] at zIndex `z` would conflict. */
export function findOverlappingClip(
	annotations: TimelineAnnotation[],
	startTime: number,
	endTime: number,
	z: number,
	excludeId?: string,
): TimelineAnnotation | null {
	for (const a of annotations) {
		if (a.id === excludeId) continue;
		if (a.zIndex !== z) continue;
		if (a.startTime < endTime && a.endTime > startTime) return a;
	}
	return null;
}
