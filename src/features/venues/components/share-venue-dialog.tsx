import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";

interface ShareVenueDialogProps {
	venueId: string;
	existingCode?: string | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function ShareVenueDialog({
	venueId,
	existingCode,
	open,
	onOpenChange,
}: ShareVenueDialogProps) {
	const [code, setCode] = useState<string | null>(existingCode ?? null);
	const [loading, setLoading] = useState(false);
	const [copied, setCopied] = useState(false);

	const handleGetCode = async () => {
		setLoading(true);
		try {
			const result = await invoke<string>("get_or_create_share_code", {
				venueId,
			});
			setCode(result);
		} catch (err) {
			console.error("Failed to get share code:", err);
		} finally {
			setLoading(false);
		}
	};

	const handleCopy = async () => {
		if (!code) return;
		await navigator.clipboard.writeText(code);
		setCopied(true);
		setTimeout(() => setCopied(false), 2000);
	};

	// Fetch the code when the dialog opens for the first time.
	useEffect(() => {
		if (open && !code && !loading) {
			void handleGetCode();
		}
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [open]);

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Share venue</DialogTitle>
					<DialogDescription>
						Others can join this venue with this code.
					</DialogDescription>
				</DialogHeader>
				<div className="grid gap-3 py-2">
					{loading ? (
						<div className="text-xs text-muted-foreground">Generating…</div>
					) : code ? (
						<button
							type="button"
							onClick={handleCopy}
							className="w-full bg-control border border-control-border px-3 py-3 text-center font-mono text-lg tracking-[0.25em] select-all hover:bg-hover transition-colors cursor-pointer"
						>
							{copied ? (
								<span className="text-xs text-muted-foreground tracking-normal">
									copied to clipboard
								</span>
							) : (
								code
							)}
						</button>
					) : (
						<button
							type="button"
							onClick={handleGetCode}
							disabled={loading}
							className="text-xs text-muted-foreground hover:text-foreground transition-colors"
						>
							generate code
						</button>
					)}
				</div>
			</DialogContent>
		</Dialog>
	);
}
