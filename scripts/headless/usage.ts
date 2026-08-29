/**
 * What a subscription's rate-limit windows look like from this script,
 * whichever vendor's plan they belong to.
 *
 * Both plans (`claude-usage.ts`, `codex-usage.ts`) are read the same way by the
 * gate: one short window that the CLI itself refuses past, and one weekly
 * window that this script refuses past *early*, so a run never starts inside
 * the last half of the week's quota. Everything vendor-specific — endpoint,
 * token, which windows exist — stays in the module that knows it.
 */

/** One rate-limit window. `usedFraction` is 0–1; `resetsAt` is null for a
 * window the plan reports without a rolling reset. */
export type UsageWindow = { usedFraction: number; resetsAt: Date | null };

/** A plan's windows. `weekly` is the one the gate thresholds; `windows` is
 * every one the plan reports, labelled, for the summary line. */
export type PlanUsage = {
	windows: { label: string; window: UsageWindow }[];
	weekly?: UsageWindow;
	short?: UsageWindow;
};

/** "in 4h12m" / "in 5d" — how long until a window rolls over. */
export function untilReset(at: Date | null): string {
	if (!at) return "no reset";
	const ms = at.getTime() - Date.now();
	if (ms <= 0) return "now";
	const minutes = Math.round(ms / 60_000);
	if (minutes < 60) return `in ${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `in ${hours}h${String(minutes % 60).padStart(2, "0")}m`;
	return `in ${Math.floor(hours / 24)}d${hours % 24}h`;
}

/** One line: every window the plan reports, with its percentage and reset. */
export function summarizeUsage(usage: PlanUsage): string {
	const parts = usage.windows.map(
		({ label, window: w }) =>
			`${label} ${(w.usedFraction * 100).toFixed(0)}% (resets ${untilReset(w.resetsAt)})`,
	);
	return parts.length ? parts.join("   ") : "no usage windows reported";
}
