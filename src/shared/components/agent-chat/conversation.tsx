import { code } from "@streamdown/code";
import { ChevronRight } from "lucide-react";
import { createContext, useContext, useEffect, useMemo, useState } from "react";
import { Streamdown } from "streamdown";
import type { AgentChatMessage } from "@/shared/lib/agent/messages";
import { cn } from "@/shared/lib/utils";
import {
	type RenderPart,
	type ToolView,
	type ToolVocab,
	toRenderParts,
} from "./parts";
import {
	describeTool,
	summarize,
	thinkingLabel,
	verbForStatus,
} from "./tool-verbs";

const DEFAULT_VOCAB: ToolVocab = {
	verbs: {},
	formatLabel: (tool) => ({ verb: tool.name, detail: null }),
};

const VocabContext = createContext<ToolVocab>(DEFAULT_VOCAB);

/** Browser-native virtualization: off-screen rows keep their remembered size
 * but skip layout and paint entirely, so a pane resize (or a long transcript)
 * only pays for the rows actually in view. The intrinsic-size fallback is a
 * rough one-row estimate; the engine replaces it with the real measurement
 * after first render. */
const ROW_CLASS =
	"[content-visibility:auto] [contain-intrinsic-size:auto_2.5rem]";

/** Render a list of Pi-folded transcript messages. Pass the feature's tool `vocab`
 * so tool runs get readable labels. The grouping/summarizing of reasoning +
 * tool calls is shared across every agent — only the vocab differs. */
export function AgentConversation({
	messages,
	streaming,
	vocab = DEFAULT_VOCAB,
}: {
	messages: AgentChatMessage[];
	streaming: boolean;
	vocab?: ToolVocab;
}) {
	const rows = useMemo(() => groupConversationMessages(messages), [messages]);
	return (
		<VocabContext.Provider value={vocab}>
			{rows.map((row, i) =>
				row.kind === "user" ? (
					<UserMessage key={row.message.id} message={row.message} />
				) : (
					<AssistantRun
						key={row.messages[0]?.id ?? `assistant-${i}`}
						messages={row.messages}
						isStreaming={streaming && i === rows.length - 1}
					/>
				),
			)}
		</VocabContext.Provider>
	);
}

type ConversationRow =
	| { kind: "user"; message: AgentChatMessage }
	| { kind: "assistant"; messages: AgentChatMessage[] };

/** Tool results can make one visible assistant response span several Pi
 * messages. Keep consecutive assistant messages in one render run so empty
 * turn boundaries do not split thinking/tool activity into separate groups. */
function groupConversationMessages(
	messages: AgentChatMessage[],
): ConversationRow[] {
	const rows: ConversationRow[] = [];
	for (const message of messages) {
		const last = rows[rows.length - 1];
		if (message.role !== "user" && last?.kind === "assistant") {
			last.messages.push(message);
		} else if (message.role !== "user") {
			rows.push({ kind: "assistant", messages: [message] });
		} else {
			rows.push({ kind: "user", message });
		}
	}
	return rows;
}

function UserMessage({ message }: { message: AgentChatMessage }) {
	const text = message.parts
		.map((part) => (part.type === "text" ? part.text : ""))
		.join("");
	return (
		<div className={cn("flex justify-end", ROW_CLASS)}>
			<div className="max-w-[90%] rounded-2xl bg-primary/15 text-foreground px-2.5 py-1.5 text-xs whitespace-pre-wrap break-words leading-relaxed">
				{text}
			</div>
		</div>
	);
}

function AssistantRun({
	messages,
	isStreaming,
}: {
	messages: AgentChatMessage[];
	isStreaming: boolean;
}) {
	const parts = useMemo(
		() =>
			messages.flatMap((message) =>
				toRenderParts(message).map((part) => ({
					...part,
					id: `${message.id}:${part.id}`,
				})),
			),
		[messages],
	);
	const segments = useMemo(() => groupAssistantParts(parts), [parts]);
	return (
		<div className={cn("space-y-1.5", ROW_CLASS)}>
			{segments.length === 0 ? (
				<div className="text-[11px] italic text-muted-foreground">…</div>
			) : (
				segments.map((seg, i) => {
					const isLastSegment = i === segments.length - 1;
					if (seg.kind === "text") {
						return (
							<MarkdownText
								key={`t-${seg.part.id}-${i}`}
								text={seg.part.text}
								streaming={isStreaming && isLastSegment}
							/>
						);
					}
					return (
						<ToolRun
							key={`run-${runKey(seg.parts)}-${i}`}
							parts={seg.parts}
							isStreaming={isStreaming && isLastSegment}
						/>
					);
				})
			)}
		</div>
	);
}

