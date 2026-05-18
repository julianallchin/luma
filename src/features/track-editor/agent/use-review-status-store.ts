import { create } from "zustand";

const STORAGE_KEY = "luma:track-needs-review";

/** A track-needs-review flag is keyed by `${trackId}:${venueId}` because the
 * same track can have a different score per venue. */
function key(trackId: string, venueId: string): string {
	return `${trackId}:${venueId}`;
}

function readPersisted(): Record<string, true> {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return {};
		const parsed = JSON.parse(raw);
		if (!parsed || typeof parsed !== "object") return {};
		const out: Record<string, true> = {};
		for (const k of Object.keys(parsed)) {
			if (parsed[k] === true) out[k] = true;
		}
		return out;
	} catch {
		return {};
	}
}

function writePersisted(map: Record<string, true>): void {
	try {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
	} catch {
		// localStorage may be unavailable / full
	}
}

type ReviewStatusState = {
	flagged: Record<string, true>;
	markNeedsReview: (trackId: string, venueId: string) => void;
	clearNeedsReview: (trackId: string, venueId: string) => void;
	isFlagged: (trackId: string, venueId: string) => boolean;
};

export const useReviewStatusStore = create<ReviewStatusState>((set, get) => ({
	flagged: readPersisted(),

	markNeedsReview: (trackId, venueId) => {
		const k = key(trackId, venueId);
		const current = get().flagged;
		if (current[k]) return;
		const next = { ...current, [k]: true as const };
		writePersisted(next);
		set({ flagged: next });
	},

	clearNeedsReview: (trackId, venueId) => {
		const k = key(trackId, venueId);
		const current = get().flagged;
		if (!current[k]) return;
		const next = { ...current };
		delete next[k];
		writePersisted(next);
		set({ flagged: next });
	},

	isFlagged: (trackId, venueId) =>
		Boolean(get().flagged[key(trackId, venueId)]),
}));
