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
	forkAuthoredWorkspace,
	mergeAuthoredWorkspace,
	mergeAuthoredWorkspaceIntoWorkspace,
	removeAuthoredWorkspace,
} from "./authored-workspace";

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
	initialDocument: AuthoredProjectedDocument;
	runWorkspaceOperation: <T>(operation: () => T | Promise<T>) => Promise<T>;
	bindDocumentSink: (
		sink: (document: AuthoredProjectedDocument) => void | Promise<void>,
	) => void;
	finalize: (summary: string) => Promise<AuthoredSubagentFinalization>;
	discard: () => Promise<void>;
};

const MAX_REVISION_SUBJECT_BYTES = 240;
const textEncoder = new TextEncoder();

/** Keep child-authored revision metadata deterministic, one-line, and within
 * the backend's byte limit regardless of the model's final response shape. */
export function subagentRevisionSubject(result: string): string {
	const firstLine = result
		.split(/\r?\n/)
		.map((line) => line.replace(/\s+/g, " ").trim())
		.find(Boolean);
	const subject = firstLine || "Subagent changes";
	if (textEncoder.encode(subject).byteLength <= MAX_REVISION_SUBJECT_BYTES) {
		return subject;
	}

	const suffix = "…";
	const suffixBytes = textEncoder.encode(suffix).byteLength;
	let truncated = "";
	for (const character of subject) {
		if (
			textEncoder.encode(truncated + character).byteLength + suffixBytes >
			MAX_REVISION_SUBJECT_BYTES
		) {
			break;
		}
		truncated += character;
	}
	return `${truncated}${suffix}`;
}

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

