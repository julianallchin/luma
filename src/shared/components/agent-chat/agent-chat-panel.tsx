import {
	Bot,
	Check,
	LoaderCircle,
	MessageSquareText,
	SquarePen,
} from "lucide-react";
import {
	type ReactNode,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import {
	PromptInput,
	PromptInputButton,
	PromptInputFooter,
	type PromptInputMessage,
	PromptInputSubmit,
	PromptInputTextarea,
	PromptInputTools,
} from "@/shared/components/ai-elements/prompt-input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import type { ThreadInit } from "@/shared/lib/agent/threads";
import { AuthoredStateHistory } from "./authored-state-history";
import { AgentConversation } from "./conversation";
import type { AgentChat } from "./create-agent-chat";
import { SubagentNavContext } from "./subagent-nav";
import { SubagentsPane } from "./subagent-panel";
import {
	collectSubagentEntries,
	mergeSubagentStates,
	type SubagentState,
} from "./subagent-state";

const THREAD_TIME = new Intl.DateTimeFormat(undefined, {
	month: "short",
	day: "numeric",
	hour: "numeric",
	minute: "2-digit",
});

function formatThreadTime(value: string): string {
	const date = new Date(value);
	return Number.isNaN(date.getTime()) ? value : THREAD_TIME.format(date);
}

/** Generic chat surface shared by every agent: scrollback + conversation +
 * prompt. Feature-specific chrome (header, empty state, footer status) is
 * passed in — the *look* of the conversation itself is identical everywhere. */
export function AgentChatPanel<Bridge>({
	chat,
	subjectKey,
	threadInit,
	ready,
	placeholder,
	empty,
	centerEmpty = false,
	footerStatus,
}: {
	chat: AgentChat<Bridge>;
	/** The thing being worked on (patternId, trackId). The durable thread for
	 * it is resolved once the editor and its immutable scope are ready. */
	subjectKey: string | null;
	/** Metadata stamped on the thread if this subject doesn't have one yet. */
	threadInit?: ThreadInit;
	ready: boolean;
	placeholder: string;
	empty?: ReactNode;
	/** Center the empty state and composer as one hero block. */
	centerEmpty?: boolean;
	footerStatus?: ReactNode;
}) {
	const session = chat.useSession(ready ? subjectKey : null, threadInit);
	const {
		messages,
		streaming,
		error,
		send,
		steer,
		stop,
		newChat,
		openChat,
		refreshChats,
		switching,
		restoring,
		threads,
		threadId,
		restoreRevision,
	} = session;
	// The prompt is live only once the editor is ready *and* the thread is
	// hydrated — sending before that would race the history.
	const canSend = ready && session.ready && !switching && !restoring;
	const [draft, setDraft] = useState("");
	const [historyOpen, setHistoryOpen] = useState(false);
	const [subagentsOpen, setSubagentsOpen] = useState(false);
	const [selectedSubagent, setSelectedSubagent] = useState<string | null>(null);
	const scrollRef = useRef<HTMLDivElement>(null);
	const surfaceRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const subagentsPaneRef = useRef<HTMLElement>(null);
	const subagentsTriggerRef = useRef<HTMLElement | null>(null);
	const seenSubagentsRef = useRef(new Set<string>());
	const isEmpty = messages.length === 0;
	const sessionSubagents =
		(session as typeof session & { subagents?: readonly SubagentState[] })
			.subagents ?? [];
	const subagents = useMemo(
		() => mergeSubagentStates(messages, sessionSubagents),
		[messages, sessionSubagents],
	);
	const subagentEntries = useMemo(
		() => collectSubagentEntries(messages, subagents),
		[messages, subagents],
	);
	const openSubagents = useCallback(
		(selectedId?: string) => {
			if (!subagentsOpen && document.activeElement instanceof HTMLElement) {
				subagentsTriggerRef.current = document.activeElement;
			}
			if (selectedId !== undefined) setSelectedSubagent(selectedId);
			setSubagentsOpen(true);
		},
		[subagentsOpen],
	);
	const closeSubagents = useCallback(() => {
		setSubagentsOpen(false);
		const trigger = subagentsTriggerRef.current;
		subagentsTriggerRef.current = null;
		requestAnimationFrame(() => {
			if (trigger?.isConnected) trigger.focus();
		});
	}, []);

	useEffect(() => {
		if (!subagentsOpen) return;
		const frame = requestAnimationFrame(() => {
			const pane = subagentsPaneRef.current;
			const firstControl = pane?.querySelector<HTMLElement>(
				'button:not(:disabled), [href], input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])',
			);
			(firstControl ?? pane)?.focus();
		});
		return () => cancelAnimationFrame(frame);
	}, [subagentsOpen]);

	useEffect(() => {
		const fresh = subagentEntries.filter(
			(entry) => !seenSubagentsRef.current.has(entry.id),
		);
		for (const entry of fresh) seenSubagentsRef.current.add(entry.id);
		if (
			fresh.length > 0 &&
			streaming &&
			(surfaceRef.current?.clientWidth ?? 0) >= 720
		) {
			openSubagents();
		}
	}, [streaming, subagentEntries, openSubagents]);

	useEffect(() => {
		setSelectedSubagent(null);
		closeSubagents();
	}, [threadId, closeSubagents]);

	// Stick to the bottom only while the user is already there. Once they scroll
	// up, stop following (so they can read back mid-stream); re-engage when they
	// return to the bottom.
	const stuckToBottomRef = useRef(true);
	useEffect(() => {
		const el = scrollRef.current;
		if (!el || !stuckToBottomRef.current) return;
		el.scrollTop = el.scrollHeight;
	}, [messages]);

	const hasDraft = draft.trim().length > 0;
	// Match Foam's composer: Stop is shown only while the active run has no
	// draft. Typing turns the control back into Send, which steers the Pi run.
	const submitStatus = streaming && !hasDraft ? "streaming" : "ready";

	const handleSubmit = async (message: PromptInputMessage) => {
		const text = message.text.trim();
		if (!text || !canSend) return;
		setDraft("");
		requestAnimationFrame(() => textareaRef.current?.focus());
		if (streaming) steer(text);
		else await send(text);
	};

	const handleNewChat = async () => {
		try {
			await newChat();
		} catch {
			// The shared session surfaces transition failures inline.
		} finally {
			requestAnimationFrame(() => textareaRef.current?.focus());
		}
	};
	const handleOpenChat = async (targetThreadId: string) => {
		setHistoryOpen(false);
		try {
			await openChat(targetThreadId);
		} catch {
			// The shared session surfaces transition failures inline.
		} finally {
			requestAnimationFrame(() => textareaRef.current?.focus());
		}
	};

	return (
		<SubagentNavContext.Provider value={openSubagents}>
			<div
				ref={surfaceRef}
				className="relative flex min-h-0 flex-1 overflow-hidden"
			>
				<div
					className={[
						"flex-1 flex flex-col min-h-0 bg-background",
						centerEmpty && isEmpty ? "justify-center" : "",
					].join(" ")}
				>
					<div
						ref={scrollRef}
						onScroll={(e) => {
							const el = e.currentTarget;
							stuckToBottomRef.current =
								el.scrollHeight - el.scrollTop - el.clientHeight < 40;
						}}
						className={[
							"p-3 space-y-3",
							centerEmpty && isEmpty
								? "shrink-0 overflow-visible"
								: "flex-1 overflow-y-auto",
						].join(" ")}
					>
						<div className="mx-auto w-full max-w-xl space-y-3">
							{isEmpty ? (
								(empty ?? null)
							) : (
								<AgentConversation
									messages={messages}
									streaming={streaming}
									vocab={chat.vocab}
									subagents={subagents}
								/>
							)}
							{error && (
								<div className="rounded-md border border-destructive/30 bg-destructive/10 p-2 text-xs text-destructive">
									{error}
								</div>
							)}
						</div>
					</div>

					<div className="mx-auto w-full max-w-xl px-3 pt-2 pb-3">
						<PromptInput
							onSubmit={handleSubmit}
							className={[
								"[&_[data-slot=input-group]]:rounded-[8px]",
								"[&_[data-slot=input-group]]:border-border/80",
								"[&_[data-slot=input-group]]:bg-control",
								"[&_[data-slot=input-group]]:dark:bg-control",
								"[&_[data-slot=input-group]:focus-within]:!border-muted-foreground/50",
								"[&_[data-slot=input-group]:focus-within]:!ring-1",
								"[&_[data-slot=input-group]:focus-within]:!ring-primary/25",
							].join(" ")}
						>
							<PromptInputTextarea
								ref={textareaRef}
								value={draft}
								onChange={(e) => setDraft(e.target.value)}
								onKeyDown={(event) => {
									if (event.key !== "Escape" || !streaming) return;
									event.preventDefault();
									stop();
								}}
								placeholder={
									streaming
										? "Steer the active turn… (Esc to stop)"
										: placeholder
								}
								disabled={!canSend}
								className="min-h-14 px-3.5 pt-3 pb-2 text-[13px] leading-relaxed text-foreground/90 placeholder:text-foreground/45"
							/>
							<PromptInputFooter className="px-2.5 pt-0 pb-2">
								<span className="text-[10px] font-normal text-muted-foreground/90">
									{footerStatus}
								</span>
								<PromptInputTools>
									<PromptInputButton
										type="button"
										onClick={() => {
											if (subagentsOpen) closeSubagents();
											else openSubagents();
										}}
										aria-label="Subagents"
										title="Subagents"
										aria-pressed={subagentsOpen}
										className="relative size-7 rounded-[5px] border-0 bg-transparent p-0 text-muted-foreground hover:bg-hover/70 hover:text-foreground"
									>
										<Bot className="size-3.5" />
										{subagentEntries.length > 0 ? (
											<span className="absolute -top-0.5 -right-0.5 min-w-3 rounded-full bg-primary px-0.5 text-center text-[8px] leading-3 text-primary-foreground tabular-nums">
												{subagentEntries.length}
											</span>
										) : null}
									</PromptInputButton>
									<AuthoredStateHistory
										threadId={threadId}
										disabled={
											!session.ready || streaming || switching || restoring
										}
										restoring={restoring}
										onRestore={restoreRevision}
									/>
									<Popover
										open={historyOpen}
										onOpenChange={(open) => {
											setHistoryOpen(open);
											if (open) void refreshChats().catch(() => undefined);
										}}
									>
										<PopoverTrigger asChild>
											<PromptInputButton
												aria-label="Conversation history"
												title="Conversation history"
												disabled={!session.ready || switching || restoring}
												className="size-7 rounded-[5px] border-0 bg-transparent p-0 text-muted-foreground hover:bg-hover/70 hover:text-foreground"
											>
												<MessageSquareText className="size-3.5" />
											</PromptInputButton>
										</PopoverTrigger>
										<PopoverContent align="end" side="top" className="w-64 p-1">
											<div className="px-2 py-1.5 text-[9px] font-bold uppercase tracking-wider text-muted-foreground/70">
												Conversations
											</div>
											{threads.length === 0 ? (
												<div className="px-2 py-2 text-xs text-muted-foreground">
													No saved conversations
												</div>
											) : (
												threads.map((thread) => (
													<button
														key={thread.id}
														type="button"
														onClick={() => void handleOpenChat(thread.id)}
														disabled={switching || restoring}
														className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left hover:bg-hover transition-colors disabled:pointer-events-none disabled:opacity-50"
													>
														<div className="min-w-0 flex-1">
															<div className="truncate text-xs text-foreground/90">
																{thread.title?.trim() ||
																	"Untitled conversation"}
															</div>
															<div className="text-[10px] text-muted-foreground/70">
																{formatThreadTime(thread.updatedAt)}
															</div>
														</div>
														{thread.id === threadId && (
															<Check className="size-3 shrink-0 text-primary" />
														)}
													</button>
												))
											)}
										</PopoverContent>
									</Popover>
									<PromptInputButton
										onClick={() => void handleNewChat()}
										aria-label="New conversation"
										title="New conversation"
										disabled={!session.ready || switching || restoring}
										className="size-7 rounded-[5px] border-0 bg-transparent p-0 text-muted-foreground hover:bg-hover/70 hover:text-foreground"
									>
										{switching || restoring ? (
											<LoaderCircle className="size-3.5 animate-spin" />
										) : (
											<SquarePen className="size-3.5" />
										)}
									</PromptInputButton>
									<PromptInputSubmit
										status={submitStatus}
										onStop={stop}
										disabled={!canSend || (!streaming && !hasDraft)}
										className="size-7 rounded-[5px] border-0 bg-primary p-0 text-primary-foreground hover:bg-primary/85 disabled:bg-transparent disabled:text-muted-foreground/80"
									/>
								</PromptInputTools>
							</PromptInputFooter>
						</PromptInput>
					</div>
				</div>
				{subagentsOpen ? (
					<aside
						ref={subagentsPaneRef}
						tabIndex={-1}
						aria-label="Subagents"
						onKeyDown={(event) => {
							if (event.key !== "Escape") return;
							event.preventDefault();
							event.stopPropagation();
							closeSubagents();
						}}
						className="absolute inset-y-0 right-0 z-30 w-[min(24rem,calc(100%-2rem))] border-l border-border bg-background shadow-2xl"
					>
						<SubagentsPane
							entries={subagentEntries}
							allSubagents={subagents}
							selectedId={selectedSubagent}
							onSelect={setSelectedSubagent}
							vocab={chat.vocab}
							onClose={closeSubagents}
						/>
					</aside>
				) : null}
			</div>
		</SubagentNavContext.Provider>
	);
}
