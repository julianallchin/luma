import type { ToolSet } from "ai";
import type {
	AuthoredMergeConflict,
	AuthoredProjectedDocument,
	AuthoredWorkspaceCheck,
	AuthoredWorkspaceCommit,
	AuthoredWorkspaceHandle,
	AuthoredWorkspaceMerge,
} from "@/bindings/schema";
import {
	checkAuthoredWorkspace,
	commitAuthoredWorkspace,
	createAuthoredWorkspace,
	currentAuthoredRevision,
	mergeAuthoredWorkspace,
	removeAuthoredWorkspace,
} from "./authored-workspace";
import { buildAuthoredWorkspaceTools } from "./authored-workspace-tools";

export type AuthoredSubagentFinalization =
	| {
			status: "merged" | "unchanged";
			workspaceId: string;
			revisionId: string;
			document: AuthoredProjectedDocument;
	  }
	| {
			status: "conflicted";
			workspaceId: string;
			proposalRevisionId: string;
			conflicts: AuthoredMergeConflict[];
	  };

export type AppliedAuthoredSubagent = Extract<
	AuthoredSubagentFinalization,
	{ status: "merged" | "unchanged" }
>;

export type PreparedAuthoredSubagent = {
	workspaceId: string;
	baseRevisionId: string;
	tools: ToolSet;
	finalize: (summary: string) => Promise<AuthoredSubagentFinalization>;
	discard: () => Promise<void>;
};

type MergeSlot = {
	waitForTurn: () => Promise<void>;
	release: () => void;
};

type ScheduledMergeSlot = {
	predecessor: Promise<void>;
	childrenTail: Promise<void>;
	done: Promise<void>;
	release: () => void;
};

/**
 * Deterministic post-order merge scheduling for recursive subagents.
 *
 * Root runs and siblings retain spawn order. A nested run starts after its
 * previous sibling (or its parent's predecessor), not after its parent, so a
 * foreground parent can await the child without deadlocking. Once the model is
 * done, the parent waits for its reserved children before taking its own turn.
 */
export class AuthoredSubagentMergeScheduler {
	private rootTail: Promise<void> = Promise.resolve();
	private readonly slots = new Map<string, ScheduledMergeSlot>();

	reserve(runId: string, parentSubagentId?: string): MergeSlot {
		if (this.slots.has(runId)) {
			throw new Error(`A merge slot already exists for subagent "${runId}".`);
		}
		const parent = parentSubagentId
			? this.slots.get(parentSubagentId)
			: undefined;
		if (parentSubagentId && !parent) {
			throw new Error(
				`The parent merge slot for subagent "${parentSubagentId}" is unavailable.`,
			);
		}

		const predecessor = parent ? parent.childrenTail : this.rootTail;
		let resolveDone: () => void = () => undefined;
		const done = new Promise<void>((resolve) => {
			resolveDone = resolve;
		});
		let released = false;
		const slot: ScheduledMergeSlot = {
			predecessor,
			childrenTail: predecessor,
			done,
			release: () => {
				if (released) return;
				released = true;
				this.slots.delete(runId);
				resolveDone();
			},
		};
		this.slots.set(runId, slot);
		if (parent) parent.childrenTail = done;
		else this.rootTail = done;

		return {
			waitForTurn: async () => {
				await slot.predecessor;
				// Direct children are all reserved before this run's model returns.
				// Each child similarly waits for its descendants before releasing.
				await slot.childrenTail;
			},
			release: slot.release,
		};
	}
}

function workspaceFiles(document: AuthoredProjectedDocument): string[] {
	switch (document.kind) {
		case "track_score":
			return ["score.luma"];
		case "pattern_graph":
			return ["graph.json", "layout.json"];
	}
}

/**
 * Owns authored workspaces for every child of one parent conversation.
 * Children execute concurrently, but their detached revisions merge in spawn
 * order so completion timing cannot change the resulting authored history.
 */
export class AuthoredSubagentSupervisor {
	private readonly mergeScheduler = new AuthoredSubagentMergeScheduler();

	constructor(
		private readonly threadId: string,
		private readonly applyMergedDocument: (
			result: AppliedAuthoredSubagent,
		) => void | Promise<void>,
		private readonly beforeMerge?: () => void | Promise<void>,
	) {}

