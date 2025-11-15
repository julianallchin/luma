import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";

import type { PatternSummary } from "@/bindings/schema";
import { Button } from "@/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
	DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { useAppViewStore } from "@/useAppViewStore";

export function PatternList() {
	const [patterns, setPatterns] = useState<PatternSummary[]>([]);
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const [dialogOpen, setDialogOpen] = useState(false);
	const [name, setName] = useState("");
	const [description, setDescription] = useState("");
	const [creating, setCreating] = useState(false);
	const setView = useAppViewStore((state) => state.setView);

	const loadPatterns = useCallback(async () => {
		setLoading(true);
		setError(null);
		try {
			const result = await invoke<PatternSummary[]>("list_patterns");
			setPatterns(result);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setLoading(false);
		}
	}, []);

	useEffect(() => {
		loadPatterns();
	}, [loadPatterns]);

	const handleCreate = async () => {
		if (!name.trim()) return;

		setCreating(true);
		try {
			await invoke<PatternSummary>("create_pattern", {
				name: name.trim(),
				description: description.trim() || null,
			});
			setName("");
			setDescription("");
			setDialogOpen(false);
			await loadPatterns();
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
			<div className="flex h-full items-center justify-center">
				<p className="text-muted-foreground">Loading patterns...</p>
			</div>
		);
	}

	return (
		<div className="flex h-full min-h-0 flex-col">
			<div className="border-b border-border bg-background p-4">
				<div className="flex items-center justify-between">
					<h1 className="text-2xl font-semibold">Patterns</h1>
					<Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
						<DialogTrigger asChild>
							<Button>Create Pattern</Button>
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
									<Label htmlFor="name">Name</Label>
									<Input
										id="name"
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
									<Label htmlFor="description">Description</Label>
									<Textarea
										id="description"
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
			</div>

			{error && (
				<div className="border-b border-destructive bg-destructive/10 p-4">
					<p className="text-sm text-destructive">{error}</p>
				</div>
			)}

			<div className="flex-1 overflow-y-auto p-4">
				{patterns.length === 0 ? (
					<div className="flex h-full items-center justify-center">
						<p className="text-muted-foreground">
							No patterns yet. Create your first pattern to get started.
						</p>
					</div>
				) : (
					<div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
						{patterns.map((pattern) => (
							<button
								key={pattern.id}
								type="button"
								onClick={() => handlePatternClick(pattern)}
								className="group rounded-lg border border-border bg-card p-4 text-left transition-colors hover:bg-accent"
							>
								<h3 className="font-semibold group-hover:text-accent-foreground">
									{pattern.name}
								</h3>
								{pattern.description && (
									<p className="mt-2 text-sm text-muted-foreground line-clamp-2">
										{pattern.description}
									</p>
								)}
								<p className="mt-2 text-xs text-muted-foreground">
									Updated {new Date(pattern.updatedAt).toLocaleDateString()}
								</p>
							</button>
						))}
					</div>
				)}
			</div>
		</div>
	);
}
