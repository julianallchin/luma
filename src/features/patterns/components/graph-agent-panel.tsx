import { Sparkles } from "lucide-react";
import { useState } from "react";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import {
	AGENT_PROVIDER_LABELS,
	setAgentApiKey,
	useAgentApiKey,
} from "@/features/track-editor/agent/openrouter-key";
import { AgentChatPanel } from "@/shared/components/agent-chat/agent-chat-panel";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import { graphAgent } from "../agent/graph-agent";

/** The Agent tab of the pattern editor. The bridge is registered separately by
 * PatternEditor; the shared chat owns both conversation and state history. */
export function GraphAgentPanel({
	patternId,
	implementationId,
	venueId,
	ready,
}: {
	patternId: string;
	implementationId: string | null;
	/** Stamped on the thread when this pattern's first thread is created. */
	venueId?: string | null;
	ready: boolean;
}) {
	const { key: apiKey } = useAgentApiKey();
	const principalId = useAuthStore((s) => s.user?.id ?? null);

	if (!apiKey) return <ApiKeyPrompt />;

	return (
		<div className="flex flex-col min-h-0 h-full bg-gutter">
			<div className="flex items-center gap-1.5 px-3 py-1.5 bg-trim shrink-0">
				<div className="flex items-center gap-1.5">
					<Sparkles className="size-3 text-muted-foreground" />
					<span className="text-[9px] font-bold uppercase tracking-wider text-muted-foreground">
						Graph Agent
					</span>
				</div>
			</div>
			<AgentChatPanel
				chat={graphAgent}
				subjectKey={patternId}
				threadInit={{
					principalId,
					implementationId,
					venueId: venueId ?? null,
				}}
				ready={ready}
				placeholder={ready ? "Ask the agent to build…" : "Loading editor…"}
				empty={<EmptyState />}
			/>
		</div>
	);
}

function EmptyState() {
	return (
		<div className="flex flex-col items-center justify-center text-center text-xs text-muted-foreground gap-1 pt-6">
			<Sparkles className="size-4" />
			<div className="font-medium text-foreground/80">Graph agent</div>
			<div className="max-w-[18rem]">
				Ask me to build or modify this pattern's node graph. I edit live, run it
				to check for errors, and measure the output in Python to verify.
			</div>
		</div>
	);
}

function ApiKeyPrompt() {
	const { provider } = useAgentApiKey();
	const providerLabel = AGENT_PROVIDER_LABELS[provider];
	const [value, setValue] = useState("");
	const save = () => {
		if (value.trim()) setAgentApiKey(value);
	};
	return (
		<div className="flex-1 flex flex-col min-h-0 bg-gutter">
			<div className="flex-1 p-4 flex items-center justify-center text-xs text-muted-foreground text-center">
				Add your {providerLabel} API key to use the graph agent.
			</div>
			<div className="border-t border-gutter p-3 space-y-2">
				<Input
					type="password"
					value={value}
					onChange={(e) => setValue(e.target.value)}
					placeholder={provider === "openrouter" ? "sk-or-..." : "API key"}
					autoComplete="off"
					spellCheck={false}
					onKeyDown={(e) => {
						if (e.key === "Enter") {
							e.preventDefault();
							save();
						}
					}}
				/>
				<div className="flex items-center justify-between gap-2">
					<a
						href={
							provider === "openrouter"
								? "https://openrouter.ai/keys"
								: "https://vercel.com/docs/ai-gateway"
						}
						target="_blank"
						rel="noreferrer"
						className="text-[11px] text-muted-foreground hover:text-foreground underline"
					>
						Get a key →
					</a>
					<Button onClick={save} disabled={!value.trim()}>
						Save
					</Button>
				</div>
			</div>
		</div>
	);
}
