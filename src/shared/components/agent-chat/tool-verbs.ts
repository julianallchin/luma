import type { RenderPart, ToolVerb, ToolView, ToolVocab } from "./parts";

const DEFAULT_VERB: ToolVerb = {
	running: "Running",
	past: "Ran",
	noun: "tool",
};

const PLURALS: Record<string, string> = {
	"node parameters": "node parameters",
};

const pluralize = (noun: string, count: number): string =>
	count === 1 ? noun : (PLURALS[noun] ?? `${noun}s`);

const listify = (parts: string[]): string =>
	parts.length <= 2
		? parts.join(" and ")
		: `${parts.slice(0, -1).join(", ")}, and ${parts.at(-1)}`;

const oneLine = (value: string): string => value.trim().replace(/\s+/g, " ");

const clip = (value: string, max = 48): string => {
	const line = oneLine(value);
	return line.length > max ? `${line.slice(0, max - 1)}…` : line;
};

export type ToolDescription = ToolVerb & { inline?: string };

/** The feature owns tool-specific wording; this shared layer adds bounded
 * inline detail and the default narration for unknown/historical tools. */
export function describeTool(
	tool: ToolView,
	vocab: ToolVocab,
): ToolDescription {
	const verb = vocab.verbs[tool.name] ?? DEFAULT_VERB;
	const inline = vocab.formatLabel(tool).detail;
	return inline ? { ...verb, inline: clip(inline) } : verb;
}

export const verbForStatus = (
	view: ToolDescription,
	status: ToolView["state"],
): string =>
	status === "pending" || status === "running" ? view.running : view.past;

export const estimateTokens = (text: string): number => {
	const words = text.trim() ? text.trim().split(/\s+/).length : 0;
	return Math.round((words * 4) / 3);
};

const tokenSuffix = (tokens: number): string =>
	tokens > 0 ? ` for ${tokens} token${tokens === 1 ? "" : "s"}` : "";

export function thinkingLabel(
	text: string,
	streaming = false,
): { verb: string; rest: string } {
	return streaming
		? { verb: "Thinking", rest: "" }
		: { verb: "Thought", rest: tokenSuffix(estimateTokens(text)) };
}

export type SummarySegment = { verb: string; rest: string; live: boolean };

const THINKING_KEY = "\u0000thinking";

/** Collapse a run in first-appearance order. Tools sharing a status-aware verb
 * merge without repeating it: "Ran 2 python cells, running 1 graph". */
export function summarize(
	parts: RenderPart[],
	vocab: ToolVocab,
	reasoningActive = false,
): SummarySegment[] {
	const order: string[] = [];
	const byVerb = new Map<
		string,
		{
			live: boolean;
			phrases: Map<string, { count: number; isObject: boolean }>;
		}
	>();
	let thinkingTokens = 0;
	let sawThinking = false;

	for (const part of parts) {
		if (part.kind === "reasoning") {
			if (!sawThinking) order.push(THINKING_KEY);
			sawThinking = true;
			thinkingTokens += estimateTokens(part.text);
			continue;
		}
		if (part.kind !== "tool") continue;
		const { running, past, noun, object } =
			vocab.verbs[part.tool.name] ?? DEFAULT_VERB;
		const live = part.tool.state === "pending" || part.tool.state === "running";
		const key = (live ? running : past).toLowerCase();
		if (!byVerb.has(key)) {
			byVerb.set(key, { live, phrases: new Map() });
			order.push(key);
		}
		const group = byVerb.get(key);
		if (!group) continue;
		const phrase = object ?? noun;
		const entry = group.phrases.get(phrase) ?? {
			count: 0,
			isObject: object !== undefined,
		};
		entry.count += 1;
		group.phrases.set(phrase, entry);
	}

	return order.map((key) => {
		if (key === THINKING_KEY) {
			return reasoningActive
				? { verb: "thinking", rest: "", live: true }
				: {
						verb: "thought",
						rest: tokenSuffix(thinkingTokens),
						live: false,
					};
		}
		const group = byVerb.get(key);
		const phrases = [...(group?.phrases ?? [])];
		const anyCountedObject = phrases.some(
			([, phrase]) => phrase.isObject && phrase.count > 1,
		);
		const details = phrases.map(([phrase, { count, isObject }]): string => {
			if (!isObject) return `${count} ${pluralize(phrase, count)}`;
			if (count > 1) return `${phrase} ${count} times`;
			return anyCountedObject ? `${phrase} once` : phrase;
		});
		return {
			verb: key,
			rest: ` ${listify(details)}`,
			live: group?.live ?? false,
		};
	});
}
