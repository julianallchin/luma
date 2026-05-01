import {
	ChevronDown,
	ChevronRight,
	Eraser,
	Loader2,
	Send,
	Sparkles,
	Square,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Streamdown } from "streamdown";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import type { BarClassificationsPayload } from "../agent/build-context";
import {
	OPENROUTER_MODEL,
	setOpenRouterKey,
	useOpenRouterKey,
} from "../agent/openrouter-key";
import {
	type ChatMessage,
	type ChatPart,
	type ChatToolPart,
	type ToolPart,
	useChatAgent,
} from "../agent/use-chat-agent";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import {
	useBarClassifications,
	useClassifierThresholds,
} from "./hooks/use-bar-classifications";

export function ChatSidebar() {
	const apiKey = useOpenRouterKey();
	const trackId = useTrackEditorStore((s) => s.trackId);
	const venueName = useAppViewStore((s) => s.currentVenue?.name ?? null);
	const barTags = useBarClassifications(trackId);
	const tagThresholds = useClassifierThresholds();

	return (
		<div className="w-80 border-l border-border bg-background/50 flex flex-col min-h-0">
			<div className="p-3 border-b border-border/50 flex items-center justify-between">
				<div className="flex items-center gap-2">
					<Sparkles className="size-3.5 text-muted-foreground" />
					<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
						Copilot
					</h2>
				</div>
				<span className="text-[10px] uppercase tracking-wide text-muted-foreground/70">
					{shortModelLabel(OPENROUTER_MODEL)}
				</span>
			</div>
			{apiKey ? (
				<ChatPanel
					barClassifications={barTags}
					tagThresholds={tagThresholds}
					venueName={venueName}
				/>
			) : (
				<ApiKeyPrompt />
			)}
		</div>
	);
}

function shortModelLabel(model: string): string {
	const slash = model.indexOf("/");
	return slash >= 0 ? model.slice(slash + 1) : model;
}

function ApiKeyPrompt() {
	const [value, setValue] = useState("");

	const handleSave = () => {
		if (!value.trim()) return;
		setOpenRouterKey(value);
	};

	return (
		<div className="flex-1 flex flex-col min-h-0">
			<div className="flex-1 p-4 flex items-center justify-center text-xs text-muted-foreground text-center">
				Add your OpenRouter API key below to start using the copilot.
			</div>
			<div className="border-t border-border/50 p-3 space-y-2">
				<label
					htmlFor="openrouter-key-sidebar"
					className="text-xs font-medium text-muted-foreground"
				>
					OpenRouter API Key
				</label>
				<Input
					id="openrouter-key-sidebar"
					type="password"
					value={value}
					onChange={(e) => setValue(e.target.value)}
					placeholder="sk-or-..."
					autoComplete="off"
					spellCheck={false}
					onKeyDown={(e) => {
						if (e.key === "Enter") {
							e.preventDefault();
							handleSave();
						}
					}}
				/>
				<div className="flex items-center justify-between gap-2">
					<a
						href="https://openrouter.ai/keys"
						target="_blank"
						rel="noreferrer"
						className="text-[11px] text-muted-foreground hover:text-foreground underline"
					>
						Get a key →
					</a>
					<Button size="sm" onClick={handleSave} disabled={!value.trim()}>
						Save
					</Button>
				</div>
			</div>
		</div>
	);
}

type ChatPanelProps = {
	barClassifications: BarClassificationsPayload | null;
	tagThresholds: Record<string, number>;
	venueName: string | null;
};

