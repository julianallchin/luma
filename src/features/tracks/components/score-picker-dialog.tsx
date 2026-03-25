import { invoke } from "@tauri-apps/api/core";
import { Plus, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Score, ScoreSummary, TrackBrowserRow } from "@/bindings/schema";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { useTrackEditorStore } from "@/features/track-editor/stores/use-track-editor-store";
import {
	AlertDialog,
	AlertDialogAction,
	AlertDialogCancel,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle,
} from "@/shared/components/ui/alert-dialog";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";

interface ScorePickerDialogProps {
	track: TrackBrowserRow | null;
	venueId: string;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function ScorePickerDialog({
	track,
	venueId,
	open,
	onOpenChange,
}: ScorePickerDialogProps) {
	const [scores, setScores] = useState<ScoreSummary[]>([]);
	const [ready, setReady] = useState(false);
	const [displayNames, setDisplayNames] = useState<Record<string, string>>({});
	const [deleteTarget, setDeleteTarget] = useState<ScoreSummary | null>(null);
	const currentUserId = useAuthStore((s) => s.user?.id ?? null);
	const loadTrack = useTrackEditorStore((s) => s.loadTrack);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);

	const lastTrackRef = useRef(track);
	if (track) lastTrackRef.current = track;
	const stableTrack = track ?? lastTrackRef.current;

	const trackName =
		stableTrack?.title || stableTrack?.filePath.split("/").pop() || "Untitled";

	useEffect(() => {
		if (!open || !track) {
			const t = setTimeout(() => setReady(false), 200);
			return () => clearTimeout(t);
		}
		invoke<ScoreSummary[]>("list_scores_for_track", {
			trackId: track.id,
			venueId,
		})
			.then(async (result) => {
				setScores(result);
				const otherUids = [
					...new Set(
						result
							.map((s) => s.uid)
							.filter((uid): uid is string => !!uid && uid !== currentUserId),
					),
				];
				if (otherUids.length > 0) {
					const names = await invoke<Record<string, string>>(
						"get_display_names",
						{ uids: otherUids },
					);
					setDisplayNames(names);
				}
			})
			.catch((err) => console.error("Failed to list scores:", err))
			.finally(() => setReady(true));
	}, [open, track?.id, venueId, currentUserId]);

	const handleSelectScore = (score: ScoreSummary) => {
		if (!track) return;
		const readOnly = score.uid !== currentUserId;
		void loadPatterns();
		void loadTrack(track.id, trackName, venueId, score.id, readOnly);
		onOpenChange(false);
	};

	const handleConfirmDelete = async () => {
		if (!deleteTarget) return;
		try {
			await invoke("delete_score", { id: deleteTarget.id });
			setScores((prev) => prev.filter((s) => s.id !== deleteTarget.id));
			if (useTrackEditorStore.getState().scoreId === deleteTarget.id) {
				useTrackEditorStore.getState().resetTrack();
			}
		} catch (err) {
			console.error("Failed to delete score:", err);
		} finally {
			setDeleteTarget(null);
		}
	};

	const handleCreateNew = async () => {
		if (!currentUserId || !track) return;
		try {
			const score = await invoke<Score>("create_score", {
				trackId: track.id,
				venueId,
				uid: currentUserId,
				name: null,
			});
			void loadPatterns();
			void loadTrack(track.id, trackName, venueId, score.id, false);
			onOpenChange(false);
		} catch (err) {
			console.error("Failed to create score:", err);
		}
	};

	return (
		<>
			<Dialog open={open && ready} onOpenChange={onOpenChange}>
				<DialogContent className="sm:max-w-md overflow-hidden">
					<DialogHeader className="overflow-hidden">
						<DialogTitle className="truncate pr-6">{trackName}</DialogTitle>
						<DialogDescription>
							{stableTrack?.artist || "Unknown artist"}
						</DialogDescription>
					</DialogHeader>

					<div className="-mx-4 flex flex-col py-2 max-h-64 overflow-y-auto">
						{scores.map((score) => {
							const isOwn = score.uid === currentUserId;
							const author = isOwn
								? "you"
								: score.uid
									? (displayNames[score.uid] ?? "shared")
									: "unknown";
							const date = new Date(score.updatedAt).toLocaleDateString(
								undefined,
								{
									month: "short",
									day: "numeric",
									year: "numeric",
								},
							);
							return (
								// biome-ignore lint/a11y/useSemanticElements: styled card with nested content
								<div
									key={score.id}
									role="button"
									tabIndex={0}
									onClick={() => handleSelectScore(score)}
									onKeyDown={(e) => {
										if (e.key === "Enter" || e.key === " ") {
											e.preventDefault();
											handleSelectScore(score);
										}
									}}
									className="flex items-center justify-between gap-3 px-4 py-2.5 text-left hover:bg-muted transition-colors cursor-pointer"
								>
									<div className="flex flex-col gap-0.5 min-w-0">
										<span className="text-sm font-medium truncate">
											{author}
											{!isOwn && " (read only)"}
										</span>
										<span className="text-xs text-muted-foreground">
											{date}
										</span>
									</div>
									<div className="flex items-center gap-2 shrink-0">
										<span className="text-xs text-muted-foreground">
											{score.annotationCount}{" "}
											{score.annotationCount === 1
												? "annotation"
												: "annotations"}
										</span>
										{isOwn && (
											<button
												type="button"
												onClick={(e) => {
													e.stopPropagation();
													setDeleteTarget(score);
												}}
												className="p-1 rounded hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-colors"
												title="Delete score"
											>
												<Trash2 className="size-3.5" />
											</button>
										)}
									</div>
								</div>
							);
						})}
						{scores.length === 0 && (
							<div className="text-xs text-muted-foreground text-center py-4">
								No scores yet for this track.
							</div>
						)}
					</div>

					<Button
						variant="outline"
						className="w-full"
						onClick={handleCreateNew}
					>
						<Plus className="size-4" />
						Create new score
					</Button>
				</DialogContent>
			</Dialog>

			<AlertDialog
				open={!!deleteTarget}
				onOpenChange={(open) => {
					if (!open) setDeleteTarget(null);
				}}
			>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>Delete score</AlertDialogTitle>
						<AlertDialogDescription>
							This will remove all annotations in this score. This cannot be
							undone.
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>Cancel</AlertDialogCancel>
						<AlertDialogAction
							onClick={handleConfirmDelete}
							className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
						>
							Delete
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</>
	);
}
