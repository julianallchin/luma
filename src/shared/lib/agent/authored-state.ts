import type {
	AuthoredOperationKind,
	FinalizeAuthoredTurnInput,
	AuthoredHistoryEntry as GeneratedAuthoredHistoryEntry,
	AuthoredHistoryPage as GeneratedAuthoredHistoryPage,
	AuthoredRestoreMode as GeneratedAuthoredRestoreMode,
	AuthoredRestoreResult as GeneratedAuthoredRestoreResult,
	AuthoredTurnCommit as GeneratedAuthoredTurnCommit,
	PreparedAuthoredTurn as GeneratedPreparedAuthoredTurn,
	PrepareAuthoredTurnInput,
	RestoreAuthoredStateInput,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";

export type PreparedAuthoredTurn = GeneratedPreparedAuthoredTurn;
export type AuthoredTurnCommit = GeneratedAuthoredTurnCommit;
export type AuthoredHistoryKind = AuthoredOperationKind;
export type AuthoredHistoryEntry = GeneratedAuthoredHistoryEntry;
export type AuthoredHistoryPage = GeneratedAuthoredHistoryPage;
export type AuthoredRestoreResult = GeneratedAuthoredRestoreResult;
export type AuthoredRestoreMode = GeneratedAuthoredRestoreMode;

export function prepareAuthoredTurn(
	input: PrepareAuthoredTurnInput,
): Promise<PreparedAuthoredTurn> {
	return invoke<PreparedAuthoredTurn>("authored_state_prepare_turn", { input });
}

export function finalizeAuthoredTurn(
	input: FinalizeAuthoredTurnInput,
): Promise<AuthoredTurnCommit> {
	return invoke<AuthoredTurnCommit>("authored_state_finalize_turn", { input });
}

export function recoverAuthoredTurns(
	threadId: string,
): Promise<AuthoredTurnCommit[]> {
	return invoke<AuthoredTurnCommit[]>("authored_state_recover_turns", {
		threadId,
	});
}

export function listAuthoredHistory(
	threadId: string,
	cursor: string | null = null,
	limit = 100,
): Promise<AuthoredHistoryPage> {
	return invoke<AuthoredHistoryPage>("authored_state_list_history", {
		threadId,
		cursor,
		limit,
	});
}

export function restoreAuthoredState(
	input: RestoreAuthoredStateInput,
): Promise<AuthoredRestoreResult> {
	return invoke<AuthoredRestoreResult>("authored_state_restore", { input });
}
