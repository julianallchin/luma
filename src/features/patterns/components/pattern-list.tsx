import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useState } from "react";

import type { PatternSummary } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { usePatternsStore } from "@/features/patterns/stores/use-patterns-store";
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

export function PatternList() {
	const { patterns, loading, error: storeError, refresh } = usePatternsStore();
	const [error, setError] = useState<string | null>(null);
	const [dialogOpen, setDialogOpen] = useState(false);
	const [name, setName] = useState("");
	const [description, setDescription] = useState("");
	const [creating, setCreating] = useState(false);
	const setView = useAppViewStore((state) => state.setView);
	const nameId = useId();
	const descriptionId = useId();

	const displayError = error ?? storeError;

	useEffect(() => {
		// Only fetch if we have no patterns and aren't currently loading
		// This prevents re-fetching on tab switches if data exists
		if (patterns.length === 0) {
			refresh().catch((err) => {
				console.error("Failed to load patterns", err);
			});
		}
	}, [refresh, patterns.length]);

	const handleCreate = async () => {
		if (!name.trim()) return;

		setCreating(true);
		setError(null);
		try {
			await invoke<PatternSummary>("create_pattern", {
				name: name.trim(),
				description: description.trim() || null,
			});
			setName("");
			setDescription("");
			setDialogOpen(false);
			// Force refresh after creating
			await refresh();
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setCreating(false);
		}
	};

	const handlePatternClick = (pattern: PatternSummary) => {
		setView({ type: "pattern", patternId: pattern.id, name: pattern.name });
	};

	if (loading) {
		return (
			<div className="p-8 text-xs text-muted-foreground">
				Loading patterns...
			</div>
		);
	}

	return (
		<div className="flex flex-col h-full">
			<div className="flex items-center justify-between p-2 border-b border-border/50 min-h-[40px]">
				<div className="text-xs text-muted-foreground px-2">
					{patterns.length} patterns
				</div>
				<Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
					<DialogTrigger asChild>
						<Button variant="ghost" size="sm" className="h-7 text-xs px-2">
							Create Pattern
						</Button>
					</DialogTrigger>
					<DialogContent>
						<DialogHeader>
							<DialogTitle>Create New Pattern</DialogTitle>
							<DialogDescription>
								Enter a name and optional description for your pattern.
							</DialogDescription>
						</DialogHeader>
						<div className="grid gap-4 py-4">
							<div className="grid gap-2">
								<Label htmlFor={nameId}>Name</Label>
								<Input
									id={nameId}
									value={name}
									onChange={(e) => setName(e.target.value)}
									placeholder="Pattern name"
									onKeyDown={(e) => {
										if (e.key === "Enter" && name.trim()) {
											handleCreate();
										}
									}}
								/>
							</div>
							<div className="grid gap-2">
								<Label htmlFor={descriptionId}>Description</Label>
								<Textarea
									id={descriptionId}
									value={description}
									onChange={(e) => setDescription(e.target.value)}
									placeholder="Optional description"
									rows={3}
								/>
							</div>
						</div>
						<DialogFooter>
							<Button
								variant="outline"
								onClick={() => setDialogOpen(false)}
								disabled={creating}
							>
								Cancel
							</Button>
							<Button
								onClick={handleCreate}
								disabled={creating || !name.trim()}
							>
								{creating ? "Creating..." : "Create"}
							</Button>
						</DialogFooter>
					</DialogContent>
				</Dialog>
			</div>

			{displayError && (
				<div className="bg-destructive/10 p-2 text-xs text-destructive border-b border-destructive/20">
					{displayError}
				</div>
			)}

			<div className="grid grid-cols-[1fr_2fr_120px] gap-4 px-4 py-2 text-xs font-medium text-muted-foreground border-b border-border/50 select-none">
				<div>NAME</div>
				<div>DESCRIPTION</div>
				<div className="text-right">MODIFIED</div>
			</div>

			<div className="flex-1 overflow-y-auto">
				{patterns.length === 0 ? (
					<div className="flex flex-col items-center justify-center h-32 text-xs text-muted-foreground">
						No patterns created yet
					</div>
				) : (
					patterns.map((pattern) => (
						<button
							key={pattern.id}
							type="button"
							onClick={() => handlePatternClick(pattern)}
							className="w-full grid grid-cols-[1fr_2fr_120px] gap-4 px-4 py-1.5 text-sm hover:bg-muted items-center group cursor-pointer text-left"
						>
							<div className="font-medium truncate text-foreground/90">
								{pattern.name}
							</div>
							<div className="text-xs text-muted-foreground truncate">
								{pattern.description}
							</div>
							<div className="text-xs text-muted-foreground text-right font-mono opacity-70">
								{new Date(pattern.updatedAt).toLocaleDateString()}
							</div>
						</button>
					))
				)}
			</div>
		</div>
	);
}
