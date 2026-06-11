import type { UIMessage } from "ai";
import { ChevronDown, ChevronRight, Loader2 } from "lucide-react";
import {
	createContext,
	useContext,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { Streamdown } from "streamdown";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	type RenderPart,
	type ToolView,
	type ToolVocab,
	toRenderParts,
} from "./parts";

const DEFAULT_VOCAB: ToolVocab = {
	verbs: {},
	formatLabel: (tool) => ({ verb: tool.name, detail: null }),
};

const VocabContext = createContext<ToolVocab>(DEFAULT_VOCAB);

/** Render a list of native SDK `UIMessage`s. Pass the feature's tool `vocab`
 * so tool runs get readable labels. The grouping/summarizing of reasoning +
 * tool calls is shared across every agent — only the vocab differs. */
export function AgentConversation({
	messages,
	streaming,
	vocab = DEFAULT_VOCAB,
}: {
	messages: UIMessage[];
	streaming: boolean;
	vocab?: ToolVocab;
}) {
	return (
		<VocabContext.Provider value={vocab}>
			{messages.map((m, i) => (
				<MessageBubble
					key={m.id}
					message={m}
					isStreaming={streaming && i === messages.length - 1}
				/>
			))}
		</VocabContext.Provider>
	);
}

function MessageBubble({
	message,
	isStreaming,
}: {
	message: UIMessage;
	isStreaming: boolean;
}) {
	if (message.role === "user") {
		const text = message.parts
			.map((p) => (p.type === "text" ? p.text : ""))
			.join("");
		return (
			<div className="flex justify-end">
				<div className="max-w-[90%] rounded-2xl rounded-br-sm bg-primary/15 text-foreground px-2.5 py-1.5 text-xs whitespace-pre-wrap break-words leading-relaxed">
					{text}
				</div>
			</div>
		);
	}
	return <AssistantMessage message={message} isStreaming={isStreaming} />;
}

