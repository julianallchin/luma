import { History, LoaderCircle, MessagesSquare, RotateCcw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
	AlertDialog,
	AlertDialogAction,
	AlertDialogCancel,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle,
} from "@/shared/components/ui/alert-dialog";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	type AuthoredHistoryEntry,
	type AuthoredHistoryKind,
	type AuthoredRestoreMode,
	listAuthoredHistory,
} from "@/shared/lib/agent/authored-state";

const REVISION_TIME = new Intl.DateTimeFormat(undefined, {
	month: "short",
	day: "numeric",
	hour: "numeric",
	minute: "2-digit",
});

function formatRevisionTime(value: string): string {
	const date = new Date(value);
	return Number.isNaN(date.getTime()) ? value : REVISION_TIME.format(date);
}

function kindLabel(kind: AuthoredHistoryKind): string {
	switch (kind) {
		case "initial_import":
			return "Initial state";
		case "edit":
			return "Edit";
		case "agent_turn":
			return "Agent turn";
		case "restore":
			return "Restore";
		case "pattern_fork":
			return "Forked pattern";
		case "workspace_merge":
			return "Merged agent work";
		case "sync_integration":
			return "Synced edit";
		case "revision":
			return "Revision";
	}
	return "Revision";
}

/** Authored document history is independent from transcript history. A state
 * restore always advances the document with a new revision; optional rewind
 * creates a new thread sharing an immutable transcript prefix. */
