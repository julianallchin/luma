import { Sparkles } from "lucide-react";
import { useState } from "react";
import { AgentChatPanel } from "@/shared/components/agent-chat/agent-chat-panel";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import type { BarClassificationsPayload } from "../agent/build-context";
import {
	OPENROUTER_MODEL,
	setOpenRouterKey,
	useOpenRouterKey,
} from "../agent/openrouter-key";
import { trackAgent } from "../agent/track-agent";
import { useTrackAgentBridge } from "../agent/use-track-agent";
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
	const threadInit = useTrackAgentBridge(trackId, {
		barClassifications,
		drumOnsets,
		tagThresholds,
	});

	return (
		<AgentChatPanel
			chat={trackAgent}
			subjectKey={trackId}
			threadInit={threadInit}
			ready={trackId !== null}
			placeholder={trackId ? "Ask the copilot…" : "Open a track to start"}
			empty={
				<EmptyState
					hasBarTags={
						!!barClassifications &&
						barClassifications.classifications.length > 0
					}
				/>
			}
			footerStatus={
				barClassifications
					? `${barClassifications.classifications.length} bar tags loaded`
					: "no bar tags"
			}
		/>
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