function ChatPanel({
	barClassifications,
	tagThresholds,
	venueName,
}: ChatPanelProps) {
	const trackId = useTrackEditorStore((s) => s.trackId);
	const { messages, streaming, error, send, abort, reset } = useChatAgent();
	const [draft, setDraft] = useState("");
	const scrollRef = useRef<HTMLDivElement>(null);

	useEffect(() => {
		const el = scrollRef.current;
		if (!el) return;
		el.scrollTop = el.scrollHeight;
	}, [messages]);

	const trackReady = trackId !== null;

	const handleSubmit = async () => {
		if (!draft.trim() || streaming || !trackReady) return;
		const text = draft;
		setDraft("");
		await send({
			prompt: text,
			venueName,
			barClassifications,
			tagThresholds,
		});
	};

	return (
		<div className="flex-1 flex flex-col min-h-0">
			<div ref={scrollRef} className="flex-1 overflow-y-auto p-3 space-y-3">
				{messages.length === 0 ? (
					<EmptyState
						hasBarTags={
							!!barClassifications &&
							barClassifications.classifications.length > 0
						}
					/>
				) : (
					messages.map((m, i) => (
						<MessageBubble
							key={m.id}
							message={m}
							isStreaming={streaming && i === messages.length - 1}
						/>
					))
				)}
				{error && (
					<div className="rounded-md border border-destructive/30 bg-destructive/10 p-2 text-xs text-destructive">
						{error}
					</div>
				)}
			</div>

			<div className="border-t border-border/50 p-3 space-y-2">
				<div className="flex items-center gap-2">
					<Input
						value={draft}
						onChange={(e) => setDraft(e.target.value)}
						placeholder={
							trackReady ? "Ask the copilot…" : "Open a track to start"
						}
						disabled={!trackReady || streaming}
						onKeyDown={(e) => {
							if (e.key === "Enter" && !e.shiftKey) {
								e.preventDefault();
								void handleSubmit();
							}
						}}
					/>
					{streaming ? (
						<Button size="icon" variant="destructive" onClick={abort}>
							<Square className="size-4" />
						</Button>
					) : (
						<Button
							size="icon"
							onClick={() => void handleSubmit()}
							disabled={!trackReady || !draft.trim()}
						>
							<Send className="size-4" />
						</Button>
					)}
				</div>
				<div className="flex items-center justify-between text-[10px] text-muted-foreground/70">
					<span>
						{barClassifications
							? `${barClassifications.classifications.length} bar tags loaded`
							: "no bar tags"}
					</span>
					{messages.length > 0 && (
						<button
							type="button"
							onClick={reset}
							className="hover:text-foreground inline-flex items-center gap-1"
						>
							<Eraser className="size-3" /> reset
						</button>
					)}
				</div>
			</div>
		</div>
	);
}

function EmptyState({ hasBarTags }: { hasBarTags: boolean }) {
	return (
		<div className="flex flex-col items-center justify-center text-center text-xs text-muted-foreground gap-1 pt-6">
			<Sparkles className="size-4" />
			<div className="font-medium text-foreground/80">Lighting copilot</div>
			<div className="max-w-[18rem]">
				Ask me to analyze the track, suggest patterns, or place annotations.
				{!hasBarTags && (
					<>
						{" "}
						Bar tags aren't ready for this track yet — I'll work without them.
					</>
				)}
			</div>
		</div>
	);
}

function MessageBubble({
	message,
	isStreaming,
}: {
	message: ChatMessage;
	isStreaming: boolean;
}) {
	if (message.role === "user") {
		return (
			<div className="flex justify-end">
				<div className="max-w-[90%] rounded-2xl rounded-br-sm bg-primary/15 text-foreground px-2.5 py-1.5 text-xs whitespace-pre-wrap break-words leading-relaxed">
					{message.text}
				</div>
			</div>
		);
	}
	return <AssistantMessage parts={message.parts} isStreaming={isStreaming} />;
}

function AssistantMessage({
	parts,
	isStreaming,
}: {
	parts: ChatPart[];
	isStreaming: boolean;
}) {
	const segments = useMemo(() => groupAssistantParts(parts), [parts]);
	// Only the truly-last part of the message can be "still streaming". As
	// soon as anything new (tool call, text delta, another reasoning) lands,
	// the previous reasoning collapses to "Thought for Ns".
	const last = parts[parts.length - 1];
	const activeReasoningId =
		isStreaming && last?.kind === "reasoning" ? last.id : null;
	return (
		<div className="space-y-1.5">
			{segments.length === 0 ? (
				<div className="text-[11px] italic text-muted-foreground">…</div>
			) : (
				segments.map((seg, i) => {
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
							isStreaming={isStreaming}
							activeReasoningId={activeReasoningId}
						/>
					);
				})
			)}
		</div>
	);
}

