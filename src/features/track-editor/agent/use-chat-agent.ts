import { useCallback, useEffect } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import type { BarClassificationsPayload, DrumOnsets } from "./build-context";
import {
	type ChatMessage,
	type ChatPart,
	type ChatReasoningPart,
	type ChatTextPart,
	type ChatToolPart,
	type SendArgs,
	type SessionContext,
	type ToolPart,
	useChatSessionsStore,
} from "./use-chat-sessions-store";

export type {
	ChatMessage,
	ChatPart,
	ChatReasoningPart,
	ChatTextPart,
	ChatToolPart,
	SendArgs,
	ToolPart,
};

const EMPTY_MESSAGES: ChatMessage[] = [];

type ChatSessionExtras = {
	barClassifications: BarClassificationsPayload | null;
	drumOnsets: DrumOnsets | null;
	tagThresholds: Record<string, number>;
};

/**
 * Subscribe to the chat session for a given trackId. Ensures a session
 * exists and that its context is seeded from whatever the editor store
 * currently knows; the module-level mirror in `use-chat-sessions-store`
 * keeps the context fresh as the editor's state evolves.
 *
 * Pass live extras (bar tags, drum onsets, tag thresholds, venue name) so
 * they land on the session and are available even when the agent runs in
 * the background.
 */
export function useChatSession(
	trackId: string | null,
	extras: ChatSessionExtras,
) {
	const venueName = useAppViewStore((s) => s.currentVenue?.name ?? null);

	const session = useChatSessionsStore((s) =>
		trackId ? s.sessions[trackId] : undefined,
	);

	const messages = session?.messages ?? EMPTY_MESSAGES;
	const streaming = session?.streaming ?? false;
	const error = session?.error ?? null;

	// Seed the session on mount and whenever the editor state we mirror has
	// already moved past whatever the session knows. The shared module-level
	// subscription handles ongoing updates; this just covers the initial
	// state-already-set case.
	useEffect(() => {
		if (!trackId) return;
		const sessions = useChatSessionsStore.getState();
		sessions.ensureSession(trackId);
		const editor = useTrackEditorStore.getState();
		const seed: Partial<SessionContext> = {
			venueName,
			barClassifications: extras.barClassifications,
			drumOnsets: extras.drumOnsets,
			tagThresholds: extras.tagThresholds,
		};
		if (editor.trackId === trackId) {
			if (editor.venueId) seed.venueId = editor.venueId;
			if (editor.scoreId) seed.scoreId = editor.scoreId;
			seed.readOnly = editor.readOnly;
			seed.trackName = editor.trackName;
			seed.durationSeconds = editor.durationSeconds;
			seed.beatGrid = editor.beatGrid;
			seed.annotations = editor.annotations;
			seed.patterns = editor.patterns;
			seed.patternArgs = editor.patternArgs;
		}
		sessions.updateContext(trackId, seed);
	}, [
		trackId,
		venueName,
		extras.barClassifications,
		extras.drumOnsets,
		extras.tagThresholds,
	]);

	const send = useCallback(
		async (args: SendArgs) => {
			if (!trackId) return;
			await useChatSessionsStore.getState().send(trackId, args);
		},
		[trackId],
	);

	const abort = useCallback(() => {
		if (!trackId) return;
		useChatSessionsStore.getState().abort(trackId);
	}, [trackId]);

	const reset = useCallback(() => {
		if (!trackId) return;
		useChatSessionsStore.getState().reset(trackId);
	}, [trackId]);

	return { messages, streaming, error, send, abort, reset };
}