function AssistantMessage({
	message,
	isStreaming,
}: {
	message: UIMessage;
	isStreaming: boolean;
}) {
	const parts = useMemo(() => toRenderParts(message), [message]);
	const segments = useMemo(() => groupAssistantParts(parts), [parts]);
	const last = parts[parts.length - 1];
	const activeReasoningId =
		isStreaming && last?.kind === "reasoning" ? last.id : null;
	return (
		<div className="space-y-1.5">
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
							/>
						);
					}
					return (
						<ToolRun
							key={`run-${runKey(seg.parts)}-${i}`}
							parts={seg.parts}
							isStreaming={isStreaming && isLastSegment}
							activeReasoningId={isLastSegment ? activeReasoningId : null}
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

const REASONING_MARKDOWN_CLASSNAME =
	"text-xs italic text-muted-foreground leading-relaxed break-words " +
	"[&>*:first-child]:mt-0 [&>*:last-child]:mb-0 " +
	"[&_p]:my-1 " +
	"[&_h1]:text-xs [&_h1]:font-semibold [&_h1]:mt-1.5 [&_h1]:mb-0.5 " +
	"[&_h2]:text-xs [&_h2]:font-semibold [&_h2]:mt-1.5 [&_h2]:mb-0.5 " +
	"[&_h3]:text-xs [&_h3]:font-semibold [&_h3]:mt-1 [&_h3]:mb-0.5 " +
	"[&_ul]:list-disc [&_ul]:pl-4 [&_ul]:my-1 " +
	"[&_ol]:list-decimal [&_ol]:pl-4 [&_ol]:my-1 " +
	"[&_li]:my-0.5 " +
	"[&_code]:font-mono [&_code]:text-[0.85em] [&_code]:bg-muted/50 [&_code]:px-1 [&_code]:py-0.5 [&_code]:rounded [&_code]:not-italic " +
	"[&_pre]:bg-muted/50 [&_pre]:p-2 [&_pre]:rounded [&_pre]:my-1 [&_pre]:overflow-auto [&_pre]:not-italic " +
	"[&_pre_code]:bg-transparent [&_pre_code]:p-0 " +
	"[&_a]:underline [&_a]:underline-offset-2 " +
	"[&_strong]:font-semibold";

function MarkdownText({ text }: { text: string }) {
	return (
		<Streamdown className={MARKDOWN_CLASSNAME}>
			{cleanResponseText(text)}
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
	out = out.replace(/```[a-zA-Z0-9_+-]*\n?([\s\S]*?)```/g, "$1");
	out = out.replace(/```[a-zA-Z0-9_+-]*\n?/g, "");
	out = out.replace(/`([^`\n]+)`/g, "$1");
	return out;
}

function summarizeRun(
	parts: RenderPart[],
	verbs: ToolVocab["verbs"],
): Array<{ verb: string; detail: string }> {
	const tools = parts.filter(
		(p): p is Extract<RenderPart, { kind: "tool" }> => p.kind === "tool",
	);
	const reasonings = parts.filter(
		(p): p is Extract<RenderPart, { kind: "reasoning" }> =>
			p.kind === "reasoning",
	);
	const out: Array<{ verb: string; detail: string }> = [];
	if (reasonings.length > 0) {
		const totalMs = reasonings.reduce(
			(sum, r) => sum + Math.max(0, (r.lastDeltaAt ?? 0) - (r.startedAt ?? 0)),
			0,
		);
		out.push({
			verb: "Thought",
			detail: totalMs > 0 ? `for ${formatReasoningDuration(totalMs)}` : "",
		});
	}
	if (tools.length === 0 && reasonings.length === 0) {
		return [{ verb: "Thought", detail: "" }];
	}
	const counts = new Map<string, number>();
	for (const t of tools) {
		counts.set(t.tool.name, (counts.get(t.tool.name) ?? 0) + 1);
	}
	for (const [name, count] of counts) {
		const meta = verbs[name];
		const verbRaw = meta?.past ?? name;
		const verb = out.length === 0 ? verbRaw : lcFirst(verbRaw);
		if (!meta) {
			out.push({ verb, detail: `×${count}` });
		} else if (meta.noun === null) {
			out.push({ verb, detail: count === 1 ? "" : `${count} times` });
		} else {
			const noun = count === 1 ? meta.noun : `${meta.noun}s`;
			out.push({ verb, detail: `${count} ${noun}` });
		}
	}
	return out;
}

function lcFirst(s: string): string {
	return s.charAt(0).toLowerCase() + s.slice(1);
}

function formatReasoningDuration(ms: number): string {
	if (ms < 1000) return "<1s";
	const sec = Math.round(ms / 1000);
	if (sec < 60) return `${sec}s`;
	const min = Math.floor(sec / 60);
	const rem = sec % 60;
	return rem > 0 ? `${min}m ${rem}s` : `${min}m`;
}

function VerbDetail({
	verb,
	detail,
	error,
}: {
	verb: string;
	detail?: string | null;
	error?: boolean;
}) {
	return (
		<span className="text-xs leading-relaxed">
			<span className={error ? "text-destructive" : "text-muted-foreground"}>
				{verb}
			</span>
			{detail ? (
				<>
					{" "}
					<span className="text-muted-foreground/50">{detail}</span>
				</>
			) : null}
		</span>
	);
}

function ToolRun({
	parts,
	isStreaming,
	activeReasoningId,
}: {
	parts: RenderPart[];
	isStreaming: boolean;
	activeReasoningId: string | null;
}) {
	const hasTools = parts.some((p) => p.kind === "tool");
	if (!hasTools) {
		return (
			<div className="space-y-1">
				{parts.map((p, i) => (
					<RunLineView
						key={partKey(p, i)}
						part={p}
						activeReasoningId={activeReasoningId}
					/>
				))}
			</div>
		);
	}
	return (
		<ToolRunAggregated
			parts={parts}
			isStreaming={isStreaming}
			activeReasoningId={activeReasoningId}
		/>
	);
}

function ToolRunAggregated({
	parts,
	isStreaming,
	activeReasoningId,
}: {
	parts: RenderPart[];
	isStreaming: boolean;
	activeReasoningId: string | null;
}) {
	const { verbs } = useContext(VocabContext);
	const [open, setOpen] = useState(false);

	const activeReasoning = activeReasoningId
		? (parts.find(
				(p): p is Extract<RenderPart, { kind: "reasoning" }> =>
					p.kind === "reasoning" && p.id === activeReasoningId,
			) ?? null)
		: null;
	const summaryParts = activeReasoning
		? parts.filter((p) => p !== activeReasoning)
		: parts;
	const inFlight =
		isStreaming &&
		parts.some(
			(p) =>
				p.kind === "tool" &&
				(p.tool.state === "pending" || p.tool.state === "running"),
		);

	const phrases = useMemo(
		() => summarizeRun(summaryParts, verbs),
		[summaryParts, verbs],
	);
	const caretClass =
		"size-3 shrink-0 text-muted-foreground/60 transition-opacity " +
		(open ? "opacity-100" : "opacity-0 group-hover:opacity-100");

	const showSummary = summaryParts.length > 0;

	return (
		<div className="space-y-1">
			{showSummary ? (
				<button
					type="button"
					onClick={() => setOpen((o) => !o)}
					className="group inline-flex max-w-full items-center gap-1.5 text-left"
				>
					{inFlight ? (
						<Loader2 className="size-3 shrink-0 animate-spin text-muted-foreground" />
					) : null}
					<span className="text-xs leading-relaxed min-w-0">
						{phrases.map((p, i) => (
							<span key={`${p.verb}-${i}`}>
								{i > 0 && <span className="text-muted-foreground/50">, </span>}
								<span className="text-muted-foreground">{p.verb}</span>
								{p.detail ? (
									<>
										{" "}
										<span className="text-muted-foreground/50">{p.detail}</span>
									</>
								) : null}
							</span>
						))}
					</span>
					{open ? (
						<ChevronDown className={caretClass} />
					) : (
						<ChevronRight className={caretClass} />
					)}
				</button>
			) : null}
			{showSummary && open ? (
				<div className="space-y-1">
					{summaryParts.map((p, i) => (
						<RunLineView
							key={partKey(p, i)}
							part={p}
							activeReasoningId={null}
						/>
					))}
				</div>
			) : null}
			{activeReasoning ? (
				<ReasoningTrace text={activeReasoning.text} autoscroll />
			) : null}
		</div>
	);
}

function RunLineView({
	part,
	activeReasoningId,
}: {
	part: RenderPart;
	activeReasoningId: string | null;
}) {
	if (part.kind === "text") return <MarkdownText text={part.text} />;
	if (part.kind === "reasoning") {
		const done = part.id !== activeReasoningId;
		return <ReasoningLine part={part} done={done} />;
	}
	return <ToolLine tool={part.tool} />;
}

function ReasoningTrace({
	text,
	autoscroll = false,
}: {
	text: string;
	autoscroll?: boolean;
}) {
	const ref = useRef<HTMLDivElement>(null);

	// Keep the latest thoughts in view by pinning to the bottom.
	useEffect(() => {
		const el = ref.current;
		if (!el) return;
		el.scrollTop = el.scrollHeight;
	}, [text]);

	if (!text.trim()) return null;

	// While actively thinking: a fixed peek that shows only the last thoughts —
	// no scrollbar, not user-scrollable, fading out at the top. Once done (the
	// expanded "Thought for Ns" trace), it's a taller scrollable region.
	if (autoscroll) {
		return (
			<div
				ref={ref}
				className="max-h-[140px] overflow-hidden"
				style={{
					WebkitMaskImage:
						"linear-gradient(to bottom, transparent, black 2rem)",
					maskImage: "linear-gradient(to bottom, transparent, black 2rem)",
				}}
			>
				<Streamdown className={REASONING_MARKDOWN_CLASSNAME}>{text}</Streamdown>
			</div>
		);
	}

	return (
		<div className="max-h-[500px] overflow-y-auto">
			<Streamdown className={REASONING_MARKDOWN_CLASSNAME}>{text}</Streamdown>
		</div>
	);
}

function ReasoningLine({
	part,
	done,
}: {
	part: Extract<RenderPart, { kind: "reasoning" }>;
	done: boolean;
}) {
	const [open, setOpen] = useState(false);
	if (!part.text.trim() && !done) return null;
	if (!done) return <ReasoningTrace text={part.text} autoscroll />;

	const ms = Math.max(0, (part.lastDeltaAt ?? 0) - (part.startedAt ?? 0));
	const hasTrace = part.text.trim().length > 0;
	const caretClass =
		"size-3 shrink-0 text-muted-foreground/60 transition-opacity " +
		(open ? "opacity-100" : "opacity-0 group-hover:opacity-100");

	return (
		<div>
			<button
				type="button"
				onClick={() => hasTrace && setOpen((o) => !o)}
				disabled={!hasTrace}
				className="group inline-flex max-w-full items-center gap-1.5 text-left"
			>
				<VerbDetail
					verb="Thought"
					detail={ms > 0 ? `for ${formatReasoningDuration(ms)}` : null}
				/>
				{hasTrace ? (
					open ? (
						<ChevronDown className={caretClass} />
					) : (
						<ChevronRight className={caretClass} />
					)
				) : null}
			</button>
			{open && hasTrace ? (
				<div className="mt-1">
					<ReasoningTrace text={part.text} />
				</div>
			) : null}
		</div>
	);
}

function ToolLine({ tool }: { tool: ToolView }) {
	const { formatLabel } = useContext(VocabContext);
	const inFlight = tool.state === "pending" || tool.state === "running";
	const isError = tool.state === "error";
	const { verb, detail } = formatLabel(tool);
	const hasDetail =
		tool.state === "done" || tool.state === "error" || tool.input !== undefined;

	const row = (
		<div className="flex items-center gap-1.5 min-w-0">
			{inFlight ? (
				<Loader2 className="size-3 shrink-0 animate-spin text-muted-foreground" />
			) : null}
			<VerbDetail verb={verb} detail={detail} error={isError} />
		</div>
	);

	if (!hasDetail) return row;

	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="text-left hover:bg-muted/40 rounded-sm -mx-1 px-1 transition-colors"
				>
					{row}
				</button>
			</PopoverTrigger>
			<PopoverContent
				side="left"
				align="start"
				className="w-[420px] max-h-[70vh] overflow-auto p-3 space-y-2"
			>
				<ToolDetail tool={tool} />
			</PopoverContent>
		</Popover>
	);
}

function ToolDetail({ tool }: { tool: ToolView }) {
	const imageOutput = extractImageOutput(tool.output);
	return (
		<>
			<div className="flex items-center justify-between gap-2 border-b border-border/50 pb-1.5">
				<span className="text-xs font-mono text-foreground/90">
					{tool.name}
				</span>
				<span
					className={
						"text-[10px] uppercase tracking-wide " +
						(tool.state === "error"
							? "text-destructive"
							: tool.state === "done"
								? "text-muted-foreground"
								: "text-muted-foreground/70")
					}
				>
					{tool.state}
				</span>
			</div>

			<DetailSection label="Input">
				<JsonBlock value={tool.input} />
			</DetailSection>

			{tool.error ? (
				<DetailSection label="Error">
					<div className="text-[11px] text-destructive whitespace-pre-wrap break-words font-mono">
						{tool.error}
					</div>
				</DetailSection>
			) : null}

			{imageOutput ? (
				<DetailSection
					label={`Image · ${imageOutput.width}×${imageOutput.height}`}
				>
					<img
						src={`data:image/png;base64,${imageOutput.base64}`}
						alt="Tool output preview"
						className="w-full rounded border border-border/50 [image-rendering:pixelated]"
					/>
				</DetailSection>
			) : null}

			{tool.output !== undefined ? (
				<DetailSection label="Output">
					<JsonBlock value={tool.output} stripBase64 />
				</DetailSection>
			) : null}
		</>
	);
}

function DetailSection({
	label,
	children,
}: {
	label: string;
	children: React.ReactNode;
}) {
	return (
		<div className="space-y-1">
			<div className="text-[10px] uppercase tracking-wide text-muted-foreground/70">
				{label}
			</div>
			{children}
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
		<pre className="text-[10.5px] font-mono leading-snug bg-muted/40 rounded p-2 overflow-auto max-h-64 whitespace-pre-wrap break-words">
			{text}
		</pre>
	);
}

function extractImageOutput(
	output: unknown,
): { base64: string; width: number; height: number } | null {
	if (!output || typeof output !== "object") return null;
	const o = output as Record<string, unknown>;
	if (typeof o.base64 !== "string") return null;
	const width = typeof o.width === "number" ? o.width : 0;
	const height = typeof o.height === "number" ? o.height : 0;
	return { base64: o.base64, width, height };
}

function redactBase64(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(redactBase64);
	if (value && typeof value === "object") {
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
			if (k === "base64" && typeof v === "string") {
				out[k] = `<${v.length} bytes>`;
			} else {
				out[k] = redactBase64(v);
			}
		}
		return out;
	}
	return value;
}
