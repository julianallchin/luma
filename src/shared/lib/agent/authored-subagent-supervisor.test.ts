import { beforeEach, describe, expect, it, vi } from "vitest";
import {
	AuthoredSubagentMergeScheduler,
	AuthoredSubagentSupervisor,
	subagentRevisionSubject,
} from "./authored-subagent-supervisor";
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

vi.mock("./authored-workspace", () => ({
	checkAuthoredWorkspace: vi.fn(),
	commitAuthoredWorkspace: vi.fn(),
	createAuthoredWorkspace: vi.fn(),
	currentAuthoredRevision: vi.fn(),
	forkAuthoredWorkspace: vi.fn(),
	mergeAuthoredWorkspace: vi.fn(),
	mergeAuthoredWorkspaceIntoWorkspace: vi.fn(),
	removeAuthoredWorkspace: vi.fn(),
}));

describe("AuthoredSubagentMergeScheduler", () => {
	it("keeps root merges in spawn order", async () => {
		const scheduler = new AuthoredSubagentMergeScheduler();
		const first = scheduler.reserve("first");
		const second = scheduler.reserve("second");
		let firstReady = false;
		let secondReady = false;
		void first.waitForTurn().then(() => {
			firstReady = true;
		});
		void second.waitForTurn().then(() => {
			secondReady = true;
		});

		await vi.waitFor(() => expect(firstReady).toBe(true));
		expect(secondReady).toBe(false);
		first.release();
		await vi.waitFor(() => expect(secondReady).toBe(true));
		second.release();
	});

	it("runs descendants before ancestors while preserving sibling order", async () => {
		const scheduler = new AuthoredSubagentMergeScheduler();
		const root = scheduler.reserve("root");
		const firstChild = scheduler.reserve("first-child", "root");
		const grandchild = scheduler.reserve("grandchild", "first-child");
		const secondChild = scheduler.reserve("second-child", "root");
		const ready: string[] = [];
		void root.waitForTurn().then(() => ready.push("root"));
		void firstChild.waitForTurn().then(() => ready.push("first-child"));
		void grandchild.waitForTurn().then(() => ready.push("grandchild"));
		void secondChild.waitForTurn().then(() => ready.push("second-child"));

		await vi.waitFor(() => expect(ready).toEqual(["grandchild"]));
		grandchild.release();
		await vi.waitFor(() =>
			expect(ready).toEqual(["grandchild", "first-child"]),
		);
		firstChild.release();
		await vi.waitFor(() =>
			expect(ready).toEqual(["grandchild", "first-child", "second-child"]),
		);
		secondChild.release();
		await vi.waitFor(() =>
			expect(ready).toEqual([
				"grandchild",
				"first-child",
				"second-child",
				"root",
			]),
		);
		root.release();
	});

	it("rejects a nested slot after its parent is no longer active", () => {
		const scheduler = new AuthoredSubagentMergeScheduler();
		const parent = scheduler.reserve("parent");
		parent.release();
		expect(() => scheduler.reserve("late-child", "parent")).toThrow(
			/parent merge slot/i,
		);
	});
});