type AssistantSegment =
	| { kind: "text"; part: Extract<ChatPart, { kind: "text" }> }
	| { kind: "run"; parts: ChatPart[] };

function groupAssistantParts(parts: ChatPart[]): AssistantSegment[] {
	// Models interleave text deltas with reasoning/tool events. We treat the
	// entire message as: one run of reasoning + tool calls, followed by the
	// concatenated text response. This keeps the response from fragmenting
	// when reasoning happens mid-stream.
	const segments: AssistantSegment[] = [];
	const runParts: ChatPart[] = [];
	let textBuf = "";
	let firstTextId: string | null = null;
	for (const p of parts) {
		if (p.kind === "text") {
			if (!firstTextId) firstTextId = p.id;
			textBuf += p.text;
		} else {
			runParts.push(p);
		}
	}
	if (runParts.length > 0) segments.push({ kind: "run", parts: runParts });
	if (textBuf.length > 0) {
		segments.push({
			kind: "text",
			part: { kind: "text", id: firstTextId ?? "combined", text: textBuf },
		});
	}
	return segments;
}

function runKey(parts: ChatPart[]): string {
	const first = parts[0];
	if (!first) return "empty";
	if (first.kind === "tool") return first.tool.id;
	return first.id;
}

function partKey(part: ChatPart, index: number): string {
	if (part.kind === "text") return `t-${part.id}-${index}`;
	if (part.kind === "reasoning") return `r-${part.id}-${index}`;
	return `tool-${part.tool.id}`;
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

function MarkdownText({ text }: { text: string }) {
	return (
		<Streamdown className={MARKDOWN_CLASSNAME}>
			{stripCodeMarks(text)}
		</Streamdown>
	);
}

/**
 * Strip fenced code blocks and inline backticks. Streamdown wraps fenced
 * code in a styled CodeBlock with copy/download buttons regardless of CSS,
 * and we want plain prose. Handles partial fences during streaming too.
 */
function stripCodeMarks(text: string): string {
	let out = text.replace(/```[a-zA-Z0-9_+-]*\n?([\s\S]*?)```/g, "$1");
	out = out.replace(/```[a-zA-Z0-9_+-]*\n?/g, "");
	out = out.replace(/`([^`\n]+)`/g, "$1");
	return out;
}

const TOOL_VERB: Record<string, { past: string; noun: string }> = {
	search_patterns: { past: "Searched", noun: "pattern" },
	read_pattern: { past: "Read", noun: "pattern" },
	place_annotation: { past: "Created", noun: "annotation" },
	update_annotation: { past: "Updated", noun: "annotation" },
	delete_annotation: { past: "Deleted", noun: "annotation" },
};

type ToolLabel = { verb: string; detail: string | null };

function formatToolLabel(
	tool: ToolPart,
	patternName: (id: string) => string | undefined,
): ToolLabel {
	const meta = TOOL_VERB[tool.name];
	const verb = meta?.past ?? tool.name;
	switch (tool.name) {
		case "search_patterns": {
			const input = tool.input as { query?: string } | undefined;
			const q = input?.query?.trim();
			return {
				verb,
				detail: q ? `"${q}" patterns` : "all patterns",
			};
		}
		case "read_pattern": {
			const input = tool.input as { patternId?: string } | undefined;
			const output = tool.output as { name?: string } | undefined;
			const name =
				output?.name ??
				(input?.patternId ? patternName(input.patternId) : undefined);
			return { verb, detail: name ?? null };
		}
		case "place_annotation": {
			const input = tool.input as { patternId?: string } | undefined;
			const name = input?.patternId ? patternName(input.patternId) : undefined;
			return { verb: "Created annotation", detail: name ?? null };
		}
		case "update_annotation":
			return { verb: "Updated annotation", detail: null };
		case "delete_annotation":
			return { verb: "Deleted annotation", detail: null };
		default:
			return { verb, detail: null };
	}
}

function summarizeRun(parts: ChatPart[]): Array<{
	verb: string;
	detail: string;
}> {
	const tools = parts.filter((p): p is ChatToolPart => p.kind === "tool");
	if (tools.length === 0) {
		const reasonings = parts.filter(
			(p): p is Extract<ChatPart, { kind: "reasoning" }> =>
				p.kind === "reasoning",
		);
		if (reasonings.length === 0) return [{ verb: "Thought", detail: "" }];
		const totalMs = reasonings.reduce(
			(sum, r) => sum + Math.max(0, r.lastDeltaAt - r.startedAt),
			0,
		);
		return [
			{ verb: "Thought", detail: `for ${formatReasoningDuration(totalMs)}` },
		];
	}
	const out: Array<{ verb: string; detail: string }> = [];
	const counts = new Map<string, number>();
	for (const t of tools) {
		counts.set(t.tool.name, (counts.get(t.tool.name) ?? 0) + 1);
	}
	for (const [name, count] of counts) {
		const meta = TOOL_VERB[name];
		const verbRaw = meta?.past ?? name;
		const verb = out.length === 0 ? verbRaw : lcFirst(verbRaw);
		if (meta) {
			const noun = count === 1 ? meta.noun : `${meta.noun}s`;
			out.push({ verb, detail: `${count} ${noun}` });
		} else {
			out.push({ verb, detail: `×${count}` });
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
	parts: ChatPart[];
	isStreaming: boolean;
	activeReasoningId: string | null;
}) {
	const hasTools = parts.some((p) => p.kind === "tool");
	if (isStreaming)
		return <ToolRunLive parts={parts} activeReasoningId={activeReasoningId} />;
	if (hasTools) return <ToolRunCollapsed parts={parts} />;
	return <ToolRunLive parts={parts} activeReasoningId={null} />;
}

function ToolRunCollapsed({ parts }: { parts: ChatPart[] }) {
	const [open, setOpen] = useState(false);
	const phrases = useMemo(() => summarizeRun(parts), [parts]);
	const caretClass =
		"size-3 shrink-0 text-muted-foreground/60 transition-opacity " +
		(open ? "opacity-100" : "opacity-0 group-hover:opacity-100");

	return (
		<div className="space-y-1">
			<button
				type="button"
				onClick={() => setOpen((o) => !o)}
				className="group inline-flex max-w-full items-center gap-1.5 text-left"
			>
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
			{open && (
				<div className="space-y-1">
					{parts.map((p, i) => (
						<RunLineView
							key={partKey(p, i)}
							part={p}
							activeReasoningId={null}
						/>
					))}
				</div>
			)}
		</div>
	);
}

function ToolRunLive({
	parts,
	activeReasoningId,
}: {
	parts: ChatPart[];
	activeReasoningId: string | null;
}) {
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

function RunLineView({
	part,
	activeReasoningId,
}: {
	part: ChatPart;
	activeReasoningId: string | null;
}) {
	if (part.kind === "text") return <MarkdownText text={part.text} />;
	if (part.kind === "reasoning") {
		const done = part.id !== activeReasoningId;
		return <ReasoningLine part={part} done={done} />;
	}
	return <ToolLine tool={part.tool} />;
}

function ReasoningTrace({ text }: { text: string }) {
	if (!text.trim()) return null;
	return (
		<div className="text-xs italic text-muted-foreground whitespace-pre-wrap break-words leading-relaxed">
			{text}
		</div>
	);
}

function ReasoningLine({
	part,
	done,
}: {
	part: Extract<ChatPart, { kind: "reasoning" }>;
	done: boolean;
}) {
	const [open, setOpen] = useState(false);
	if (!part.text.trim() && !done) return null;
	if (!done) return <ReasoningTrace text={part.text} />;

	const ms = Math.max(0, part.lastDeltaAt - part.startedAt);
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
					detail={`for ${formatReasoningDuration(ms)}`}
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

function ToolLine({ tool }: { tool: ToolPart }) {
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternName = (id: string) => patterns.find((p) => p.id === id)?.name;
	const inFlight =
		tool.state === "input-streaming" || tool.state === "executing";
	const isError = tool.state === "error";
	const { verb, detail } = formatToolLabel(tool, patternName);
	return (
		<div className="flex items-center gap-1.5 min-w-0">
			{inFlight ? (
				<Loader2 className="size-3 shrink-0 animate-spin text-muted-foreground" />
			) : null}
			<VerbDetail verb={verb} detail={detail} error={isError} />
		</div>
	);
}
