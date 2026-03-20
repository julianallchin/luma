import { invoke } from "@tauri-apps/api/core";
import type {
	RekordboxLibraryInfo,
	RekordboxPlaylist,
	RekordboxTrack,
} from "@/bindings/rekordbox";
import type { TrackSummary } from "@/bindings/schema";
import type {
	DjPlaylist,
	DjSourceAdapter,
	DjTrack,
} from "../stores/use-dj-import-store";

function mapTrack(t: RekordboxTrack): DjTrack {
	return {
		key: t.uuid,
		title: t.title,
		artist: t.artist,
		album: t.album,
		bpm: t.bpm,
		duration: t.durationSeconds,
		filename: t.filename,
	};
}

function mapPlaylist(p: RekordboxPlaylist): DjPlaylist {
	return {
		key: p.id,
		title: p.name,
		parentKey: p.parentId,
		trackCount: 0, // TODO: populate from backend
	};
}

export const rekordboxAdapter: DjSourceAdapter = {
	name: "rekordbox",
	label: "Rekordbox",
	progressEvent: "rekordbox-import-progress",

	openLibrary: async () => {
		const info = await invoke<RekordboxLibraryInfo>("rekordbox_open_library");
		const [rawPlaylists, rawTracks] = await Promise.all([
			invoke<RekordboxPlaylist[]>("rekordbox_list_playlists"),
			invoke<RekordboxTrack[]>("rekordbox_list_tracks"),
		]);

		return {
			libraryPath: null, // Rekordbox auto-discovers
			info: { trackCount: info.trackCount },
			playlists: rawPlaylists.map(mapPlaylist),
			tracks: rawTracks.map(mapTrack),
		};
	},

	listTracks: async () => {
		const raw = await invoke<RekordboxTrack[]>("rekordbox_list_tracks");
		return raw.map(mapTrack);
	},

	getPlaylistTracks: async (_libraryPath, playlistKey) => {
		const raw = await invoke<RekordboxTrack[]>(
			"rekordbox_get_playlist_tracks",
			{
				playlistId: playlistKey,
			},
		);
		return raw.map(mapTrack);
	},

	search: async (_libraryPath, query) => {
		const raw = await invoke<RekordboxTrack[]>("rekordbox_search_tracks", {
			query,
		});
		return raw.map(mapTrack);
	},

	importTracks: async (_libraryPath, trackKeys) => {
		return invoke<TrackSummary[]>("rekordbox_import_tracks", {
			trackUuids: trackKeys,
		});
	},
};
