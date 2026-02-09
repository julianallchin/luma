import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";

import type { PatternSummary } from "@/bindings/schema";
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
import { toSnakeCase } from "@/shared/lib/utils";

type CreatePatternDialogProps = {
	trigger: React.ReactNode;
	onCreated?: (pattern: PatternSummary) => void;
};

export function CreatePatternDialog({
	trigger,
	onCreated,
}: CreatePatternDialogProps) {
	const [open, setOpen] = useState(false);
	const [name, setName] = useState("");
	const [description, setDescription] = useState("");
	const [creating, setCreating] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const normalizedName = toSnakeCase(name);

	const handleCreate = async () => {
		if (!normalizedName) return;

		setCreating(true);
		setError(null);
		try {
			const pattern = await invoke<PatternSummary>("create_pattern", {
				name: normalizedName,
				description: description.trim() || null,
			});
			setName("");
			setDescription("");
			setOpen(false);
			onCreated?.(pattern);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setCreating(false);
		}
	};

	const handleOpenChange = (newOpen: boolean) => {
		setOpen(newOpen);
		if (newOpen) {
			setError(null);
		}
	};

	return (
		<Dialog open={open} onOpenChange={handleOpenChange}>
			<DialogTrigger asChild>{trigger}</DialogTrigger>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Create Pattern</DialogTitle>
					<DialogDescription>
						Enter a name and optional description for your pattern.
					</DialogDescription>
				</DialogHeader>
				<div className="grid gap-4 py-4">
					<div className="grid gap-2">
						<Label htmlFor="pattern-name">Name</Label>
						<Input
							id="pattern-name"
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="my_pattern_name"
							onKeyDown={(e) => {
								if (e.key === "Enter" && normalizedName) {
									handleCreate();
								}
							}}
						/>
						{name && name !== normalizedName && (
							<p className="text-xs text-muted-foreground">
								{normalizedName ? (
									<>
										Will be saved as:{" "}
										<code className="bg-muted px-1 rounded">
											{normalizedName}
										</code>
									</>
								) : (
									<span className="text-destructive">
										Name must contain at least one letter or number
									</span>
								)}
							</p>
						)}
					</div>
					<div className="grid gap-2">
						<Label htmlFor="pattern-description">Description</Label>
						<Textarea
							id="pattern-description"
							value={description}
							onChange={(e) => setDescription(e.target.value)}
							placeholder="Optional description"
							rows={3}
						/>
					</div>
					{error && <div className="text-xs text-destructive">{error}</div>}
				</div>
				<DialogFooter>
					<Button
						variant="outline"
						onClick={() => setOpen(false)}
						disabled={creating}
					>
						Cancel
					</Button>
					<Button onClick={handleCreate} disabled={creating || !normalizedName}>
						{creating ? "Creating..." : "Create"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
