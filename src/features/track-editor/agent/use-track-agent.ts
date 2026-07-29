import { useEffect } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import type { ThreadInit } from "@/shared/lib/agent/threads";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import type { BarClassificationsPayload, DrumOnsets } from "./build-context";
import { trackAgent, trackBridge } from "./track-agent";
import type { SessionContext } from "./track-session-store";
import { useTrackSessionStore } from "./track-session-store";

export type TrackAgentExtras = {
	barClassifications: BarClassificationsPayload | null;
	drumOnsets: DrumOnsets | null;
	tagThresholds: Record<string, number>;
};

/**
 * Seed the agent's session context for a track from whatever the editor store
 * currently knows and register the live bridge. The module-level mirror in
 * `track-session-store` keeps the context fresh as the editor evolves; this
 * covers the initial state-already-set case.
 *
 * Returns the thread init metadata the panel should resolve its thread with.
 */
export function useTrackAgentBridge(
	trackId: string | null,
	extras: TrackAgentExtras,
): ThreadInit {
	const venueName = useAppViewStore((s) => s.currentVenue?.name ?? null);
	const venueId = useTrackEditorStore((s) => s.venueId);
	const scoreId = useTrackEditorStore((s) => s.scoreId);
	const trackName = useTrackEditorStore((s) => s.trackName);

	useEffect(() => {
		if (!trackId) return;
		trackAgent.registerBridge(trackId, trackBridge(trackId));

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
		useTrackSessionStore.getState().updateContext(trackId, seed);
	}, [
		trackId,
		venueName,
		extras.barClassifications,
		extras.drumOnsets,
		extras.tagThresholds,
	]);

	return { venueId, scoreId, title: trackName || null };
}