type ActiveAuthoredWorkspace = {
	workspaceId: string;
	checkpoint: () => Promise<void>;
	applyDocument: (document: AuthoredProjectedDocument) => Promise<void>;
	runOperation: <T>(operation: () => T | Promise<T>) => Promise<T>;
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

/**
 * Owns authored workspaces for every child of one parent conversation.
 * Children execute concurrently, but their detached revisions merge in spawn
 * order so completion timing cannot change the resulting authored history.
 */
export class AuthoredSubagentSupervisor {
	private readonly mergeScheduler = new AuthoredSubagentMergeScheduler();
	private readonly activeWorkspaces = new Map<
		string,
		ActiveAuthoredWorkspace
	>();

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
		let initialDocument: AuthoredProjectedDocument | undefined;
		let parentWorkspaceId: string | undefined;
		let parentWorkspaceRuntime: ActiveAuthoredWorkspace | undefined;
		try {
			if (parentSubagentId) {
				const parentWorkspace = this.activeWorkspaces.get(parentSubagentId);
				if (!parentWorkspace) {
					throw new Error(
						`The parent workspace for subagent "${parentSubagentId}" is unavailable.`,
					);
				}
				parentWorkspaceRuntime = parentWorkspace;
				parentWorkspaceId = parentWorkspace.workspaceId;
				const allocation = await parentWorkspace.runOperation(async () => {
					await parentWorkspace.checkpoint();
					const childWorkspace = await forkAuthoredWorkspace({
						threadId: this.threadId,
						requestId: crypto.randomUUID(),
						sourceWorkspaceId: parentWorkspace.workspaceId,
					});
					const childInitialDocument = (
						await checkAuthoredWorkspace({
							threadId: this.threadId,
							workspaceId: childWorkspace.id,
						})
					).document;
					return { childWorkspace, childInitialDocument };
				});
				workspace = allocation.childWorkspace;
				initialDocument = allocation.childInitialDocument;
			} else {
				const current = await currentAuthoredRevision(this.threadId);
				workspace = await createAuthoredWorkspace({
					threadId: this.threadId,
					requestId: crypto.randomUUID(),
					expectedBaseRevisionId: current.revisionId,
				});
				initialDocument = current.document;
			}
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
		if (!workspace || !initialDocument) {
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
		let checkpointTail = Promise.resolve();
		let workspaceOperationTail = Promise.resolve();
		let documentSink:
			| ((document: AuthoredProjectedDocument) => void | Promise<void>)
			| undefined;

		const checkpoint = (): Promise<void> => {
			const attempt = checkpointTail
				.catch(() => undefined)
				.then(async () => {
					const check = await checkAuthoredWorkspace({
						threadId: this.threadId,
						workspaceId: allocatedWorkspace.id,
					});
					if (!check.changed) return;
					await commitAuthoredWorkspace({
						threadId: this.threadId,
						workspaceId: allocatedWorkspace.id,
						expectedHeadRevisionId: check.headRevisionId,
						expectedSnapshotId: check.snapshotId,
						operationId: crypto.randomUUID(),
						message: "Checkpoint before recursive delegation",
					});
				});
			checkpointTail = attempt.then(
				() => undefined,
				() => undefined,
			);
			return attempt;
		};

		const runWorkspaceOperation = <T>(
			operation: () => T | Promise<T>,
		): Promise<T> => {
			const attempt = workspaceOperationTail
				.catch(() => undefined)
				.then(operation);
			workspaceOperationTail = attempt.then(
				() => undefined,
				() => undefined,
			);
			return attempt;
		};

		const removeWorkspace = async () => {
			if (workspaceRemoved) return;
			await removeAuthoredWorkspace({
				threadId: this.threadId,
				workspaceId: allocatedWorkspace.id,
			});
			workspaceRemoved = true;
			this.activeWorkspaces.delete(runId);
		};

		const applyBestEffort = async (result: AppliedAuthoredSubagent) => {
			if (applied) return;
			applied = true;
			try {
				await this.applyMergedDocument(result);
			} catch (error) {
				// The relational merge is authoritative. A projection bridge failure
				// must not turn committed child work into an authored failure. The
				// caller can refresh immediately; later hydration remains a fallback.
				console.error("Failed to apply merged subagent projection", error);
			}
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
			const headAdvanced =
				checked.headRevisionId !== allocatedWorkspace.baseRevisionId;
			if (!checked.changed && !headAdvanced) {
				let currentRevisionId: string;
				let currentDocument: AuthoredProjectedDocument;
				if (parentWorkspaceId) {
					const current = await checkAuthoredWorkspace({
						threadId: this.threadId,
						workspaceId: parentWorkspaceId,
					});
					currentRevisionId = current.headRevisionId;
					currentDocument = current.document;
				} else {
					currentAfterSlot ??= await currentAuthoredRevision(this.threadId);
					currentRevisionId = currentAfterSlot.revisionId;
					currentDocument = currentAfterSlot.document;
				}
				const result: AppliedAuthoredSubagent = {
					status: "unchanged",
					workspaceId: allocatedWorkspace.id,
					revisionId: currentRevisionId,
					document: currentDocument,
				};
				await removeWorkspace();
				finalResult = result;
				slot.release();
				if (!parentWorkspaceId) await applyBestEffort(result);
				return result;
			}

			if (checked.changed) {
				committed ??= await commitAuthoredWorkspace({
					threadId: this.threadId,
					workspaceId: allocatedWorkspace.id,
					expectedHeadRevisionId: checked.headRevisionId,
					expectedSnapshotId: checked.snapshotId,
					operationId: commitOperationId,
					message: stableSummary ?? "Subagent changes",
				});
			}
			const proposalRevisionId =
				committed?.revisionId ?? checked.headRevisionId;
			merged ??=
				parentWorkspaceRuntime && parentWorkspaceId
					? await parentWorkspaceRuntime.runOperation(async () => {
							await parentWorkspaceRuntime.checkpoint();
							const mergeChild = () =>
								mergeAuthoredWorkspaceIntoWorkspace({
									threadId: this.threadId,
									workspaceId: allocatedWorkspace.id,
									targetWorkspaceId: parentWorkspaceId,
									expectedHeadRevisionId: proposalRevisionId,
									operationId: mergeOperationId,
								});
							let result: AuthoredWorkspaceMerge;
							try {
								result = await mergeChild();
							} catch {
								// Keep the parent queue closed across the idempotent replay.
								// Otherwise a queued parent tool can author from the stale
								// pre-merge document after a lost successful response.
								result = await mergeChild();
							}
							if (result.status === "merged") {
								await parentWorkspaceRuntime.applyDocument(result.document);
							}
							return result;
						})
					: await mergeAuthoredWorkspace({
							threadId: this.threadId,
							workspaceId: allocatedWorkspace.id,
							expectedHeadRevisionId: proposalRevisionId,
							operationId: mergeOperationId,
						});

			if (merged.status === "conflicted") {
				const result: AuthoredSubagentFinalization = {
					status: "conflicted",
					workspaceId: allocatedWorkspace.id,
					proposalRevisionId,
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
			await removeWorkspace();
			finalResult = result;
			slot.release();
			if (!parentWorkspaceId) await applyBestEffort(result);
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
			stableSummary ??= subagentRevisionSubject(summary);
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

		this.activeWorkspaces.set(runId, {
			workspaceId: allocatedWorkspace.id,
			checkpoint,
			runOperation: runWorkspaceOperation,
			applyDocument: async (document) => {
				await documentSink?.(document);
			},
		});
		return {
			workspaceId: allocatedWorkspace.id,
			baseRevisionId: allocatedWorkspace.baseRevisionId,
			initialDocument,
			runWorkspaceOperation,
			bindDocumentSink: (sink) => {
				documentSink = sink;
			},
			finalize,
			discard,
		};
	}
}
