import { toast } from "sonner";
import type { TrackBrowserRow } from "@/bindings/schema";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { trackAgent, trackBridge } from "./track-agent";
import { useTrackSessionStore } from "./track-session-store";
import { useReviewStatusStore } from "./use-review-status-store";

const AUTO_LIGHT_PROMPT =
	"Design a complete lighting score for this track. Work section by section: foundation, then movement, then accents. Place clips for the entire track end-to-end.";

/** Track IDs in a currently-running auto-light batch — used to suppress
 * the per-session toast that the global listener would otherwise emit
 * (the batch shows its own aggregate progress toast). */
const inFlight = new Set<string>();

export function isAutoLightInFlight(trackId: string): boolean {
	return inFlight.has(trackId);
}

export type AutoLightArgs = {
	tracks: TrackBrowserRow[];
	venueId: string;
	venueName: string | null;
	userId: string;
};

/**
 * Spawn a background chat session per selected track and prompt it to
 * design a full lighting score. Skips tracks that already have clips on
 * the current venue's score (those are presumed hand-tuned). One sonner
 * toast updates as sessions complete; on success each track is flagged
 * for review (blue dot in the browser) until the user opens it.
 */
export async function autoLightTracks({
	tracks,
	venueId,
	venueName,
	userId,
}: AutoLightArgs): Promise<void> {
	const candidates = tracks.filter((t) => t.venueAnnotationCount === 0);
	const skipped = tracks.length - candidates.length;
	if (candidates.length === 0) {
		toast.info(
			skipped === 1
				? "Track already has a lighting score — nothing to auto-light."
				: `All ${skipped} selected tracks already have lighting scores.`,
		);
		return;
	}

	const toastId = "auto-light-batch";
	const total = candidates.length;
	let done = 0;
	let failed = 0;

	const renderProgress = () =>
		toast.loading(`Auto-lighting ${done}/${total}…`, { id: toastId });
	renderProgress();

	for (const t of candidates) inFlight.add(t.id);

	const sessions = useTrackSessionStore.getState();
	const reviews = useReviewStatusStore.getState();

	await Promise.all(
		candidates.map(async (track) => {
			const trackName =
				track.title || track.filePath.split("/").pop() || "Untitled";
			let turnError: string | null = null;
			const unsubscribe = trackAgent.onSessionFinished((event) => {
				if (event.subjectKey === track.id) turnError = event.error;
			});
			try {
				const result = await sessions.bootstrap({
					trackId: track.id,
					venueId,
					venueName,
					userId,
					trackName,
				});
				if (!result.ok) throw new Error(result.error);

				trackAgent.registerBridge(track.id, trackBridge(track.id));
				await trackAgent.send(track.id, AUTO_LIGHT_PROMPT, {
					venueId,
					scoreId: result.context.scoreId,
					title: trackName,
				});
				if (turnError) throw new Error(turnError);

				reviews.markNeedsReview(track.id, venueId);
			} catch (err) {
				failed += 1;
				console.error(`Auto-light failed for ${trackName}:`, err);
			} finally {
				unsubscribe();
				done += 1;
				inFlight.delete(track.id);
				renderProgress();
			}
		}),
	);

	const succeeded = total - failed;
	if (failed === 0) {
		toast.success(
			succeeded === 1
				? "Auto-lit 1 track — open it to review."
				: `Auto-lit ${succeeded} tracks — open them to review.`,
			{ id: toastId },
		);
	} else if (succeeded === 0) {
		toast.error(`Auto-light failed for all ${failed} tracks.`, { id: toastId });
	} else {
		toast.warning(
			`Auto-lit ${succeeded} of ${total} tracks (${failed} failed).`,
			{ id: toastId },
		);
	}
}

/** Listen for any session that finishes (interactive or background) and
 * surface a toast unless: the user is currently viewing that track in the
 * editor, or the track is part of an active auto-light batch (which has
 * its own aggregate toast). Wired up at module load so it runs once for
 * the lifetime of the app. */
trackAgent.onSessionFinished((event) => {
	const trackId = event.subjectKey;
	if (inFlight.has(trackId)) return;
	if (useTrackEditorStore.getState().trackId === trackId) return;
	if (event.aborted) return;

	const trackName =
		useTrackSessionStore.getState().getContext(trackId)?.trackName ?? "track";
	if (event.error) {
		toast.error(`Copilot errored on "${trackName}"`, {
			description: event.error,
		});
	} else {
		toast.success(`Copilot finished on "${trackName}"`);
	}
});
