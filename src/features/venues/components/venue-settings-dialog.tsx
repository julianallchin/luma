import { useId, useState } from "react";
import type { Venue } from "@/bindings/venues";
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
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";
import { Textarea } from "@/shared/components/ui/textarea";
import { useVenuesStore } from "../stores/use-venues-store";

interface VenueSettingsDialogProps {
	venue: Venue;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function VenueSettingsDialog({
	venue,
	open,
	onOpenChange,
}: VenueSettingsDialogProps) {
	const [name, setName] = useState(venue.name);
	const [description, setDescription] = useState(venue.description ?? "");
	const [saving, setSaving] = useState(false);
	const [confirmingDelete, setConfirmingDelete] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const nameId = useId();
	const descriptionId = useId();
	const { updateVenue, deleteVenue } = useVenuesStore();

	const handleSave = async () => {
		if (!name.trim()) return;

		setSaving(true);
		setError(null);

		try {
			await updateVenue(venue.id, name.trim(), description.trim() || undefined);
			onOpenChange(false);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setSaving(false);
		}
	};

	const handleDelete = async () => {
		try {
			await deleteVenue(venue.id);
			onOpenChange(false);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		}
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Venue Settings</DialogTitle>
					<DialogDescription>
						Edit your venue details or delete this venue.
					</DialogDescription>
				</DialogHeader>

				<div className="grid gap-4 py-4">
					{error && (
						<div className="bg-destructive/10 p-2 text-xs text-destructive border border-destructive/20 rounded">
							{error}
						</div>
					)}

					<div className="grid gap-2">
						<Label htmlFor={nameId}>Name</Label>
						<Input
							id={nameId}
							autoCapitalize="off"
							autoCorrect="off"
							spellCheck={false}
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="e.g., Main Stage, Club XYZ"
							onKeyDown={(e) => {
								if (e.key === "Enter" && name.trim()) {
									handleSave();
								}
							}}
							autoFocus
						/>
					</div>

					<div className="grid gap-2">
						<Label htmlFor={descriptionId}>Description (optional)</Label>
						<Textarea
							id={descriptionId}
							value={description}
							onChange={(e) => setDescription(e.target.value)}
							placeholder="Notes about this venue..."
							rows={3}
						/>
					</div>
				</div>

				<DialogFooter className="flex !justify-between">
					<Button
						variant="ghost"
						className="text-destructive hover:text-destructive hover:bg-destructive/10"
						onClick={() => setConfirmingDelete(true)}
						disabled={saving}
					>
						Delete Venue
					</Button>
					<div className="flex gap-2">
						<Button
							variant="outline"
							onClick={() => onOpenChange(false)}
							disabled={saving}
						>
							Cancel
						</Button>
						<Button onClick={handleSave} disabled={saving || !name.trim()}>
							{saving ? "Saving..." : "Save"}
						</Button>
					</div>
				</DialogFooter>
			</DialogContent>

			<AlertDialog open={confirmingDelete} onOpenChange={setConfirmingDelete}>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>Delete Venue</AlertDialogTitle>
						<AlertDialogDescription>
							Delete "{venue.name}"? This cannot be undone.
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>Cancel</AlertDialogCancel>
						<AlertDialogAction
							className="bg-destructive text-white hover:bg-destructive/90"
							onClick={handleDelete}
						>
							Delete
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</Dialog>
	);
}