	async prepare(
		runId: string = crypto.randomUUID(),
		parentSubagentId?: string,
	): Promise<PreparedAuthoredSubagent> {
		const slot = this.mergeScheduler.reserve(runId, parentSubagentId);
		let workspace: AuthoredWorkspaceHandle | undefined;
		let current:
			| Awaited<ReturnType<typeof currentAuthoredRevision>>
			| undefined;
		try {
			current = await currentAuthoredRevision(this.threadId);
			workspace = await createAuthoredWorkspace({
				threadId: this.threadId,
				requestId: crypto.randomUUID(),
				expectedBaseRevisionId: current.revisionId,
			});
		} catch (error) {
			try {
				if (workspace) {
					await removeAuthoredWorkspace({
						threadId: this.threadId,
						workspaceId: workspace.id,
					});
				}
			} finally {
				slot.release();
			}
			throw error;
		}
		if (!workspace || !current) {
			slot.release();
			throw new Error("The authored workspace allocation was incomplete.");
		}

		const allocatedWorkspace = workspace;
		const commitOperationId = crypto.randomUUID();
		const mergeOperationId = crypto.randomUUID();
		let stableSummary: string | undefined;
		let checked: AuthoredWorkspaceCheck | undefined;
		let committed: AuthoredWorkspaceCommit | undefined;
		let merged: AuthoredWorkspaceMerge | undefined;
		let currentAfterSlot:
			| Awaited<ReturnType<typeof currentAuthoredRevision>>
			| undefined;
		let applied = false;
		let workspaceRemoved = false;
		let discardRequested = false;
		let discarded = false;
		let finalResult: AuthoredSubagentFinalization | undefined;
		let finalizeAttempt: Promise<AuthoredSubagentFinalization> | undefined;
		let discardAttempt: Promise<void> | undefined;

		const removeWorkspace = async () => {
			if (workspaceRemoved) return;
			await removeAuthoredWorkspace({
				threadId: this.threadId,
				workspaceId: allocatedWorkspace.id,
			});
			workspaceRemoved = true;
		};

		const runFinalize = async (): Promise<AuthoredSubagentFinalization> => {
			await slot.waitForTurn();
			if (discardRequested) {
				throw new Error("The subagent workspace was discarded.");
			}
			await this.beforeMerge?.();

			checked ??= await checkAuthoredWorkspace({
				threadId: this.threadId,
				workspaceId: allocatedWorkspace.id,
			});
			if (!checked.changed) {
				currentAfterSlot ??= await currentAuthoredRevision(this.threadId);
				const result: AppliedAuthoredSubagent = {
					status: "unchanged",
					workspaceId: allocatedWorkspace.id,
					revisionId: currentAfterSlot.revisionId,
					document: currentAfterSlot.document,
				};
				if (!applied) {
					await this.applyMergedDocument(result);
					applied = true;
				}
				await removeWorkspace();
				finalResult = result;
				slot.release();
				return result;
			}

			committed ??= await commitAuthoredWorkspace({
				threadId: this.threadId,
				workspaceId: allocatedWorkspace.id,
				expectedHeadRevisionId: checked.headRevisionId,
				expectedSnapshotId: checked.snapshotId,
				operationId: commitOperationId,
				message: stableSummary ?? "Subagent changes",
			});
			merged ??= await mergeAuthoredWorkspace({
				threadId: this.threadId,
				workspaceId: allocatedWorkspace.id,
				expectedHeadRevisionId: committed.revisionId,
				operationId: mergeOperationId,
			});

			if (merged.status === "conflicted") {
				const result: AuthoredSubagentFinalization = {
					status: "conflicted",
					workspaceId: allocatedWorkspace.id,
					proposalRevisionId: committed.revisionId,
					conflicts: merged.conflicts,
				};
				// The immutable proposal revision and structured conflicts are the
				// complete resolver handoff. A later child resolves against current
				// state, so retaining this private directory only leaks resources.
				await removeWorkspace();
				finalResult = result;
				slot.release();
				return result;
			}

			const result: AppliedAuthoredSubagent = {
				status: "merged",
				workspaceId: allocatedWorkspace.id,
				revisionId: merged.revisionId,
				document: merged.document,
			};
			// `document` is deliberately the backend's safe current projection,
			// including when an idempotent merge replay no longer owns the head.
			if (!applied) {
				await this.applyMergedDocument(result);
				applied = true;
			}
			await removeWorkspace();
			finalResult = result;
			slot.release();
			return result;
		};

		const finalize = (summary: string) => {
			if (finalResult) return Promise.resolve(finalResult);
			if (discardRequested) {
				return Promise.reject(
					new Error("The subagent workspace was discarded."),
				);
			}
			if (finalizeAttempt) return finalizeAttempt;
			stableSummary ??= summary.trim() || "Subagent changes";
			const attempt = runFinalize();
			finalizeAttempt = attempt;
			void attempt.catch(() => {
				if (finalizeAttempt === attempt) finalizeAttempt = undefined;
			});
			return attempt;
		};

		const discard = () => {
			if (discardAttempt) return discardAttempt;
			if (discarded) return Promise.resolve();
			discardRequested = true;
			const activeFinalize = finalizeAttempt;
			const attempt = (async () => {
				try {
					await slot.waitForTurn();
					if (activeFinalize) {
						try {
							await activeFinalize;
						} catch {
							// A failed phase remains resumable, but discard retires it.
						}
					}
					await removeWorkspace();
					discarded = true;
				} finally {
					slot.release();
				}
			})();
			discardAttempt = attempt;
			void attempt.catch(() => {
				if (discardAttempt === attempt) discardAttempt = undefined;
			});
			return attempt;
		};

		return {
			workspaceId: allocatedWorkspace.id,
			baseRevisionId: allocatedWorkspace.baseRevisionId,
			tools: buildAuthoredWorkspaceTools({
				threadId: this.threadId,
				workspaceId: allocatedWorkspace.id,
				fileNames: workspaceFiles(current.document),
			}),
			finalize,
			discard,
		};
	}
}
