import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type {
	EngineDjLibraryInfo,
	EngineDjPlaylist,
	EngineDjTrack,
} from "@/bindings/engine_dj";
import type { TrackSummary } from "@/bindings/schema";
import type {
	DjPlaylist,
	DjSourceAdapter,
	DjTrack,
} from "../stores/use-dj-import-store";

function mapTrack(t: EngineDjTrack): DjTrack {
	return {
		key: String(t.id),
		title: t.title,
		artist: t.artist,
		album: t.album,
		bpm: t.bpmAnalyzed,
		duration: t.length,
		filename: t.filename,
	};
}

function mapPlaylist(p: EngineDjPlaylist): DjPlaylist {
	return {
		key: String(p.id),
		title: p.title,
		parentKey: p.parentId ? String(p.parentId) : null,
		trackCount: p.trackCount,
	};
}

export const engineDjAdapter: DjSourceAdapter = {
	name: "engine_dj",
	label: "Engine DJ",
	progressEvent: "engine-dj-import-progress",

	openLibrary: async () => {
		// Try default path first
		const defaultPath = await invoke<string>("engine_dj_default_library_path");
		let libraryPath: string;
		try {
			await invoke<EngineDjLibraryInfo>("engine_dj_open_library", {
				libraryPath: defaultPath,
			});
			libraryPath = defaultPath;
		} catch {
			const selection = await open({
				multiple: false,
				directory: true,
				title: "Select Engine Library folder",
				defaultPath,
			});
			if (typeof selection !== "string") throw new Error("No library selected");
			libraryPath = selection;
		}

		const info = await invoke<EngineDjLibraryInfo>("engine_dj_open_library", {
			libraryPath,
		});
		const [rawPlaylists, rawTracks] = await Promise.all([
			invoke<EngineDjPlaylist[]>("engine_dj_list_playlists", { libraryPath }),
			invoke<EngineDjTrack[]>("engine_dj_list_tracks", { libraryPath }),
		]);

		return {
			libraryPath,
			info: { trackCount: info.trackCount },
			playlists: rawPlaylists.map(mapPlaylist),
			tracks: rawTracks.map(mapTrack),
		};
	},

	listTracks: async (libraryPath) => {
		const raw = await invoke<EngineDjTrack[]>("engine_dj_list_tracks", {
			libraryPath,
		});
		return raw.map(mapTrack);
	},

	getPlaylistTracks: async (libraryPath, playlistKey) => {
		const raw = await invoke<EngineDjTrack[]>("engine_dj_get_playlist_tracks", {
			libraryPath,
			playlistId: Number(playlistKey),
		});
		return raw.map(mapTrack);
	},

	search: async (libraryPath, query) => {
		const raw = await invoke<EngineDjTrack[]>("engine_dj_search_tracks", {
			libraryPath,
			query,
		});
		return raw.map(mapTrack);
	},

	importTracks: async (libraryPath, trackKeys) => {
		return invoke<TrackSummary[]>("engine_dj_import_tracks", {
			libraryPath,
			trackIds: trackKeys.map(Number),
		});
	},
};