type AssistantSegment =
	| { kind: "text"; part: Extract<RenderPart, { kind: "text" }> }
	| { kind: "run"; parts: RenderPart[] };

function groupAssistantParts(parts: RenderPart[]): AssistantSegment[] {
	const segments: AssistantSegment[] = [];
	let runBuf: RenderPart[] = [];
	let textBuf: { id: string; text: string } | null = null;

	const flushRun = () => {
		if (runBuf.length === 0) return;
		segments.push({ kind: "run", parts: runBuf });
		runBuf = [];
	};
	const flushText = () => {
		if (!textBuf) return;
		const trimmed = textBuf.text.replace(/^\s+/, "");
		if (trimmed.length > 0) {
			segments.push({
				kind: "text",
				part: { kind: "text", id: textBuf.id, text: trimmed },
			});
		}
		textBuf = null;
	};

	for (const p of parts) {
		if (p.kind === "text") {
			if (!/\S/.test(p.text)) continue;
			flushRun();
			if (textBuf) textBuf.text += p.text;
			else textBuf = { id: p.id, text: p.text };
		} else {
			flushText();
			runBuf.push(p);
		}
	}
	flushRun();
	flushText();
	return segments;
}

function runKey(parts: RenderPart[]): string {
	const first = parts[0];
	if (!first) return "empty";
	if (first.kind === "tool") return first.tool.callId;
	return first.id;
}

function partKey(part: RenderPart, index: number): string {
	if (part.kind === "tool") return `tool-${part.tool.callId}`;
	return `${part.kind}-${part.id}-${index}`;
}

const MARKDOWN_CLASSNAME =
	"text-xs text-foreground/90 leading-relaxed break-words " +
	"[&>*:first-child]:mt-0 [&>*:last-child]:mb-0 " +
	"[&_p]:my-1.5 " +
	"[&_h1]:text-sm [&_h1]:font-semibold [&_h1]:mt-2 [&_h1]:mb-1 " +
	"[&_h2]:text-xs [&_h2]:font-semibold [&_h2]:mt-2 [&_h2]:mb-1 " +
	"[&_h3]:text-xs [&_h3]:font-semibold [&_h3]:mt-1.5 [&_h3]:mb-0.5 " +
	"[&_ul]:list-disc [&_ul]:pl-4 [&_ul]:my-1.5 " +
	"[&_ol]:list-decimal [&_ol]:pl-4 [&_ol]:my-1.5 " +
	"[&_li]:my-0.5 " +
	"[&_code]:font-mono [&_code]:text-[0.85em] [&_code]:bg-muted/50 [&_code]:px-1 [&_code]:py-0.5 [&_code]:rounded " +
	"[&_pre]:bg-muted/50 [&_pre]:p-2 [&_pre]:rounded [&_pre]:my-1.5 [&_pre]:overflow-auto " +
	"[&_pre_code]:bg-transparent [&_pre_code]:p-0 " +
	"[&_a]:text-blue-400 [&_a]:underline [&_a]:underline-offset-2 " +
	"[&_strong]:font-semibold [&_em]:italic " +
	"[&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-2 [&_blockquote]:text-muted-foreground " +
	"[&_table]:border-collapse [&_table]:my-1.5 [&_th]:border [&_th]:border-border [&_th]:px-2 [&_th]:py-0.5 " +
	"[&_td]:border [&_td]:border-border [&_td]:px-2 [&_td]:py-0.5";

function MarkdownText({
	text,
	streaming = false,
}: {
	text: string;
	streaming?: boolean;
}) {
	const cleaned = cleanResponseText(text);
	return (
		<Streamdown
			mode={streaming ? "streaming" : "static"}
			isAnimating={streaming}
			plugins={{ code }}
			className={MARKDOWN_CLASSNAME}
		>
			{cleaned}
		</Streamdown>
	);
}

function cleanResponseText(text: string): string {
	let out = text;
	out = out.replace(
		/<(think|thinking|reasoning)\b[^>]*>[\s\S]*?<\/\1\s*>/gi,
		"",
	);
	out = out.replace(/<(think|thinking|reasoning)\b[^>]*>[\s\S]*$/i, "");
	return out;
}

