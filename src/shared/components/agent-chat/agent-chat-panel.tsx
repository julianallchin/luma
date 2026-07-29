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
	footerStatus?: ReactNode;
}) {
	const session = chat.useSession(subjectKey, threadInit);
	const { messages, streaming, error, send, stop, reset } = session;
	// The prompt is live only once the editor is ready *and* the thread is
	// hydrated — sending before that would race the history.
	const canSend = ready && session.ready;
	const [draft, setDraft] = useState("");
	const scrollRef = useRef<HTMLDivElement>(null);

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
		await send(text);
	};

	return (
		<div className="flex-1 flex flex-col min-h-0">
			<div
				ref={scrollRef}
				onScroll={(e) => {
					const el = e.currentTarget;
					stuckToBottomRef.current =
						el.scrollHeight - el.scrollTop - el.clientHeight < 40;
				}}
				className="flex-1 overflow-y-auto p-3 space-y-3"
			>
				{messages.length === 0 ? (
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

			<div className="p-3">
				<PromptInput
					onSubmit={handleSubmit}
					className="[&_[data-slot=input-group]]:bg-input [&_[data-slot=input-group]]:dark:bg-input [&_[data-slot=input-group]]:border-border"
				>
					<PromptInputTextarea
						value={draft}
						onChange={(e) => setDraft(e.target.value)}
						placeholder={placeholder}
						disabled={!canSend}
					/>
					<PromptInputFooter>
						<span className="text-[10px] text-muted-foreground/70">
							{footerStatus}
						</span>
						<PromptInputTools>
							{messages.length > 0 && (
								<PromptInputButton
									onClick={() => void reset()}
									aria-label="Reset conversation"
								>
									<Eraser className="size-3.5" />
								</PromptInputButton>
							)}
							<PromptInputSubmit
								status={status}
								onStop={stop}
								disabled={!canSend || (!streaming && !draft.trim())}
							/>
						</PromptInputTools>
					</PromptInputFooter>
				</PromptInput>
			</div>
		</div>
	);
}
