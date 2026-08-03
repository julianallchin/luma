import { ArrowLeft, Bot, X } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { cn } from "@/shared/lib/utils";
import { AgentConversation } from "./conversation";
import type { ToolVocab } from "./parts";
import { SubagentAvatar } from "./subagent-avatar";
import {
	isSubagentDone,
	lastSubagentText,
	type SubagentEntry,
	type SubagentState,
	subagentAction,
} from "./subagent-state";

const DETAIL = "text-muted-foreground/75";
const DONE_PAGE_SIZE = 10;

function timeAgo(timestamp: number): string {
	const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h`;
	return `${Math.floor(hours / 24)}d`;
}

function SectionLabel({ children }: { children: ReactNode }) {
	return (
		<div className={cn("px-2 pt-3 pb-1.5 text-xs font-medium", DETAIL)}>
			{children}
		</div>
	);
}

function EntryTitle({ entry }: { entry: SubagentEntry }) {
	const live = !isSubagentDone(entry.subagent);
	return (
		<span className="truncate">
			<strong
				className={cn(
					"font-medium text-muted-foreground",
					live && "agent-shimmer",
				)}
			>
				{entry.name ?? entry.type ?? "Agent"}
			</strong>
			{entry.name && entry.type ? (
				<span className={DETAIL}>, {entry.type}</span>
			) : null}
		</span>
	);
}

function EntryRow({
	entry,
	onSelect,
}: {
	entry: SubagentEntry;
	onSelect: (id: string) => void;
}) {
	const { subagent } = entry;
	const summary =
		lastSubagentText(subagent) ?? subagent.error ?? subagentAction(subagent);
	const timestamp =
		subagent.lastActivityAt ?? subagent.finishedAt ?? subagent.startedAt;
	return (
		<button
			type="button"
			onClick={() => onSelect(entry.id)}
			className="flex w-full items-start gap-2.5 rounded-lg px-2 py-1.5 text-left hover:bg-control"
		>
			<SubagentAvatar seed={entry.id} className="mt-px" />
			<div className="flex min-w-0 flex-1 flex-col gap-0.5">
				<span className="flex items-baseline gap-2">
					<span
						className={cn(
							"min-w-0 flex-1 truncate font-medium",
							subagent.status === "error"
								? "text-destructive"
								: "text-foreground",
						)}
					>
						{entry.name ?? entry.type ?? "Agent"}
					</span>
					<span className={cn("shrink-0 text-[11px] tabular-nums", DETAIL)}>
						{timeAgo(timestamp)}
					</span>
				</span>
				<span className={cn("truncate text-[13px]", DETAIL)}>{summary}</span>
			</div>
		</button>
	);
}

function SubagentList({
	entries,
	onSelect,
}: {
	entries: SubagentEntry[];
	onSelect: (id: string) => void;
}) {
	const [shown, setShown] = useState(DONE_PAGE_SIZE);
	const active = entries.filter((entry) => !isSubagentDone(entry.subagent));
	const done = entries.filter((entry) => isSubagentDone(entry.subagent));
	const recentDone = [...done].sort(
		(a, b) =>
			(b.subagent.lastActivityAt ?? b.subagent.finishedAt ?? 0) -
			(a.subagent.lastActivityAt ?? a.subagent.finishedAt ?? 0),
	);

	return (
		<div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-2">
			<SectionLabel>Active</SectionLabel>
			{active.length === 0 ? (
				<p className={cn("px-2 py-1 text-xs", DETAIL)}>No active subagents</p>
			) : (
				active.map((entry) => (
					<EntryRow key={entry.id} entry={entry} onSelect={onSelect} />
				))
			)}
			{done.length > 0 ? (
				<>
					<SectionLabel>Done · {done.length}</SectionLabel>
					{recentDone.slice(0, shown).map((entry) => (
						<EntryRow key={entry.id} entry={entry} onSelect={onSelect} />
					))}
					{recentDone.length > shown ? (
						<button
							type="button"
							onClick={() => setShown((value) => value + DONE_PAGE_SIZE)}
							className={cn(
								"w-full px-9.5 py-2 text-left text-xs hover:text-foreground",
								DETAIL,
							)}
						>
							Show {Math.min(DONE_PAGE_SIZE, recentDone.length - shown)} more
						</button>
					) : null}
				</>
			) : null}
		</div>
	);
}

function SubagentFeed({
	subagent,
	allSubagents,
	vocab,
}: {
	subagent: SubagentState;
	allSubagents: readonly SubagentState[];
	vocab: ToolVocab;
}) {
	const scrollRef = useRef<HTMLDivElement>(null);
	const stuckToBottomRef = useRef(true);

	useEffect(() => {
		const element = scrollRef.current;
		if (element && stuckToBottomRef.current) {
			element.scrollTop = element.scrollHeight;
		}
	}, [subagent]);

	return (
		<div
			ref={scrollRef}
			onScroll={(event) => {
				const element = event.currentTarget;
				stuckToBottomRef.current =
					element.scrollHeight - element.scrollTop - element.clientHeight < 40;
			}}
			className="min-h-0 flex-1 overflow-y-auto px-3 py-3"
		>
			<AgentConversation
				messages={subagent.messages}
				streaming={!isSubagentDone(subagent)}
				vocab={vocab}
				subagents={allSubagents}
				showUserMessages={false}
			/>
		</div>
	);
}

/** Foam-style list and drill-in surface. The outer chat owns whether this
 * surface is docked, overlaid, or hidden. */
export function SubagentsPane({
	entries,
	allSubagents,
	selectedId,
	onSelect,
	vocab,
	onClose,
}: {
	entries: SubagentEntry[];
	allSubagents: readonly SubagentState[];
	selectedId: string | null;
	onSelect: (id: string | null) => void;
	vocab: ToolVocab;
	onClose: () => void;
}) {
	const selected = entries.find((entry) => entry.id === selectedId);
	return (
		<div className="flex h-full min-h-0 flex-col overflow-hidden">
			{selected ? (
				<div className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
					<button
						type="button"
						onClick={() => onSelect(null)}
						aria-label="Back to subagents"
						className={cn("rounded p-1 hover:text-foreground", DETAIL)}
					>
						<ArrowLeft className="size-4" />
					</button>
					<div className="flex min-w-0 flex-1 items-center gap-2">
						<SubagentAvatar seed={selected.id} className="size-4" />
						<EntryTitle entry={selected} />
					</div>
					<button
						type="button"
						onClick={onClose}
						aria-label="Close subagents"
						className={cn("rounded p-1 hover:text-foreground", DETAIL)}
					>
						<X className="size-4" />
					</button>
				</div>
			) : (
				<div className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3 text-xs font-medium text-foreground/90">
					<Bot className="size-4 text-muted-foreground" />
					<span className="min-w-0 flex-1">Subagents</span>
					<button
						type="button"
						onClick={onClose}
						aria-label="Close subagents"
						className={cn("rounded p-1 hover:text-foreground", DETAIL)}
					>
						<X className="size-4" />
					</button>
				</div>
			)}
			{selected ? (
				<SubagentFeed
					key={selected.id}
					subagent={selected.subagent}
					allSubagents={allSubagents}
					vocab={vocab}
				/>
			) : (
				<SubagentList entries={entries} onSelect={onSelect} />
			)}
		</div>
	);
}
