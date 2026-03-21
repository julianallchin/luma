import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";

interface ShareVenueDialogProps {
	venueId: number;
	existingCode?: string | null;
}

export function ShareVenueDialog({
	venueId,
	existingCode,
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

	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="text-xs opacity-50 hover:opacity-100 transition-opacity"
					onClick={() => {
						if (!code) handleGetCode();
					}}
				>
					[ share ]
				</button>
			</PopoverTrigger>
			<PopoverContent className="w-56" align="end">
				<div className="grid gap-3">
					<p className="text-xs text-muted-foreground">
						Others can join this venue with this code.
					</p>
					{loading ? (
						<div className="text-xs text-muted-foreground">Generating...</div>
					) : code ? (
						<button
							type="button"
							onClick={handleCopy}
							className="w-full bg-input border px-3 py-2.5 rounded text-center font-mono text-lg tracking-[0.25em] select-all hover:bg-muted transition-colors cursor-pointer"
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
			</PopoverContent>
		</Popover>
	);
}