export function AuthoredStateHistory({
	threadId,
	disabled,
	restoring,
	onRestore,
}: {
	threadId: string | null;
	disabled: boolean;
	restoring: boolean;
	onRestore: (revisionId: string, mode: AuthoredRestoreMode) => Promise<void>;
}) {
	const [open, setOpen] = useState(false);
	const [entries, setEntries] = useState<AuthoredHistoryEntry[]>([]);
	const [nextCursor, setNextCursor] = useState<string | null>(null);
	const [loading, setLoading] = useState(false);
	const [loadingOlder, setLoadingOlder] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [pending, setPending] = useState<AuthoredHistoryEntry | null>(null);
	const requestId = useRef(0);

	const refresh = useCallback(async () => {
		if (!threadId) {
			setEntries([]);
			return;
		}
		const request = ++requestId.current;
		setLoading(true);
		setLoadingOlder(false);
		setError(null);
		try {
			const page = await listAuthoredHistory(threadId);
			if (request === requestId.current) {
				setEntries(page.entries);
				setNextCursor(page.nextCursor);
			}
		} catch (cause) {
			if (request === requestId.current) {
				setError(cause instanceof Error ? cause.message : String(cause));
			}
		} finally {
			if (request === requestId.current) setLoading(false);
		}
	}, [threadId]);

	const loadOlder = useCallback(async () => {
		if (!threadId || !nextCursor || loading || loadingOlder) return;
		const request = requestId.current;
		setLoadingOlder(true);
		setError(null);
		try {
			const page = await listAuthoredHistory(threadId, nextCursor);
			if (request === requestId.current) {
				setEntries((current) => {
					const known = new Set(current.map((entry) => entry.revisionId));
					return [
						...current,
						...page.entries.filter((entry) => !known.has(entry.revisionId)),
					];
				});
				setNextCursor(page.nextCursor);
			}
		} catch (cause) {
			if (request === requestId.current) {
				setError(cause instanceof Error ? cause.message : String(cause));
			}
		} finally {
			if (request === requestId.current) setLoadingOlder(false);
		}
	}, [loading, loadingOlder, nextCursor, threadId]);

	useEffect(() => {
		requestId.current += 1;
		setEntries([]);
		setNextCursor(null);
		setLoadingOlder(false);
		setError(null);
		setPending(null);
		setOpen(false);
	}, [threadId]);

	useEffect(() => {
		if (open) void refresh();
	}, [open, refresh]);

	const confirmRestore = async (
		event: React.MouseEvent<HTMLButtonElement>,
		mode: AuthoredRestoreMode,
	) => {
		event.preventDefault();
		if (!pending || disabled || restoring) return;
		setError(null);
		try {
			await onRestore(pending.revisionId, mode);
			setPending(null);
			await refresh();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : String(cause));
		}
	};

	return (
		<>
			<Popover open={open} onOpenChange={setOpen}>
				<PopoverTrigger asChild>
					<button
						type="button"
						aria-label="State history"
						title="State history"
						disabled={disabled}
						className="flex size-7 items-center justify-center rounded-[5px] text-muted-foreground transition-colors hover:bg-hover/70 hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
					>
						{restoring ? (
							<LoaderCircle className="size-3.5 animate-spin" />
						) : (
							<History className="size-3.5" />
						)}
					</button>
				</PopoverTrigger>
				<PopoverContent align="end" side="top" className="w-72 p-1">
					<div className="flex items-center justify-between px-2 py-1.5">
						<div className="text-[9px] font-bold uppercase tracking-wider text-muted-foreground/70">
							State history
						</div>
						{loading && <LoaderCircle className="size-3 animate-spin" />}
					</div>
					{error && (
						<div className="mx-1 mb-1 rounded-sm bg-destructive/10 px-2 py-1.5 text-[10px] text-destructive">
							{error}
						</div>
					)}
					{!loading && entries.length === 0 && !error ? (
						<div className="px-2 py-2 text-xs text-muted-foreground">
							No saved revisions
						</div>
					) : (
						<div className="max-h-72 overflow-y-auto">
							{entries.map((entry) => {
								const current = entry.position === "current";
								return (
									<button
										key={entry.revisionId}
										type="button"
										onClick={() => !current && setPending(entry)}
										disabled={current || disabled}
										className="flex w-full items-start gap-2 rounded-sm px-2 py-1.5 text-left transition-colors hover:bg-hover disabled:cursor-default disabled:opacity-70"
									>
										<History className="mt-0.5 size-3 shrink-0 text-muted-foreground/60" />
										<div className="min-w-0 flex-1">
											<div
												className="truncate text-xs text-foreground/90"
												title={entry.message}
											>
												{entry.message}
											</div>
											<div className="flex items-center gap-1.5 text-[10px] text-muted-foreground/70">
												<span>{kindLabel(entry.kind)}</span>
												<span aria-hidden="true">·</span>
												<span>{formatRevisionTime(entry.authoredAt)}</span>
												{current && (
													<span className="ml-auto text-primary">Current</span>
												)}
												{entry.position === "superseded" && (
													<span className="ml-auto text-muted-foreground">
														Superseded
													</span>
												)}
											</div>
										</div>
									</button>
								);
							})}
							{nextCursor && (
								<button
									type="button"
									onClick={() => void loadOlder()}
									disabled={loadingOlder}
									className="flex w-full items-center justify-center gap-1.5 rounded-sm px-2 py-2 text-[10px] font-medium text-muted-foreground transition-colors hover:bg-hover hover:text-foreground disabled:opacity-60"
								>
									{loadingOlder && (
										<LoaderCircle className="size-3 animate-spin" />
									)}
									Load older revisions
								</button>
							)}
						</div>
					)}
				</PopoverContent>
			</Popover>

			<AlertDialog
				open={pending !== null}
				onOpenChange={(next) => !next && !restoring && setPending(null)}
			>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>Restore this state?</AlertDialogTitle>
						<AlertDialogDescription>
							Both choices apply the selected state as a new forward revision.
							Rewinding also opens a new conversation from that turn; the
							original conversation remains complete.
						</AlertDialogDescription>
					</AlertDialogHeader>
					{error && (
						<div className="rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">
							{error}
						</div>
					)}
					{pending &&
						(pending.conversationCheckpoint === null ||
							pending.conversationCheckpoint.threadId !== threadId) && (
							<div className="text-xs text-muted-foreground">
								This revision has no checkpoint in this conversation, so only
								its state can be restored.
							</div>
						)}
					<AlertDialogFooter>
						<AlertDialogCancel disabled={restoring}>Cancel</AlertDialogCancel>
						<AlertDialogAction
							onClick={(event) => void confirmRestore(event, "state_only")}
							disabled={disabled || restoring}
						>
							{restoring ? (
								<LoaderCircle className="size-4 animate-spin" />
							) : (
								<RotateCcw className="size-4" />
							)}
							Restore state
						</AlertDialogAction>
						<AlertDialogAction
							onClick={(event) =>
								void confirmRestore(event, "state_and_conversation")
							}
							disabled={
								disabled ||
								restoring ||
								pending?.conversationCheckpoint === null ||
								pending?.conversationCheckpoint?.threadId !== threadId
							}
						>
							{restoring ? (
								<LoaderCircle className="size-4 animate-spin" />
							) : (
								<MessagesSquare className="size-4" />
							)}
							Restore + rewind chat
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</>
	);
}