const VERB = "text-muted-foreground";
const DETAIL = "text-muted-foreground/75";
const SHIMMER = "agent-shimmer";

const cap = (value: string): string =>
	value.charAt(0).toUpperCase() + value.slice(1);

const isRunning = (tool: ToolView): boolean =>
	tool.state === "pending" || tool.state === "running";

function Caret({ open }: { open: boolean }) {
	return (
		<ChevronRight
			className={cn(
				"size-4 shrink-0 transition-[opacity,transform]",
				open ? "rotate-90 opacity-100" : "opacity-0 group-hover:opacity-100",
			)}
		/>
	);
}

function Disclosure({
	header,
	detail,
	open,
	onToggle,
}: {
	header: React.ReactNode;
	detail?: React.ReactNode;
	open: boolean;
	onToggle: () => void;
}) {
	if (!detail) {
		return (
			<div className={cn("flex items-center gap-2", DETAIL)}>{header}</div>
		);
	}
	return (
		<div>
			<button
				type="button"
				onClick={onToggle}
				className={cn("group flex w-full items-center gap-2 text-left", DETAIL)}
			>
				{header}
				<Caret open={open} />
			</button>
			{open ? <div className="mt-1">{detail}</div> : null}
		</div>
	);
}

function ActivityRow({
	header,
	detail,
	live,
}: {
	header: React.ReactNode;
	detail?: React.ReactNode;
	live?: boolean;
}) {
	const [open, setOpen] = useState(live ?? false);
	useEffect(() => {
		if (live !== undefined) setOpen(live);
	}, [live]);
	return (
		<Disclosure
			header={header}
			detail={detail}
			open={open}
			onToggle={() => setOpen((value) => !value)}
		/>
	);
}

function ThinkingText({ text, live }: { text: string; live: boolean }) {
	const label = thinkingLabel(text, live);
	return (
		<span>
			<strong className={cn("font-medium", VERB, live && SHIMMER)}>
				{label.verb}
			</strong>
			{label.rest}
		</span>
	);
}

function ToolHeader({ tool }: { tool: ToolView }) {
	const vocab = useContext(VocabContext);
	const view = describeTool(tool, vocab);
	return (
		<span>
			<strong
				className={cn(
					"font-medium",
					tool.state === "error" ? "text-destructive" : VERB,
					isRunning(tool) && SHIMMER,
				)}
			>
				{verbForStatus(view, tool.state)}
			</strong>
			{view.inline ? <> {view.inline}</> : null}
			{tool.state === "error" ? (
				<span className="text-destructive"> — failed</span>
			) : null}
		</span>
	);
}

function ToolRun({
	parts,
	isStreaming,
}: {
	parts: RenderPart[];
	isStreaming: boolean;
}) {
	const vocab = useContext(VocabContext);
	const [open, setOpen] = useState(false);
	const active =
		isStreaming ||
		parts.some((part) => part.kind === "tool" && isRunning(part.tool));

	if (parts.length === 1) {
		const part = parts[0];
		if (!part) return null;
		const header =
			part.kind === "reasoning" ? (
				<ThinkingText text={part.text} live={active} />
			) : part.kind === "tool" ? (
				<ToolHeader tool={part.tool} />
			) : null;
		const detail =
			part.kind === "reasoning"
				? reasoningDetail(part.text)
				: part.kind === "tool"
					? toolDetail(part.tool)
					: undefined;
		return (
			<div className="text-sm">
				<Disclosure
					header={header}
					detail={detail}
					open={open}
					onToggle={() => setOpen((value) => !value)}
				/>
			</div>
		);
	}

	const lastPart = parts.at(-1);
	const segments = summarize(
		parts,
		vocab,
		isStreaming && lastPart?.kind === "reasoning",
	);
	return (
		<div className="text-sm">
			<button
				type="button"
				onClick={() => setOpen((value) => !value)}
				className={cn("group flex w-full items-center gap-2 text-left", DETAIL)}
			>
				<span>
					{segments.map((segment, index) => (
						<span key={`${segment.verb}-${index}`}>
							{index > 0 ? ", " : null}
							<strong
								className={cn("font-medium", VERB, segment.live && SHIMMER)}
							>
								{index === 0 ? cap(segment.verb) : segment.verb}
							</strong>
							{segment.rest}
						</span>
					))}
				</span>
				<Caret open={open} />
			</button>
			{open ? (
				<div className="mt-1.5 flex flex-col gap-1.5">
					{parts.map((part, originalIndex) => {
						if (part.kind === "tool") {
							return (
								<ActivityRow
									key={partKey(part, originalIndex)}
									header={<ToolHeader tool={part.tool} />}
									detail={toolDetail(part.tool)}
								/>
							);
						}
						if (part.kind !== "reasoning") return null;
						const live = active && originalIndex === parts.length - 1;
						return (
							<ActivityRow
								key={partKey(part, originalIndex)}
								live={live}
								header={<ThinkingText text={part.text} live={live} />}
								detail={reasoningDetail(part.text)}
							/>
						);
					})}
				</div>
			) : null}
		</div>
	);
}

