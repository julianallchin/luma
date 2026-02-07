import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";
import type {
	EngineDjLibraryInfo,
	EngineDjPlaylist,
	EngineDjTrack,
} from "@/bindings/engine_dj";
import type { TrackSummary } from "@/bindings/schema";

type ActiveView = "all" | "playlist";

interface EngineDjState {
	libraryPath: string | null;
	libraryInfo: EngineDjLibraryInfo | null;
	playlists: EngineDjPlaylist[];
	tracks: EngineDjTrack[];
	selectedTrackIds: Set<number>;
	activeView: ActiveView;
	activePlaylistId: number | null;
	searchQuery: string;
	importing: boolean;
	importProgress: { done: number; total: number };
	loading: boolean;
	error: string | null;

	openLibrary: () => Promise<void>;
	openLibraryAt: (path: string) => Promise<void>;
	selectPlaylist: (playlistId: number | null) => Promise<void>;
	search: (query: string) => Promise<void>;
	toggleTrackSelection: (trackId: number) => void;
	selectAllTracks: () => void;
	clearSelection: () => void;
	importSelected: () => Promise<TrackSummary[]>;
	reset: () => void;
}

export const useEngineDjStore = create<EngineDjState>((set, get) => ({
	libraryPath: null,
	libraryInfo: null,
	playlists: [],
	tracks: [],
	selectedTrackIds: new Set(),
	activeView: "all",
	activePlaylistId: null,
	searchQuery: "",
	importing: false,
	importProgress: { done: 0, total: 0 },
	loading: false,
	error: null,

	openLibrary: async () => {
		try {
			// Try default path first
			const defaultPath = await invoke<string>(
				"engine_dj_default_library_path",
			);

			// Check if default exists by trying to open it
			try {
				await get().openLibraryAt(defaultPath);
				return;
			} catch {
				// Default not found, prompt user to pick
			}

			const selection = await open({
				multiple: false,
				directory: true,
				title: "Select Engine Library folder",
				defaultPath,
			});
			if (typeof selection !== "string") return;

			await get().openLibraryAt(selection);
		} catch (err) {
			set({ error: err instanceof Error ? err.message : String(err) });
		}
	},

	openLibraryAt: async (path: string) => {
		set({ loading: true, error: null });
		try {
			const info = await invoke<EngineDjLibraryInfo>(
				"engine_dj_open_library",
				{ libraryPath: path },
			);
			const [playlists, tracks] = await Promise.all([
				invoke<EngineDjPlaylist[]>("engine_dj_list_playlists", {
					libraryPath: path,
				}),
				invoke<EngineDjTrack[]>("engine_dj_list_tracks", {
					libraryPath: path,
				}),
			]);
			set({
				libraryPath: path,
				libraryInfo: info,
				playlists,
				tracks,
				loading: false,
				activeView: "all",
				activePlaylistId: null,
				selectedTrackIds: new Set(),
				searchQuery: "",
			});
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
			throw err;
		}
	},

	selectPlaylist: async (playlistId: number | null) => {
		const { libraryPath } = get();
		if (!libraryPath) return;

		set({ loading: true, error: null });
		try {
			if (playlistId === null) {
				const tracks = await invoke<EngineDjTrack[]>(
					"engine_dj_list_tracks",
					{ libraryPath },
				);
				set({
					tracks,
					activeView: "all",
					activePlaylistId: null,
					loading: false,
					selectedTrackIds: new Set(),
				});
			} else {
				const tracks = await invoke<EngineDjTrack[]>(
					"engine_dj_get_playlist_tracks",
					{ libraryPath, playlistId },
				);
				set({
					tracks,
					activeView: "playlist",
					activePlaylistId: playlistId,
					loading: false,
					selectedTrackIds: new Set(),
				});
			}
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	search: async (query: string) => {
		const { libraryPath } = get();
		set({ searchQuery: query });
		if (!libraryPath) return;

		if (!query.trim()) {
			// Reset to current view
			const { activeView, activePlaylistId } = get();
			if (activeView === "playlist" && activePlaylistId !== null) {
				await get().selectPlaylist(activePlaylistId);
			} else {
				await get().selectPlaylist(null);
			}
			return;
		}

		set({ loading: true, error: null });
		try {
			const tracks = await invoke<EngineDjTrack[]>(
				"engine_dj_search_tracks",
				{ libraryPath, query },
			);
			set({ tracks, loading: false, selectedTrackIds: new Set() });
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	toggleTrackSelection: (trackId: number) => {
		const selected = new Set(get().selectedTrackIds);
		if (selected.has(trackId)) {
			selected.delete(trackId);
		} else {
			selected.add(trackId);
		}
		set({ selectedTrackIds: selected });
	},

	selectAllTracks: () => {
		const ids = new Set(get().tracks.map((t) => t.id));
		set({ selectedTrackIds: ids });
	},

	clearSelection: () => {
		set({ selectedTrackIds: new Set() });
	},

	importSelected: async () => {
		const { libraryPath, selectedTrackIds } = get();
		if (!libraryPath || selectedTrackIds.size === 0) return [];

		const trackIds = Array.from(selectedTrackIds);
		set({
			importing: true,
			importProgress: { done: 0, total: trackIds.length },
			error: null,
		});

		try {
			const imported = await invoke<TrackSummary[]>(
				"engine_dj_import_tracks",
				{ libraryPath, trackIds },
			);
			set({
				importing: false,
				importProgress: { done: imported.length, total: trackIds.length },
				selectedTrackIds: new Set(),
			});
			return imported;
		} catch (err) {
			set({
				importing: false,
				error: err instanceof Error ? err.message : String(err),
			});
			return [];
		}
	},

	reset: () => {
		set({
			libraryPath: null,
			libraryInfo: null,
			playlists: [],
			tracks: [],
			selectedTrackIds: new Set(),
			activeView: "all",
			activePlaylistId: null,
			searchQuery: "",
			importing: false,
			importProgress: { done: 0, total: 0 },
			loading: false,
			error: null,
		});
	},
}));
