import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { toast } from "sonner";
import type { TrackBrowserRow } from "@/bindings/schema";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";

interface Props {
	track: TrackBrowserRow | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function EditMetadataDialog({ track, open, onOpenChange }: Props) {
	const [title, setTitle] = useState(track?.title ?? "");
	const [artist, setArtist] = useState(track?.artist ?? "");
	const [album, setAlbum] = useState(track?.album ?? "");
	const [saving, setSaving] = useState(false);

	// Reset fields when a new track is passed in
	const handleOpenChange = (next: boolean) => {
		if (next && track) {
			setTitle(track.title ?? "");
			setArtist(track.artist ?? "");
			setAlbum(track.album ?? "");
		}
		onOpenChange(next);
	};

	const handleSave = async () => {
		if (!track) return;
		setSaving(true);
		try {
			await invoke("update_track_metadata", {
				trackId: track.id,
				title: title.trim() || null,
				artist: artist.trim() || null,
				album: album.trim() || null,
			});
			onOpenChange(false);
		} catch (err) {
			console.error("Failed to update track metadata:", err);
			toast.error("Failed to save metadata");
		} finally {
			setSaving(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={handleOpenChange}>
			<DialogContent className="sm:max-w-md">
				<DialogHeader>
					<DialogTitle>Edit Metadata</DialogTitle>
				</DialogHeader>
				<div className="flex flex-col gap-4 py-2">
					<div className="flex flex-col gap-1.5">
						<Label htmlFor="em-title">Title</Label>
						<Input
							id="em-title"
							value={title}
							onChange={(e) => setTitle(e.target.value)}
							placeholder="Track title"
						/>
					</div>
					<div className="flex flex-col gap-1.5">
						<Label htmlFor="em-artist">Artist</Label>
						<Input
							id="em-artist"
							value={artist}
							onChange={(e) => setArtist(e.target.value)}
							placeholder="Artist name"
						/>
					</div>
					<div className="flex flex-col gap-1.5">
						<Label htmlFor="em-album">Album</Label>
						<Input
							id="em-album"
							value={album}
							onChange={(e) => setAlbum(e.target.value)}
							placeholder="Album name"
						/>
					</div>
				</div>
				<DialogFooter>
					<Button
						variant="outline"
						onClick={() => onOpenChange(false)}
						disabled={saving}
					>
						Cancel
					</Button>
					<Button onClick={handleSave} disabled={saving}>
						Save
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
