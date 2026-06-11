import { History, Sparkles } from "lucide-react";
import { useState } from "react";
import {
	setOpenRouterKey,
	useOpenRouterKey,
} from "@/features/track-editor/agent/openrouter-key";
import { AgentChatPanel } from "@/shared/components/agent-chat/agent-chat-panel";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	type GraphCheckpoint,
	graphAgent,
	useGraphSnapshots,
} from "../agent/graph-agent";

// Stable empty reference so the zustand selector doesn't return a fresh array
// every render (which would loop: new value → re-render → new value → …).
const NO_CHECKPOINTS: GraphCheckpoint[] = [];

/** The Agent tab of the pattern editor. The bridge is registered separately by
 * PatternEditor; here we just render the shared chat + a checkpoint list that
 * reverts the canvas to any of the agent's turns. */
export function GraphAgentPanel({
	patternId,
	ready,
}: {
	patternId: string;
	ready: boolean;
}) {
	const apiKey = useOpenRouterKey();

	if (!apiKey) return <ApiKeyPrompt />;

	return (
		<div className="flex flex-col min-h-0 h-full bg-background">
			<div className="flex items-center justify-between gap-2 px-3 py-1.5 bg-trim shrink-0">
				<div className="flex items-center gap-1.5">
					<Sparkles className="size-3 text-muted-foreground" />
					<span className="text-[9px] font-bold uppercase tracking-wider text-muted-foreground">
						Graph Agent
					</span>
				</div>
				<Checkpoints patternId={patternId} />
			</div>
			<AgentChatPanel
				chat={graphAgent}
				sessionKey={patternId}
				ready={ready}
				placeholder={ready ? "Ask the agent to build…" : "Loading editor…"}
				empty={<EmptyState />}
			/>
		</div>
	);
}

function Checkpoints({ patternId }: { patternId: string }) {
	const checkpoints = useGraphSnapshots(
		(s) => s.byPattern[patternId] ?? NO_CHECKPOINTS,
	);
	const [open, setOpen] = useState(false);
	if (checkpoints.length === 0) return null;

	const revert = (graph: Parameters<typeof structuredClone>[0]) => {
		const bridge = graphAgent.getBridge(patternId);
		// biome-ignore lint/suspicious/noExplicitAny: Graph type round-trips fine.
		bridge?.apply(graph as any);
		setOpen(false);
	};

	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="flex items-center gap-1 text-[9px] font-bold uppercase tracking-wider text-muted-foreground hover:text-foreground transition-colors"
				>
					<History className="size-3" />
					{checkpoints.length}
				</button>
			</PopoverTrigger>
			<PopoverContent align="end" className="w-56 p-1">
				<div className="px-2 py-1 text-[9px] uppercase tracking-wider text-muted-foreground/70">
					Revert canvas to
				</div>
				{[...checkpoints].reverse().map((c) => (
					<button
						key={c.id}
						type="button"
						onClick={() => revert(c.graph)}
						className="flex w-full items-center justify-between gap-2 px-2 py-1.5 text-xs text-left hover:bg-hover rounded-none transition-colors"
					>
						<span className="truncate">{c.label}</span>
						<span className="text-[10px] text-muted-foreground/60">
							{c.graph.nodes.length}n
						</span>
					</button>
				))}
			</PopoverContent>
		</Popover>
	);
}

function EmptyState() {
	return (
		<div className="flex flex-col items-center justify-center text-center text-xs text-muted-foreground gap-1 pt-6">
			<Sparkles className="size-4" />
			<div className="font-medium text-foreground/80">Graph agent</div>
			<div className="max-w-[18rem]">
				Ask me to build or modify this pattern's node graph. I edit live, run it
				to check for errors, and inspect the output signals to verify.
			</div>
		</div>
	);
}

function ApiKeyPrompt() {
	const [value, setValue] = useState("");
	const save = () => {
		if (value.trim()) setOpenRouterKey(value);
	};
	return (
		<div className="flex-1 flex flex-col min-h-0 bg-background">
			<div className="flex-1 p-4 flex items-center justify-center text-xs text-muted-foreground text-center">
				Add your OpenRouter API key to use the graph agent.
			</div>
			<div className="border-t border-gutter p-3 space-y-2">
				<Input
					type="password"
					value={value}
					onChange={(e) => setValue(e.target.value)}
					placeholder="sk-or-..."
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
						href="https://openrouter.ai/keys"
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