function reasoningDetail(text: string): React.ReactNode | undefined {
	return text ? (
		<div className={cn("whitespace-pre-wrap break-words", DETAIL)}>{text}</div>
	) : undefined;
}

function toolDetail(tool: ToolView): React.ReactNode | undefined {
	if (
		tool.input === undefined &&
		tool.output === undefined &&
		tool.error === undefined
	) {
		return undefined;
	}
	return <ToolDetail tool={tool} />;
}

function ToolDetail({ tool }: { tool: ToolView }) {
	const renderer = useContext(VocabContext).renderers?.[tool.name];
	return (
		<div className="overflow-hidden rounded-lg bg-control px-2 py-1.5">
			{renderer ? renderer(tool) : <GenericToolDetail tool={tool} />}
		</div>
	);
}

function GenericToolDetail({ tool }: { tool: ToolView }) {
	const images = extractImageOutputs(tool.output);
	const hasInput = tool.input !== undefined;
	const hasResult = tool.output !== undefined || tool.error !== undefined;
	return (
		<div>
			{hasInput ? <JsonBlock value={tool.input} /> : null}
			{hasResult ? (
				<div className={hasInput ? "mt-2 border-t border-border/70 pt-2" : ""}>
					{tool.error ? (
						<pre className="whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-destructive">
							{tool.error}
						</pre>
					) : (
						<JsonBlock value={tool.output} stripBase64 />
					)}
					{images.map((image, i) =>
						image.base64 ? (
							<img
								// Images are positional and never reordered.
								key={`${tool.callId}-img-${i}`}
								src={`data:image/png;base64,${image.base64}`}
								alt="Tool output preview"
								className="mt-2 w-full rounded-sm [image-rendering:pixelated]"
							/>
						) : (
							<div
								key={`${tool.callId}-img-${i}`}
								className="mt-2 text-[11px] text-muted-foreground/70"
							>
								Figure too large to keep in the transcript.
							</div>
						),
					)}
				</div>
			) : null}
		</div>
	);
}

function JsonBlock({
	value,
	stripBase64 = false,
}: {
	value: unknown;
	stripBase64?: boolean;
}) {
	const text = useMemo(() => {
		const cleaned = stripBase64 ? redactBase64(value) : value;
		try {
			return JSON.stringify(cleaned, null, 2);
		} catch {
			return String(cleaned);
		}
	}, [value, stripBase64]);
	return (
		<pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-foreground/75">
			{text}
		</pre>
	);
}

type ToolImage = { base64: string | null; width: number; height: number };

/** Images a tool output carries: either a single heatmap (`base64` at the top
 * level) or a python cell's `figures` list. A figure without base64 was too
 * large to persist — it still renders as a placeholder. */
function extractImageOutputs(output: unknown): ToolImage[] {
	if (!output || typeof output !== "object") return [];
	const o = output as Record<string, unknown>;
	if (typeof o.base64 === "string") {
		return [{ base64: o.base64, width: num(o.width), height: num(o.height) }];
	}
	if (Array.isArray(o.figures)) {
		return o.figures.map((f) => {
			const fig = (f ?? {}) as Record<string, unknown>;
			return {
				base64: typeof fig.base64Png === "string" ? fig.base64Png : null,
				width: num(fig.width),
				height: num(fig.height),
			};
		});
	}
	return [];
}

function num(v: unknown): number {
	return typeof v === "number" ? v : 0;
}

const BASE64_KEYS = new Set(["base64", "base64Png"]);

function redactBase64(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(redactBase64);
	if (value && typeof value === "object") {
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
			if (BASE64_KEYS.has(k) && typeof v === "string") {
				out[k] = `<${v.length} bytes>`;
			} else {
				out[k] = redactBase64(v);
			}
		}
		return out;
	}
	return value;
}
