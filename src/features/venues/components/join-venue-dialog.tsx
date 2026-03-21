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
import { useVenuesStore } from "../stores/use-venues-store";

interface JoinVenueDialogProps {
	trigger: React.ReactNode;
}

export function JoinVenueDialog({ trigger }: JoinVenueDialogProps) {
	const [open, setOpen] = useState(false);
	const [code, setCode] = useState("");
	const [joining, setJoining] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const codeId = useId();
	const navigate = useNavigate();
	const { joinVenue } = useVenuesStore();

	const handleJoin = async () => {
		const trimmed = code.trim();
		if (!trimmed) return;

		setJoining(true);
		setError(null);

		try {
			const venue = await joinVenue(trimmed);
			setCode("");
			setOpen(false);
			navigate(`/venue/${venue.id}/universe`);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setJoining(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={setOpen}>
			<DialogTrigger asChild>{trigger}</DialogTrigger>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Join Venue</DialogTitle>
					<DialogDescription>
						Enter the share code from a venue owner to join their venue.
					</DialogDescription>
				</DialogHeader>

				<div className="grid gap-4 py-4">
					{error && (
						<div className="bg-destructive/10 p-2 text-xs text-destructive border border-destructive/20 rounded">
							{error}
						</div>
					)}

					<div className="grid gap-2">
						<Label htmlFor={codeId}>Share Code</Label>
						<Input
							id={codeId}
							autoCapitalize="off"
							autoCorrect="off"
							spellCheck={false}
							value={code}
							onChange={(e) => setCode(e.target.value)}
							placeholder="e.g., a3kX9mBv"
							className="font-mono tracking-wider"
							onKeyDown={(e) => {
								if (e.key === "Enter" && code.trim()) {
									handleJoin();
								}
							}}
							autoFocus
						/>
					</div>
				</div>

				<DialogFooter>
					<Button
						variant="outline"
						onClick={() => setOpen(false)}
						disabled={joining}
					>
						Cancel
					</Button>
					<Button onClick={handleJoin} disabled={joining || !code.trim()}>
						{joining ? "Joining..." : "Join Venue"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
