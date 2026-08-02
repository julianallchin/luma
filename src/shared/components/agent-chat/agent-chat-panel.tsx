import type { ChatStatus } from "ai";
import { Eraser } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import {
	PromptInput,
	PromptInputButton,
	PromptInputFooter,
	type PromptInputMessage,
	PromptInputSubmit,
	PromptInputTextarea,
	PromptInputTools,
} from "@/shared/components/ai-elements/prompt-input";
import type { ThreadInit } from "@/shared/lib/agent/threads";
import { AgentConversation } from "./conversation";
import type { AgentChat } from "./create-agent-chat";

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
	 * it is resolved on first render. */
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
	const session = chat.useSession(subjectKey, threadInit);
	const { messages, streaming, error, send, stop, reset } = session;
	// The prompt is live only once the editor is ready *and* the thread is
	// hydrated — sending before that would race the history.
	const canSend = ready && session.ready;
	const [draft, setDraft] = useState("");
	const scrollRef = useRef<HTMLDivElement>(null);
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const isEmpty = messages.length === 0;

	// Stick to the bottom only while the user is already there. Once they scroll
	// up, stop following (so they can read back mid-stream); re-engage when they
	// return to the bottom.
	const stuckToBottomRef = useRef(true);
	useEffect(() => {
		const el = scrollRef.current;
		if (!el || !stuckToBottomRef.current) return;
		el.scrollTop = el.scrollHeight;
	}, [messages]);

	const status: ChatStatus = streaming ? "streaming" : "ready";

	const handleSubmit = async (message: PromptInputMessage) => {
		const text = message.text.trim();
		if (!text || streaming || !canSend) return;
		setDraft("");
		requestAnimationFrame(() => textareaRef.current?.focus());
		await send(text);
	};

	const handleReset = async () => {
		await reset();
		requestAnimationFrame(() => textareaRef.current?.focus());
	};

	return (
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
						placeholder={placeholder}
						disabled={!canSend}
						className="min-h-14 px-3.5 pt-3 pb-2 text-[13px] leading-relaxed text-foreground/90 placeholder:text-foreground/45"
					/>
					<PromptInputFooter className="px-2.5 pt-0 pb-2">
						<span className="text-[10px] font-normal text-muted-foreground/90">
							{footerStatus}
						</span>
						<PromptInputTools>
							{messages.length > 0 && (
								<PromptInputButton
									onClick={() => void handleReset()}
									aria-label="Reset conversation"
									title="Reset conversation"
									className="size-7 rounded-[5px] border-0 bg-transparent p-0 text-muted-foreground hover:bg-hover/70 hover:text-foreground"
								>
									<Eraser className="size-3.5" />
								</PromptInputButton>
							)}
							<PromptInputSubmit
								status={status}
								onStop={stop}
								disabled={!canSend || (!streaming && !draft.trim())}
								className="size-7 rounded-[5px] border-0 bg-primary p-0 text-primary-foreground hover:bg-primary/85 disabled:bg-transparent disabled:text-muted-foreground/80"
							/>
						</PromptInputTools>
					</PromptInputFooter>
				</PromptInput>
			</div>
		</div>
	);
}
