import { useEffect } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import type { ThreadInit } from "@/shared/lib/agent/threads";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { trackAgent, trackBridge } from "./track-agent";
import type { SessionContext } from "./track-session-store";
import { useTrackSessionStore } from "./track-session-store";

/**
 * Seed the agent's session context for a track from whatever the editor store
 * currently knows and register the live bridge. The module-level mirror in
 * `track-session-store` keeps the context fresh as the editor evolves; this
 * covers the initial state-already-set case.
 *
 * Returns the thread init metadata the panel should resolve its thread with.
 */
export function useTrackAgentBridge(trackId: string | null): ThreadInit {
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const principalId = useAuthStore((s) => s.user?.id ?? null);
	const currentVenueName = useAppViewStore((s) => s.currentVenue?.name ?? null);
	const venueId = useTrackEditorStore((s) => s.venueId);
	const scoreId = useTrackEditorStore((s) => s.scoreId);
	const trackName = useTrackEditorStore((s) => s.trackName);

	useEffect(() => {
		if (!trackId || !venueId || !scoreId) return;
		const scope = { trackId, venueId, scoreId };

		const editor = useTrackEditorStore.getState();
		if (
			editor.trackId !== trackId ||
			editor.venueId !== venueId ||
			editor.scoreId !== scoreId
		) {
			return;
		}
		const seed: Partial<Omit<SessionContext, "venueId" | "scoreId">> = {};
		if (currentVenueId === venueId) seed.venueName = currentVenueName;
		seed.readOnly = editor.readOnly;
		seed.trackName = editor.trackName;
		seed.durationSeconds = editor.durationSeconds;
		seed.beatGrid = editor.beatGrid;
		seed.annotations = editor.annotations;
		seed.patterns = editor.patterns;
		seed.patternArgs = editor.patternArgs;
		useTrackSessionStore.getState().updateContext(scope, seed);
		return trackAgent.registerBridge(trackId, trackBridge(scope), {
			principalId,
			venueId,
			scoreId,
		});
	}, [
		trackId,
		venueId,
		scoreId,
		principalId,
		currentVenueId,
		currentVenueName,
	]);

	return { principalId, venueId, scoreId, title: trackName || null };
}
