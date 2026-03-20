import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";
import type { TrackSummary } from "@/bindings/schema";

/**
 * Normalized track shape shared across DJ software sources.
 * Each source adapter maps its native types into this shape.
 */
export interface DjTrack {
	/** Opaque string key unique within this source */
	key: string;
	title: string | null;
	artist: string | null;
	album: string | null;
	bpm: number | null;
	/** Duration in seconds */
	duration: number | null;
	filename: string | null;
}

export interface DjPlaylist {
	key: string;
	title: string;
	parentKey: string | null;
	trackCount: number;
}

export interface DjLibraryInfo {
	trackCount: number;
}

interface ImportProgressEvent {
	done: number;
	total: number;
	currentTrack: string | null;
	phase: string;
	error: string | null;
}

type ActiveView = "all" | "playlist";

/**
 * Source adapter — each DJ software implements these hooks.
 * Commands are invoked via Tauri; the adapter just specifies the names + arg shapes.
 */
export interface DjSourceAdapter {
	name: string;
	/** Label shown in the dialog header */
	label: string;
	/** Tauri event name for import progress */
	progressEvent: string;

	/**
	 * Open the library. May auto-discover or prompt the user.
	 * Returns a path/identifier for subsequent calls (null if none needed, e.g. Rekordbox).
	 */
	openLibrary: () => Promise<{
		libraryPath: string | null;
		info: DjLibraryInfo;
		playlists: DjPlaylist[];
		tracks: DjTrack[];
	}>;

	listTracks: (libraryPath: string | null) => Promise<DjTrack[]>;
	getPlaylistTracks: (
		libraryPath: string | null,
		playlistKey: string,
	) => Promise<DjTrack[]>;
	search: (libraryPath: string | null, query: string) => Promise<DjTrack[]>;

	/**
	 * Import tracks by their keys. Returns imported TrackSummary[].
	 */
	importTracks: (
		libraryPath: string | null,
		trackKeys: string[],
	) => Promise<TrackSummary[]>;
}

interface DjImportState {
	source: DjSourceAdapter | null;
	libraryPath: string | null;
	libraryInfo: DjLibraryInfo | null;
	playlists: DjPlaylist[];
	tracks: DjTrack[];
	selectedKeys: Set<string>;
	activeView: ActiveView;
	activePlaylistKey: string | null;
	searchQuery: string;
	importing: boolean;
	importProgress: { done: number; total: number };
	currentImportTrack: string | null;
	loading: boolean;
	error: string | null;

	/** Open the dialog for a given source */
	openForSource: (adapter: DjSourceAdapter) => Promise<void>;
	selectPlaylist: (playlistKey: string | null) => Promise<void>;
	search: (query: string) => Promise<void>;
	toggleTrackSelection: (key: string) => void;
	selectAllTracks: () => void;
	clearSelection: () => void;
	importSelected: () => Promise<TrackSummary[]>;
	reset: () => void;
}

export const useDjImportStore = create<DjImportState>((set, get) => ({
	source: null,
	libraryPath: null,
	libraryInfo: null,
	playlists: [],
	tracks: [],
	selectedKeys: new Set(),
	activeView: "all",
	activePlaylistKey: null,
	searchQuery: "",
	importing: false,
	importProgress: { done: 0, total: 0 },
	currentImportTrack: null,
	loading: false,
	error: null,

	openForSource: async (adapter) => {
		set({
			source: adapter,
			loading: true,
			error: null,
			libraryPath: null,
			libraryInfo: null,
			playlists: [],
			tracks: [],
			selectedKeys: new Set(),
			activeView: "all",
			activePlaylistKey: null,
			searchQuery: "",
		});
		try {
			const result = await adapter.openLibrary();
			set({
				libraryPath: result.libraryPath,
				libraryInfo: result.info,
				playlists: result.playlists,
				tracks: result.tracks,
				loading: false,
			});
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	selectPlaylist: async (playlistKey) => {
		const { source, libraryPath } = get();
		if (!source) return;

		set({ loading: true, error: null });
		try {
			let tracks: DjTrack[];
			if (playlistKey === null) {
				tracks = await source.listTracks(libraryPath);
				set({
					tracks,
					activeView: "all",
					activePlaylistKey: null,
					loading: false,
					selectedKeys: new Set(),
				});
			} else {
				tracks = await source.getPlaylistTracks(libraryPath, playlistKey);
				set({
					tracks,
					activeView: "playlist",
					activePlaylistKey: playlistKey,
					loading: false,
					selectedKeys: new Set(),
				});
			}
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	search: async (query) => {
		const { source, libraryPath } = get();
		set({ searchQuery: query });
		if (!source) return;

		if (!query.trim()) {
			const { activeView, activePlaylistKey } = get();
			if (activeView === "playlist" && activePlaylistKey !== null) {
				await get().selectPlaylist(activePlaylistKey);
			} else {
				await get().selectPlaylist(null);
			}
			return;
		}

		set({ loading: true, error: null });
		try {
			const tracks = await source.search(libraryPath, query);
			set({ tracks, loading: false, selectedKeys: new Set() });
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	toggleTrackSelection: (key) => {
		const selected = new Set(get().selectedKeys);
		if (selected.has(key)) {
			selected.delete(key);
		} else {
			selected.add(key);
		}
		set({ selectedKeys: selected });
	},

	selectAllTracks: () => {
		const keys = new Set(get().tracks.map((t) => t.key));
		set({ selectedKeys: keys });
	},

	clearSelection: () => {
		set({ selectedKeys: new Set() });
	},

	importSelected: async () => {
		const { source, libraryPath, selectedKeys } = get();
		if (!source || selectedKeys.size === 0) return [];

		const trackKeys = Array.from(selectedKeys);
		set({
			importing: true,
			importProgress: { done: 0, total: trackKeys.length },
			currentImportTrack: null,
			error: null,
		});

		let unlisten: UnlistenFn | null = null;
		try {
			unlisten = await listen<ImportProgressEvent>(
				source.progressEvent,
				(event) => {
					set({
						importProgress: {
							done: event.payload.done,
							total: event.payload.total,
						},
						currentImportTrack: event.payload.currentTrack,
					});
				},
			);

			const imported = await source.importTracks(libraryPath, trackKeys);
			set({
				importing: false,
				importProgress: { done: imported.length, total: trackKeys.length },
				currentImportTrack: null,
				selectedKeys: new Set(),
			});
			return imported;
		} catch (err) {
			set({
				importing: false,
				currentImportTrack: null,
				error: err instanceof Error ? err.message : String(err),
			});
			return [];
		} finally {
			unlisten?.();
		}
	},

	reset: () => {
		set({
			source: null,
			libraryPath: null,
			libraryInfo: null,
			playlists: [],
			tracks: [],
			selectedKeys: new Set(),
			activeView: "all",
			activePlaylistKey: null,
			searchQuery: "",
			importing: false,
			importProgress: { done: 0, total: 0 },
			currentImportTrack: null,
			loading: false,
			error: null,
		});
	},
}));
