import { useId, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
	DialogTrigger,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";
import { Textarea } from "@/shared/components/ui/textarea";
import { useVenuesStore } from "../stores/use-venues-store";

interface CreateVenueDialogProps {
	trigger: React.ReactNode;
}

export function CreateVenueDialog({ trigger }: CreateVenueDialogProps) {
	const [open, setOpen] = useState(false);
	const [name, setName] = useState("");
	const [description, setDescription] = useState("");
	const [creating, setCreating] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const nameId = useId();
	const descriptionId = useId();
	const navigate = useNavigate();
	const { createVenue } = useVenuesStore();

	const handleCreate = async () => {
		if (!name.trim()) return;

		setCreating(true);
		setError(null);

		try {
			const venue = await createVenue(
				name.trim(),
				description.trim() || undefined,
			);
			// Reset form
			setName("");
			setDescription("");
			setOpen(false);
			// Navigate to universe designer for this venue
			navigate(`/venue/${venue.id}/universe`);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setCreating(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={setOpen}>
			<DialogTrigger asChild>{trigger}</DialogTrigger>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Create New Venue</DialogTitle>
					<DialogDescription>
						Enter a name for your venue. You can add fixtures and configure the
						DMX patch after creation.
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
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="e.g., Main Stage, Club XYZ"
							onKeyDown={(e) => {
								if (e.key === "Enter" && name.trim()) {
									handleCreate();
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

				<DialogFooter>
					<Button
						variant="outline"
						onClick={() => setOpen(false)}
						disabled={creating}
					>
						Cancel
					</Button>
					<Button onClick={handleCreate} disabled={creating || !name.trim()}>
						{creating ? "Creating..." : "Create Venue"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
