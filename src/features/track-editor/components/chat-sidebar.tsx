import type { ChatStatus } from "ai";
import { Eraser, Sparkles } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { AgentConversation } from "@/shared/components/agent-chat/conversation";
import type { ToolView, ToolVocab } from "@/shared/components/agent-chat/parts";
import {
	PromptInput,
	PromptInputButton,
	PromptInputFooter,
	type PromptInputMessage,
	PromptInputSubmit,
	PromptInputTextarea,
	PromptInputTools,
} from "@/shared/components/ai-elements/prompt-input";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import type { BarClassificationsPayload } from "../agent/build-context";
import {
	OPENROUTER_MODEL,
	setOpenRouterKey,
	useOpenRouterKey,
} from "../agent/openrouter-key";
import { toUIMessages } from "../agent/to-ui-messages";
import { useChatSession } from "../agent/use-chat-agent";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import {
	useBarClassifications,
	useClassifierThresholds,
	useDrumOnsets,
} from "./hooks/use-bar-classifications";

export function ChatSidebar() {
	const apiKey = useOpenRouterKey();
	const trackId = useTrackEditorStore((s) => s.trackId);
	const barTags = useBarClassifications(trackId);
	const drumOnsets = useDrumOnsets(trackId);
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
					trackId={trackId}
					barClassifications={barTags}
					drumOnsets={drumOnsets}
					tagThresholds={tagThresholds}
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
					<Button onClick={handleSave} disabled={!value.trim()}>
						Save
					</Button>
				</div>
			</div>
		</div>
	);
}

type ChatPanelProps = {
	trackId: string | null;
	barClassifications: BarClassificationsPayload | null;
	drumOnsets: Record<string, number[]> | null;
	tagThresholds: Record<string, number>;
};

function ChatPanel({
	trackId,
	barClassifications,
	drumOnsets,
	tagThresholds,
}: ChatPanelProps) {
	const { messages, streaming, error, send, abort, reset } = useChatSession(
		trackId,
		{ barClassifications, drumOnsets, tagThresholds },
	);
	const [draft, setDraft] = useState("");
	const scrollRef = useRef<HTMLDivElement>(null);
	const patterns = useTrackEditorStore((s) => s.patterns);

	const uiMessages = useMemo(() => toUIMessages(messages), [messages]);
	const vocab = useMemo<ToolVocab>(() => {
		const patternName = (id: string) => patterns.find((p) => p.id === id)?.name;
		return {
			verbs: TOOL_VERB,
			formatLabel: (tool) => formatToolLabel(tool, patternName),
		};
	}, [patterns]);

	useEffect(() => {
		const el = scrollRef.current;
		if (!el) return;
		el.scrollTop = el.scrollHeight;
	}, [messages]);

	const trackReady = trackId !== null;
	const status: ChatStatus = streaming ? "streaming" : "ready";

	const handleSubmit = async (message: PromptInputMessage) => {
		const text = message.text.trim();
		if (!text || streaming || !trackReady) return;
		setDraft("");
		await send({ prompt: text });
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
					<AgentConversation
						messages={uiMessages}
						streaming={streaming}
						vocab={vocab}
					/>
				)}
				{error && (
					<div className="rounded-md border border-destructive/30 bg-destructive/10 p-2 text-xs text-destructive">
						{error}
					</div>
				)}
			</div>

			<div className="p-3">
				<PromptInput
					onSubmit={handleSubmit}
					className="[&_[data-slot=input-group]]:bg-input [&_[data-slot=input-group]]:dark:bg-input [&_[data-slot=input-group]]:border-border"
				>
					<PromptInputTextarea
						value={draft}
						onChange={(e) => setDraft(e.target.value)}
						placeholder={
							trackReady ? "Ask the copilot…" : "Open a track to start"
						}
						disabled={!trackReady}
					/>
					<PromptInputFooter>
						<span className="text-[10px] text-muted-foreground/70">
							{barClassifications
								? `${barClassifications.classifications.length} bar tags loaded`
								: "no bar tags"}
						</span>
						<PromptInputTools>
							{messages.length > 0 && (
								<PromptInputButton
									onClick={reset}
									aria-label="Reset conversation"
								>
									<Eraser className="size-3.5" />
								</PromptInputButton>
							)}
							<PromptInputSubmit
								status={status}
								onStop={abort}
								disabled={!trackReady || (!streaming && !draft.trim())}
							/>
						</PromptInputTools>
					</PromptInputFooter>
				</PromptInput>
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

