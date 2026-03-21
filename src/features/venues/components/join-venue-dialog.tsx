import { REGEXP_ONLY_DIGITS_AND_CHARS } from "input-otp";
import { useState } from "react";
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
import {
	InputOTP,
	InputOTPGroup,
	InputOTPSlot,
} from "@/shared/components/ui/input-otp";
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

	const navigate = useNavigate();
	const { joinVenue } = useVenuesStore();

	const handleJoin = async () => {
		if (code.length !== 8) return;

		setJoining(true);
		setError(null);

		try {
			const venue = await joinVenue(code);
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
		<Dialog
			open={open}
			onOpenChange={(v) => {
				setOpen(v);
				if (!v) {
					setCode("");
					setError(null);
				}
			}}
		>
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
						<Label>Share Code</Label>
						<InputOTP
							maxLength={8}
							pattern={REGEXP_ONLY_DIGITS_AND_CHARS}
							value={code}
							onChange={setCode}
							onComplete={handleJoin}
							autoFocus
						>
							<InputOTPGroup>
								<InputOTPSlot index={0} />
								<InputOTPSlot index={1} />
								<InputOTPSlot index={2} />
								<InputOTPSlot index={3} />
								<InputOTPSlot index={4} />
								<InputOTPSlot index={5} />
								<InputOTPSlot index={6} />
								<InputOTPSlot index={7} />
							</InputOTPGroup>
						</InputOTP>
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
					<Button onClick={handleJoin} disabled={joining || code.length !== 8}>
						{joining ? "Joining..." : "Join Venue"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
