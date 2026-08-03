import { useEffect, useRef, useState } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { AgentChatPanel } from "@/shared/components/agent-chat/agent-chat-panel";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
	AGENT_PROVIDER_LABELS,
	setAgentApiKey,
	useAgentApiKey,
} from "../agent/openrouter-key";
import { trackAgent } from "../agent/track-agent";
import { useTrackAgentBridge } from "../agent/use-track-agent";
import { useTrackEditorStore } from "../stores/use-track-editor-store";

export function ChatSidebar() {
	const { key: apiKey } = useAgentApiKey();
	const trackId = useTrackEditorStore((s) => s.trackId);
	const [width, setWidth] = useState(DEFAULT_SIDEBAR_WIDTH);
	const drag = useRef<{ startX: number; startWidth: number } | null>(null);

	useEffect(
		() => () => {
			document.body.style.cursor = "";
			document.body.style.userSelect = "";
		},
		[],
	);

	return (
		<div
			className="relative shrink-0 border-l border-trim bg-background flex flex-col min-h-0"
			style={{ width }}
		>
			<hr
				aria-label="Resize agent sidebar"
				aria-orientation="vertical"
				aria-valuemin={MIN_SIDEBAR_WIDTH}
				aria-valuemax={MAX_SIDEBAR_WIDTH}
				aria-valuenow={width}
				tabIndex={0}
				onDoubleClick={() => {
					setWidth(DEFAULT_SIDEBAR_WIDTH);
				}}
				onKeyDown={(event) => {
					if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
					event.preventDefault();
					const delta = event.key === "ArrowLeft" ? 16 : -16;
					setWidth(clampSidebarWidth(width + delta));
				}}
				onPointerDown={(event) => {
					drag.current = { startX: event.clientX, startWidth: width };
					event.currentTarget.setPointerCapture(event.pointerId);
					document.body.style.cursor = "col-resize";
					document.body.style.userSelect = "none";
				}}
				onPointerMove={(event) => {
					if (!drag.current) return;
					const nextWidth = clampSidebarWidth(
						drag.current.startWidth + drag.current.startX - event.clientX,
					);
					setWidth(nextWidth);
				}}
				onPointerUp={(event) => {
					if (drag.current) {
						setWidth(
							clampSidebarWidth(
								drag.current.startWidth + drag.current.startX - event.clientX,
							),
						);
					}
					drag.current = null;
					event.currentTarget.releasePointerCapture(event.pointerId);
					document.body.style.cursor = "";
					document.body.style.userSelect = "";
				}}
				onPointerCancel={() => {
					drag.current = null;
					document.body.style.cursor = "";
					document.body.style.userSelect = "";
				}}
				className="absolute inset-y-0 -left-1 z-20 m-0 h-auto w-2 cursor-col-resize touch-none border-0 bg-transparent outline-none before:absolute before:inset-y-0 before:left-1/2 before:w-px before:-translate-x-1/2 before:bg-transparent before:transition-colors hover:before:bg-primary/60 focus-visible:before:bg-primary/60 active:before:bg-primary"
			/>
			{apiKey ? <ChatPanel trackId={trackId} /> : <ApiKeyPrompt />}
		</div>
	);
}

const MIN_SIDEBAR_WIDTH = 280;
const DEFAULT_SIDEBAR_WIDTH = 320;
const MAX_SIDEBAR_WIDTH = 640;

function clampSidebarWidth(width: number): number {
	return Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, width));
}

function ApiKeyPrompt() {
	const { provider } = useAgentApiKey();
	const providerLabel = AGENT_PROVIDER_LABELS[provider];
	const [value, setValue] = useState("");

	const handleSave = () => {
		if (!value.trim()) return;
		setAgentApiKey(value);
	};

	return (
		<div className="flex-1 flex flex-col min-h-0">
			<div className="flex-1 p-4 flex items-center justify-center text-xs text-muted-foreground text-center">
				Add your {providerLabel} API key below to start using Luma.
			</div>
			<div className="border-t border-border/50 p-3 space-y-2">
				<label
					htmlFor="openrouter-key-sidebar"
					className="text-xs font-medium text-muted-foreground"
				>
					{providerLabel} API Key
				</label>
				<Input
					id="openrouter-key-sidebar"
					type="password"
					value={value}
					onChange={(e) => setValue(e.target.value)}
					placeholder={provider === "openrouter" ? "sk-or-..." : "API key"}
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
};

function ChatPanel({ trackId }: ChatPanelProps) {
	const threadInit = useTrackAgentBridge(trackId);
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const venueId = useTrackEditorStore((s) => s.venueId);
	const scoreId = useTrackEditorStore((s) => s.scoreId);
	const scoreState = useTrackEditorStore((s) => s.scoreState);
	const beatGridLoading = useTrackEditorStore((s) => s.beatGridLoading);
	const waveformLoading = useTrackEditorStore((s) => s.waveformLoading);
	const annotationsLoading = useTrackEditorStore((s) => s.annotationsLoading);
	const patternsLoading = useTrackEditorStore((s) => s.patternsLoading);
	const ready = Boolean(
		trackId &&
			venueId &&
			scoreId &&
			currentVenueId === venueId &&
			scoreState === "loaded" &&
			!beatGridLoading &&
			!waveformLoading &&
			!annotationsLoading &&
			!patternsLoading,
	);

	return (
		<AgentChatPanel
			chat={trackAgent}
			subjectKey={trackId}
			threadInit={threadInit}
			ready={ready}
			placeholder={trackId ? "Ask Luma…" : "Open a track to start"}
			empty={<EmptyState />}
			centerEmpty
		/>
	);
}

function EmptyState() {
	return (
		<div className="mx-auto max-w-[18rem] text-center">
			<div className="text-xl font-normal leading-tight tracking-tight text-foreground">
				Ask about the song—or dream up its lights.
			</div>
		</div>
	);
}