// Track-agent tool vocabulary, consumed by the shared renderer.
const TOOL_VERB: Record<string, { past: string; noun: string | null }> = {
	search_patterns: { past: "Searched", noun: "pattern" },
	read_pattern: { past: "Read", noun: "pattern" },
	view_score: { past: "Viewed", noun: "score" },
	view_at: { past: "Viewed", noun: "moment" },
	preview_pattern: { past: "Previewed", noun: "pattern" },
	view_blended_result: { past: "Viewed blend", noun: "range" },
	place_clip: { past: "Placed", noun: "clip" },
	update_clip: { past: "Updated", noun: "clip" },
	restack_clip: { past: "Restacked", noun: "clip" },
	delete_clip: { past: "Deleted", noun: "clip" },
	// noun=null: verb already implies its object ("Asked venue").
	ask_venue: { past: "Asked venue", noun: null },
};

function formatToolLabel(
	tool: ToolView,
	patternName: (id: string) => string | undefined,
): { verb: string; detail: string | null } {
	const meta = TOOL_VERB[tool.name];
	const verb = meta?.past ?? tool.name;
	switch (tool.name) {
		case "search_patterns": {
			const input = tool.input as { query?: string } | undefined;
			const q = input?.query?.trim();
			return { verb, detail: q ? `"${q}" patterns` : "all patterns" };
		}
		case "read_pattern": {
			const input = tool.input as { patternId?: string } | undefined;
			const output = tool.output as { name?: string } | undefined;
			const name =
				output?.name ??
				(input?.patternId ? patternName(input.patternId) : undefined);
			return { verb, detail: name ?? null };
		}
		case "view_score": {
			const input = tool.input as
				| { startBar?: number; lastBar?: number; detail?: string }
				| undefined;
			if (input?.startBar !== undefined && input.lastBar !== undefined) {
				return {
					verb: "Viewed score",
					detail: `bars ${input.startBar}–${input.lastBar}`,
				};
			}
			return { verb: "Viewed score", detail: input?.detail ?? "summary" };
		}
		case "view_at": {
			const input = tool.input as { bar?: number } | undefined;
			return {
				verb: "Viewed stack",
				detail: input?.bar !== undefined ? `bar ${input.bar}` : null,
			};
		}
		case "preview_pattern": {
			const input = tool.input as
				| { patternId?: string; startBar?: number; lastBar?: number }
				| undefined;
			const name = input?.patternId ? patternName(input.patternId) : undefined;
			const range =
				input?.startBar !== undefined && input.lastBar !== undefined
					? `bars ${input.startBar}–${input.lastBar}`
					: null;
			const detail = [name, range].filter(Boolean).join(" · ") || null;
			return { verb: "Previewed pattern", detail };
		}
		case "view_blended_result": {
			const input = tool.input as
				| { startBar?: number; lastBar?: number }
				| undefined;
			return {
				verb: "Viewed blend",
				detail:
					input?.startBar !== undefined && input.lastBar !== undefined
						? `bars ${input.startBar}–${input.lastBar}`
						: null,
			};
		}
		case "place_clip": {
			const input = tool.input as { patternId?: string } | undefined;
			const name = input?.patternId ? patternName(input.patternId) : undefined;
			return { verb: "Placed clip", detail: name ?? null };
		}
		case "update_clip":
			return { verb: "Updated clip", detail: null };
		case "restack_clip":
			return { verb: "Restacked clip", detail: null };
		case "delete_clip":
			return { verb: "Deleted clip", detail: null };
		case "ask_venue": {
			const input = tool.input as { question?: string } | undefined;
			const q = input?.question?.trim();
			return { verb: "Asked venue", detail: q ? `"${truncate(q, 60)}"` : null };
		}
		default:
			return { verb, detail: null };
	}
}

function truncate(s: string, max: number): string {
	return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}