describe("AuthoredSubagentSupervisor", () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it("merges a domain-tool revision when raw workspace files are unchanged", async () => {
		const initialDocument = {
			kind: "track_score" as const,
			revision: "base-document",
		};
		const childDocument = {
			kind: "track_score" as const,
			revision: "child-document",
		};
		const mergedDocument = {
			kind: "track_score" as const,
			revision: "merged-document",
		};
		vi.mocked(currentAuthoredRevision).mockResolvedValue({
			documentId: "document-1",
			revisionId: "revision-base",
			document: initialDocument,
		});
		vi.mocked(createAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			baseRevisionId: "revision-base",
			headRevisionId: "revision-base",
		});
		vi.mocked(checkAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			headRevisionId: "revision-child",
			snapshotId: "snapshot-child",
			changed: false,
			document: childDocument,
		});
		vi.mocked(mergeAuthoredWorkspace).mockResolvedValue({
			status: "merged",
			documentId: "document-1",
			revisionId: "revision-merged",
			appliedToCurrentProjection: true,
			document: mergedDocument,
		});
		vi.mocked(removeAuthoredWorkspace).mockResolvedValue();
		const apply = vi.fn();
		const supervisor = new AuthoredSubagentSupervisor("thread-1", apply);

		const workspace = await supervisor.prepare("child-1");
		const result = await workspace.finalize("Changed the chorus");

		expect(workspace.initialDocument).toEqual(initialDocument);
		expect(commitAuthoredWorkspace).not.toHaveBeenCalled();
		expect(mergeAuthoredWorkspace).toHaveBeenCalledWith(
			expect.objectContaining({
				workspaceId: "workspace-1",
				expectedHeadRevisionId: "revision-child",
			}),
		);
		expect(result).toMatchObject({
			status: "merged",
			revisionId: "revision-merged",
			document: mergedDocument,
		});
		expect(apply).toHaveBeenCalledOnce();
	});

	it("forks recursive children from the parent detached workspace", async () => {
		const rootDocument = {
			kind: "track_score" as const,
			revision: "root-document",
		};
		const parentDocument = {
			kind: "track_score" as const,
			revision: "parent-document",
		};
		const mergedDocument = {
			kind: "track_score" as const,
			revision: "merged-child-document",
		};
		vi.mocked(currentAuthoredRevision).mockResolvedValue({
			documentId: "document-1",
			revisionId: "revision-root",
			document: rootDocument,
		});
		vi.mocked(createAuthoredWorkspace).mockResolvedValue({
			id: "workspace-parent",
			baseRevisionId: "revision-root",
			headRevisionId: "revision-root",
		});
		vi.mocked(forkAuthoredWorkspace).mockResolvedValue({
			id: "workspace-child",
			baseRevisionId: "revision-root",
			headRevisionId: "revision-root",
		});
		vi.mocked(checkAuthoredWorkspace)
			.mockResolvedValueOnce({
				id: "workspace-parent",
				headRevisionId: "revision-root",
				snapshotId: "snapshot-parent",
				changed: false,
				document: rootDocument,
			})
			.mockResolvedValueOnce({
				id: "workspace-child",
				headRevisionId: "revision-root",
				snapshotId: "snapshot-child",
				changed: true,
				document: parentDocument,
			})
			.mockResolvedValueOnce({
				id: "workspace-child",
				headRevisionId: "revision-root",
				snapshotId: "snapshot-child-final",
				changed: true,
				document: parentDocument,
			})
			.mockResolvedValueOnce({
				id: "workspace-parent",
				headRevisionId: "revision-root",
				snapshotId: "snapshot-parent-final",
				changed: false,
				document: parentDocument,
			});
		vi.mocked(commitAuthoredWorkspace).mockResolvedValue({
			id: "workspace-child",
			revisionId: "revision-child",
			appliedToCurrentWorkspace: true,
			changed: true,
			document: parentDocument,
		});
		vi.mocked(mergeAuthoredWorkspaceIntoWorkspace)
			.mockRejectedValueOnce(new Error("recursive merge response lost"))
			.mockResolvedValue({
				status: "merged",
				documentId: "document-1",
				revisionId: "revision-parent-merged",
				appliedToCurrentProjection: false,
				document: mergedDocument,
			});
		vi.mocked(removeAuthoredWorkspace).mockResolvedValue();
		const supervisor = new AuthoredSubagentSupervisor(
			"thread-1",
			() => undefined,
		);

		const parent = await supervisor.prepare("parent");
		const parentSink = vi.fn();
		parent.bindDocumentSink(parentSink);
		const child = await supervisor.prepare("child", "parent");

		expect(child.initialDocument).toEqual(parentDocument);
		expect(forkAuthoredWorkspace).toHaveBeenCalledWith(
			expect.objectContaining({
				threadId: "thread-1",
				sourceWorkspaceId: "workspace-parent",
			}),
		);
		expect(createAuthoredWorkspace).toHaveBeenCalledOnce();
		expect(currentAuthoredRevision).toHaveBeenCalledOnce();
		await expect(child.finalize("Nested changes")).resolves.toMatchObject({
			status: "merged",
			revisionId: "revision-parent-merged",
			document: mergedDocument,
		});
		expect(mergeAuthoredWorkspaceIntoWorkspace).toHaveBeenCalledWith(
			expect.objectContaining({
				workspaceId: "workspace-child",
				targetWorkspaceId: "workspace-parent",
				expectedHeadRevisionId: "revision-child",
			}),
		);
		expect(mergeAuthoredWorkspaceIntoWorkspace).toHaveBeenCalledTimes(2);
		expect(mergeAuthoredWorkspace).not.toHaveBeenCalled();
		expect(parentSink).toHaveBeenCalledWith(mergedDocument);

		await parent.discard();
	});

	it("reports an authoritative merge even when projection application fails", async () => {
		const document = {
			kind: "track_score" as const,
			revision: "document-1",
		};
		vi.mocked(currentAuthoredRevision).mockResolvedValue({
			documentId: "document-1",
			revisionId: "revision-1",
			document,
		});
		vi.mocked(createAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			baseRevisionId: "revision-1",
			headRevisionId: "revision-1",
		});
		vi.mocked(checkAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			headRevisionId: "revision-1",
			snapshotId: "snapshot-1",
			changed: false,
			document,
		});
		vi.mocked(removeAuthoredWorkspace).mockResolvedValue();
		const apply = vi.fn().mockRejectedValue(new Error("bridge unavailable"));
		const consoleError = vi
			.spyOn(console, "error")
			.mockImplementation(() => {});
		const supervisor = new AuthoredSubagentSupervisor("thread-1", apply);

		const workspace = await supervisor.prepare("child-1");
		await expect(workspace.finalize("No changes")).resolves.toMatchObject({
			status: "unchanged",
			revisionId: "revision-1",
		});
		expect(removeAuthoredWorkspace).toHaveBeenCalledOnce();
		expect(consoleError).toHaveBeenCalledOnce();
		consoleError.mockRestore();
	});

	it("derives a one-line UTF-8-safe revision subject", () => {
		expect(
			subagentRevisionSubject("\n  Built   the chorus  \nignored detail"),
		).toBe("Built the chorus");
		expect(subagentRevisionSubject("\n \t \n")).toBe("Subagent changes");

		const subject = subagentRevisionSubject(`Lighting ${"💡".repeat(100)}`);
		expect(new TextEncoder().encode(subject).byteLength).toBeLessThanOrEqual(
			240,
		);
		expect(subject).toMatch(/…$/);
		expect(subject).not.toContain("�");
	});

	it("never passes a raw multiline child result as a commit subject", async () => {
		const initialDocument = {
			kind: "track_score" as const,
			revision: "base-document",
		};
		vi.mocked(currentAuthoredRevision).mockResolvedValue({
			documentId: "document-1",
			revisionId: "revision-base",
			document: initialDocument,
		});
		vi.mocked(createAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			baseRevisionId: "revision-base",
			headRevisionId: "revision-base",
		});
		vi.mocked(checkAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			headRevisionId: "revision-base",
			snapshotId: "snapshot-child",
			changed: true,
			document: initialDocument,
		});
		vi.mocked(commitAuthoredWorkspace).mockResolvedValue({
			id: "workspace-1",
			revisionId: "revision-child",
			appliedToCurrentWorkspace: true,
			changed: true,
			document: initialDocument,
		});
		vi.mocked(mergeAuthoredWorkspace).mockResolvedValue({
			status: "merged",
			documentId: "document-1",
			revisionId: "revision-merged",
			appliedToCurrentProjection: true,
			document: initialDocument,
		});
		vi.mocked(removeAuthoredWorkspace).mockResolvedValue();
		const supervisor = new AuthoredSubagentSupervisor(
			"thread-1",
			() => undefined,
		);
		const workspace = await supervisor.prepare("child-1");

		await workspace.finalize(
			`\n  Lighting   ${"💡".repeat(100)}  \nfull child detail`,
		);

		const input = vi.mocked(commitAuthoredWorkspace).mock.calls[0]?.[0];
		expect(input?.message).not.toContain("\n");
		expect(
			new TextEncoder().encode(input?.message ?? "").byteLength,
		).toBeLessThanOrEqual(240);
		expect(input?.message).toMatch(/…$/);
	});
});
