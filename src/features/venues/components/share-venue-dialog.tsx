import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/shared/components/ui/button";
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
				<Button
					variant="ghost"
					size="sm"
					className="text-xs h-7"
					onClick={() => {
						if (!code) handleGetCode();
					}}
				>
					share
				</Button>
			</PopoverTrigger>
			<PopoverContent className="w-64" align="end">
				<div className="grid gap-3">
					<div className="space-y-1">
						<h4 className="text-sm font-medium">Share Venue</h4>
						<p className="text-xs text-muted-foreground">
							Others can join this venue with this code.
						</p>
					</div>
					{loading ? (
						<div className="text-xs text-muted-foreground">Generating...</div>
					) : code ? (
						<div className="flex items-center gap-2">
							<code className="flex-1 bg-muted px-3 py-2 rounded text-center font-mono text-lg tracking-widest select-all">
								{code}
							</code>
							<Button
								variant="outline"
								size="sm"
								className="text-xs shrink-0"
								onClick={handleCopy}
							>
								{copied ? "copied" : "copy"}
							</Button>
						</div>
					) : (
						<Button
							variant="outline"
							size="sm"
							onClick={handleGetCode}
							disabled={loading}
						>
							Generate Code
						</Button>
					)}
				</div>
			</PopoverContent>
		</Popover>
	);
}
