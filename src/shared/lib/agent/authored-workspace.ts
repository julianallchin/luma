import type {
	AuthoredCurrentRevision,
	AuthoredWorkspaceCheck,
	AuthoredWorkspaceCommit,
	AuthoredWorkspaceHandle,
	AuthoredWorkspaceInput,
	AuthoredWorkspaceMerge,
	CommitAuthoredWorkspaceInput,
	CreateAuthoredWorkspaceInput,
	ForkAuthoredWorkspaceInput,
	MergeAuthoredWorkspaceInput,
	MergeAuthoredWorkspaceIntoWorkspaceInput,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";

export type {
	AuthoredCurrentRevision,
	AuthoredWorkspaceCheck,
	AuthoredWorkspaceCommit,
	AuthoredWorkspaceHandle,
	AuthoredWorkspaceInput,
	AuthoredWorkspaceMerge,
};

export function currentAuthoredRevision(
	threadId: string,
): Promise<AuthoredCurrentRevision> {
	return invoke<AuthoredCurrentRevision>("authored_state_current_revision", {
		threadId,
	});
}

export function createAuthoredWorkspace(
	input: CreateAuthoredWorkspaceInput,
): Promise<AuthoredWorkspaceHandle> {
	return invoke<AuthoredWorkspaceHandle>("authored_state_create_workspace", {
		input,
	});
}

export function forkAuthoredWorkspace(
	input: ForkAuthoredWorkspaceInput,
): Promise<AuthoredWorkspaceHandle> {
	return invoke<AuthoredWorkspaceHandle>("authored_state_fork_workspace", {
		input,
	});
}

export function checkAuthoredWorkspace(
	input: AuthoredWorkspaceInput,
): Promise<AuthoredWorkspaceCheck> {
	return invoke<AuthoredWorkspaceCheck>("authored_state_check_workspace", {
		input,
	});
}

export function commitAuthoredWorkspace(
	input: CommitAuthoredWorkspaceInput,
): Promise<AuthoredWorkspaceCommit> {
	return invoke<AuthoredWorkspaceCommit>("authored_state_commit_workspace", {
		input,
	});
}

export function mergeAuthoredWorkspace(
	input: MergeAuthoredWorkspaceInput,
): Promise<AuthoredWorkspaceMerge> {
	return invoke<AuthoredWorkspaceMerge>("authored_state_merge_workspace", {
		input,
	});
}

export function mergeAuthoredWorkspaceIntoWorkspace(
	input: MergeAuthoredWorkspaceIntoWorkspaceInput,
): Promise<AuthoredWorkspaceMerge> {
	return invoke<AuthoredWorkspaceMerge>(
		"authored_state_merge_workspace_into_workspace",
		{ input },
	);
}

export function removeAuthoredWorkspace(
	input: AuthoredWorkspaceInput,
): Promise<void> {
	return invoke<void>("authored_state_remove_workspace", { input });
}
