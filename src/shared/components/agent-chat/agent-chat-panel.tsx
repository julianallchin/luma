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
import { AgentConversation } from "./conversation";
import type { AgentChat } from "./create-agent-chat";

/** Generic chat surface shared by every agent: scrollback + conversation +
 * prompt. Feature-specific chrome (header, empty state, footer status) is
 * passed in — the *look* of the conversation itself is identical everywhere. */
export function AgentChatPanel<Bridge>({
	chat,
	sessionKey,
	ready,
	placeholder,
	empty,
	footerStatus,
}: {
	chat: AgentChat<Bridge>;
	sessionKey: string | null;
	ready: boolean;
	placeholder: string;
	empty?: ReactNode;
	footerStatus?: ReactNode;
}) {
	const { messages, streaming, error, send, stop, reset } =
		chat.useSession(sessionKey);
	const [draft, setDraft] = useState("");
	const scrollRef = useRef<HTMLDivElement>(null);

	useEffect(() => {
		const el = scrollRef.current;
		if (!el) return;
		el.scrollTop = el.scrollHeight;
	}, [messages]);

	const status: ChatStatus = streaming ? "streaming" : "ready";

	const handleSubmit = async (message: PromptInputMessage) => {
		const text = message.text.trim();
		if (!text || streaming || !ready) return;
		setDraft("");
		await send(text);
	};

	return (
		<div className="flex-1 flex flex-col min-h-0">
			<div ref={scrollRef} className="flex-1 overflow-y-auto p-3 space-y-3">
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
						disabled={!ready}
					/>
					<PromptInputFooter>
						<span className="text-[10px] text-muted-foreground/70">
							{footerStatus}
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
								onStop={stop}
								disabled={!ready || (!streaming && !draft.trim())}
							/>
						</PromptInputTools>
					</PromptInputFooter>
				</PromptInput>
			</div>
		</div>
	);
}
