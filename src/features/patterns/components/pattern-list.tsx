import { invoke } from "@tauri-apps/api/core";
import type { DragEvent } from "react";
import { useEffect, useId, useMemo, useState } from "react";
import { useLocation, useNavigate, useSearchParams } from "react-router-dom";

import type { PatternSummary } from "@/bindings/schema";
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

type PatternWithCategory = PatternSummary & {
	categoryId?: number | null;
	categoryName?: string | null;
};

type PatternCategory = {
	id: number;
	name: string;
	createdAt: string;
	updatedAt: string;
};

type SelectedCategory = "all" | "uncategorized" | number;

const parseSelectedCategory = (raw: string | null): SelectedCategory => {
	if (!raw || raw === "all") return "all";
	if (raw === "uncategorized") return "uncategorized";
	const asNumber = Number(raw);
	return Number.isFinite(asNumber) ? asNumber : "all";
};

export function PatternList() {
	const { patterns, loading, error: storeError, refresh } = usePatternsStore();
	const navigate = useNavigate();
	const location = useLocation();
	const [searchParams, setSearchParams] = useSearchParams();
	const [error, setError] = useState<string | null>(null);
	const [dialogOpen, setDialogOpen] = useState(false);
	const [name, setName] = useState("");
	const [description, setDescription] = useState("");
	const [creating, setCreating] = useState(false);
	const [categories, setCategories] = useState<PatternCategory[]>([]);
	const [categoriesLoading, setCategoriesLoading] = useState(false);
	const [categoryError, setCategoryError] = useState<string | null>(null);
	const [selectedCategory, setSelectedCategory] = useState<SelectedCategory>(
		parseSelectedCategory(searchParams.get("category")),
	);
	const [categoryDialogOpen, setCategoryDialogOpen] = useState(false);
	const [categoryName, setCategoryName] = useState("");
	const [creatingCategory, setCreatingCategory] = useState(false);
	const [dragOverCategory, setDragOverCategory] =
		useState<SelectedCategory | null>(null);
	const nameId = useId();
	const descriptionId = useId();
	const categoryNameId = useId();

	const displayError = error ?? storeError ?? categoryError;

	useEffect(() => {
		// Only fetch if we have no patterns and aren't currently loading
		// This prevents re-fetching on tab switches if data exists
		if (patterns.length === 0) {
			refresh().catch((err) => {
				console.error("Failed to load patterns", err);
			});
		}
	}, [refresh, patterns.length]);

	useEffect(() => {
		const loadCategories = async () => {
			setCategoriesLoading(true);
			setCategoryError(null);
			try {
				const fresh = await invoke<PatternCategory[]>(
					"list_pattern_categories",
				);
				setCategories(fresh);
			} catch (err) {
				setCategoryError(err instanceof Error ? err.message : String(err));
			} finally {
				setCategoriesLoading(false);
			}
		};
		loadCategories();
	}, []);

	useEffect(() => {
		setSelectedCategory(parseSelectedCategory(searchParams.get("category")));
	}, [searchParams]);

	const setSelectedCategoryWithUrl = (category: SelectedCategory) => {
		setSelectedCategory(category);
		const next = new URLSearchParams(searchParams);
		if (category === "all") {
			next.delete("category");
		} else {
			next.set("category", String(category));
		}
		setSearchParams(next, { replace: true });
	};

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

	const handleCreateCategory = async () => {
		if (!categoryName.trim()) return;
		setCreatingCategory(true);
		setCategoryError(null);
		try {
			const created = await invoke<PatternCategory>("create_pattern_category", {
				name: categoryName.trim(),
			});
			setCategories((prev) =>
				[...prev, created].sort((a, b) => a.name.localeCompare(b.name)),
			);
			setCategoryName("");
			setCategoryDialogOpen(false);
			setSelectedCategoryWithUrl(created.id);
		} catch (err) {
			setCategoryError(err instanceof Error ? err.message : String(err));
		} finally {
			setCreatingCategory(false);
		}
	};

	const handlePatternClick = (pattern: PatternSummary) => {
		navigate(`/pattern/${pattern.id}`, {
			state: {
				name: pattern.name,
				from: `${location.pathname}${location.search}`,
			},
		});
	};

	const patternsWithCategory = patterns as PatternWithCategory[];

	const filteredPatterns = useMemo(() => {
		if (selectedCategory === "all") return patternsWithCategory;
		if (selectedCategory === "uncategorized") {
			return patternsWithCategory.filter(
				(p) => p.categoryId == null || p.categoryId === undefined,
			);
		}
		return patternsWithCategory.filter(
			(p) => p.categoryId === selectedCategory,
		);
	}, [patternsWithCategory, selectedCategory]);

	const selectedCategoryLabel = useMemo(() => {
		if (selectedCategory === "all") return "All Patterns";
		if (selectedCategory === "uncategorized") return "Uncategorized";
		return (
			categories.find((c) => c.id === selectedCategory)?.name ?? "Category"
		);
	}, [selectedCategory, categories]);

	const handleDragStart =
		(pattern: PatternWithCategory) => (e: DragEvent<HTMLButtonElement>) => {
			e.dataTransfer.setData("application/x-luma-pattern", String(pattern.id));
			e.dataTransfer.effectAllowed = "copy";
		};

	const handleDropOnCategory =
		(categoryId: SelectedCategory) =>
		async (e: DragEvent<HTMLButtonElement>) => {
			e.preventDefault();
			setDragOverCategory(null);
			const raw = e.dataTransfer.getData("application/x-luma-pattern");
			const patternId = Number(raw);
			if (!patternId) return;
			try {
				await invoke("set_pattern_category", {
					patternId,
					categoryId:
						categoryId === "uncategorized" || categoryId === "all"
							? null
							: categoryId,
				});
				await refresh();
			} catch (err) {
				setError(err instanceof Error ? err.message : String(err));
			}
		};

	const allowCategoryDrop =
		(categoryId: SelectedCategory) => (e: DragEvent<HTMLButtonElement>) => {
			e.preventDefault();
			e.dataTransfer.dropEffect = "copy";
			setDragOverCategory(categoryId);
		};

	const handleCategoryDragLeave = (categoryId: SelectedCategory) => () => {
		setDragOverCategory((current) => (current === categoryId ? null : current));
	};

	const handleDragEnd = () => {
		setDragOverCategory(null);
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

			<div className="flex flex-1 min-h-0">
				{/* Categories column (left) */}
				<div className="flex flex-col w-[240px] min-w-[200px] max-w-[280px] border-r border-border/50">
					<div className="flex items-center justify-between px-3 py-2 text-xs font-medium text-muted-foreground border-b border-border/50">
						<div>Categories</div>
						<Dialog
							open={categoryDialogOpen}
							onOpenChange={setCategoryDialogOpen}
						>
							<DialogTrigger asChild>
								<Button
									variant="ghost"
									size="sm"
									className="h-6 text-[11px] px-2"
								>
									New Category
								</Button>
							</DialogTrigger>
							<DialogContent>
								<DialogHeader>
									<DialogTitle>Create Category</DialogTitle>
									<DialogDescription>
										Categories group patterns in the library.
									</DialogDescription>
								</DialogHeader>
								<div className="grid gap-2 py-4">
									<Label htmlFor={categoryNameId}>Name</Label>
									<Input
										id={categoryNameId}
										value={categoryName}
										onChange={(e) => setCategoryName(e.target.value)}
										placeholder="Category name"
										onKeyDown={(e) => {
											if (e.key === "Enter" && categoryName.trim()) {
												handleCreateCategory();
											}
										}}
									/>
								</div>
								<DialogFooter>
									<Button
										variant="outline"
										onClick={() => setCategoryDialogOpen(false)}
										disabled={creatingCategory}
									>
										Cancel
									</Button>
									<Button
										onClick={handleCreateCategory}
										disabled={creatingCategory || !categoryName.trim()}
									>
										{creatingCategory ? "Creating..." : "Create"}
									</Button>
								</DialogFooter>
							</DialogContent>
						</Dialog>
					</div>

					<div className="flex-1 overflow-y-auto py-1">
						<button
							type="button"
							onClick={() => setSelectedCategoryWithUrl("all")}
							onDrop={handleDropOnCategory("all")}
							onDragOver={allowCategoryDrop("all")}
							onDragLeave={handleCategoryDragLeave("all")}
							className={`w-full flex items-center justify-between px-3 py-1.5 text-sm text-left border ${
								dragOverCategory === "all"
									? "border-primary"
									: "border-transparent"
							} ${selectedCategory === "all" ? "bg-muted" : "hover:bg-card"}`}
						>
							<span className="truncate">All Patterns</span>
							<span className="text-[10px] text-muted-foreground">
								{patterns.length}
							</span>
						</button>

						<button
							type="button"
							onClick={() => setSelectedCategoryWithUrl("uncategorized")}
							onDrop={handleDropOnCategory("uncategorized")}
							onDragOver={allowCategoryDrop("uncategorized")}
							onDragLeave={handleCategoryDragLeave("uncategorized")}
							className={`w-full flex items-center justify-between px-3 py-1.5 text-sm text-left border ${
								dragOverCategory === "uncategorized"
									? "border-primary"
									: "border-transparent"
							} ${selectedCategory === "uncategorized" ? "bg-muted" : "hover:bg-card"}`}
						>
							<span className="truncate">Uncategorized</span>
							<span className="text-[10px] text-muted-foreground">
								{
									patternsWithCategory.filter(
										(p) => p.categoryId == null || p.categoryId === undefined,
									).length
								}
							</span>
						</button>

						<div className="my-1 border-t border-border/50" />

						{categoriesLoading ? (
							<div className="px-3 py-2 text-xs text-muted-foreground">
								Loading categories...
							</div>
						) : categories.length === 0 ? (
							<div className="px-3 py-2 text-xs text-muted-foreground space-y-2">
								<div>No categories yet</div>
								<Button
									variant="outline"
									size="sm"
									className="h-7 text-xs"
									onClick={() => setCategoryDialogOpen(true)}
								>
									Create your first category
								</Button>
							</div>
						) : (
							categories.map((cat) => {
								const count = patternsWithCategory.filter(
									(p) => p.categoryId === cat.id,
								).length;
								return (
									<button
										key={cat.id}
										type="button"
										onClick={() => setSelectedCategoryWithUrl(cat.id)}
										onDrop={handleDropOnCategory(cat.id)}
										onDragOver={allowCategoryDrop(cat.id)}
										onDragLeave={handleCategoryDragLeave(cat.id)}
										className={`w-full flex items-center justify-between px-3 py-1.5 text-sm text-left border ${
											dragOverCategory === cat.id
												? "border-primary"
												: "border-transparent"
										} ${selectedCategory === cat.id ? "bg-muted" : "hover:bg-card"}`}
									>
										<span className="truncate">{cat.name}</span>
										<span className="text-[10px] text-muted-foreground">
											{count}
										</span>
									</button>
								);
							})
						)}
					</div>
				</div>

				{/* Patterns column (right) */}
				<div className="flex flex-col flex-1 min-w-0">
					<div className="flex items-center justify-between px-4 py-2 text-xs font-medium text-muted-foreground border-b border-border/50">
						<div className="truncate">{selectedCategoryLabel}</div>
						<div className="text-[10px] opacity-70">
							{filteredPatterns.length} shown
						</div>
					</div>

					<div className="grid grid-cols-[1fr_2fr_120px] gap-4 px-4 py-2 text-xs font-medium text-muted-foreground border-b border-border/50 select-none">
						<div>NAME</div>
						<div>DESCRIPTION</div>
						<div className="text-right">MODIFIED</div>
					</div>

					<div className="flex-1 overflow-y-auto">
						{filteredPatterns.length === 0 ? (
							<div className="flex flex-col items-center justify-center h-32 text-xs text-muted-foreground">
								No patterns in this category
							</div>
						) : (
							filteredPatterns.map((pattern) => (
								<button
									key={pattern.id}
									type="button"
									onClick={() => handlePatternClick(pattern)}
									draggable
									onDragStart={handleDragStart(pattern)}
									onDragEnd={handleDragEnd}
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
			</div>
		</div>
	);
}
