import { invoke } from "@tauri-apps/api/core";
import { FolderOpen, Minus, Plus, Tag, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { FixtureGroupNode } from "@/bindings/groups";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { cn } from "@/shared/lib/utils";
import { useGroupStore } from "../stores/use-group-store";

// Predefined tags - must match backend
const PREDEFINED_TAGS = [
	// Spatial
	"left",
	"right",
	"center",
	"front",
	"back",
	"high",
	"low",
	"circular",
	// Purpose
	"blinder",
	"wash",
	"spot",
	"chase",
];

// Colors for group tags (matches visualizer)
const GROUP_COLORS = [
	"#7eb8da",
	"#a8d8a8",
	"#f4a6a6",
	"#c9a8f4",
	"#f4d8a8",
	"#a8f4f4",
	"#f4a8d8",
	"#d8f4a8",
	"#a8c8f4",
	"#f4c8a8",
];

export function GroupedFixtureTree() {
	const { groups, fetchGroups, createGroup, deleteGroup, isLoading } =
		useGroupStore();
	const [selectedGroupId, setSelectedGroupId] = useState<number | null>(null);
	const [editingGroupId, setEditingGroupId] = useState<number | null>(null);
	const [editingValue, setEditingValue] = useState("");
	const [dragOverGroupId, setDragOverGroupId] = useState<number | null>(null);
	const inputRef = useRef<HTMLInputElement | null>(null);
	const venueId = useAppViewStore((state) => state.currentVenue?.id ?? null);

	useEffect(() => {
		if (venueId !== null) {
			fetchGroups(venueId);
		}
	}, [venueId, fetchGroups]);

	useEffect(() => {
		if (editingGroupId && inputRef.current) {
			inputRef.current.focus();
			inputRef.current.select();
		}
	}, [editingGroupId]);

	// Get tags for selected group
	const selectedGroup = groups.find((g) => g.groupId === selectedGroupId);
	const groupTags = selectedGroup?.tags ?? [];

	const handleAddTag = async (tag: string) => {
		if (!selectedGroupId) return;
		try {
			await invoke("add_tag_to_group", { groupId: selectedGroupId, tag });
			if (venueId) fetchGroups(venueId);
		} catch (e) {
			console.error("Failed to add tag:", e);
		}
	};

	const handleRemoveTag = async (tag: string) => {
		if (!selectedGroupId) return;
		try {
			await invoke("remove_tag_from_group", { groupId: selectedGroupId, tag });
			if (venueId) fetchGroups(venueId);
		} catch (e) {
			console.error("Failed to remove tag:", e);
		}
	};

	const handleGroupClick = (groupId: number) => {
		setSelectedGroupId(groupId);
	};

	const handleAddGroup = async () => {
		if (venueId === null) return;
		await createGroup(venueId, undefined, 0, 0, 0);
	};

	const handleDeleteGroup = async () => {
		if (selectedGroupId === null) return;
		const group = groups.find((g) => g.groupId === selectedGroupId);
		if (!group || group.fixtures.length > 0) {
			return; // Can't delete non-empty group
		}
		const success = await deleteGroup(selectedGroupId);
		if (success) {
			setSelectedGroupId(null);
			if (venueId !== null) {
				fetchGroups(venueId);
			}
		}
	};

	const startEditingGroup = (groupId: number, currentName: string) => {
		setEditingGroupId(groupId);
		setEditingValue(currentName);
	};

	const commitEdit = async () => {
		if (editingGroupId === null) return;
		const next = editingValue.trim();
		if (!next) {
			setEditingGroupId(null);
			return;
		}

		const current = groups.find((g) => g.groupId === editingGroupId);
		if (current?.groupName === next) {
			setEditingGroupId(null);
			return;
		}

		try {
			await invoke("update_group", {
				id: editingGroupId,
				name: next,
				axisLr: current?.axisLr ?? null,
				axisFb: current?.axisFb ?? null,
				axisAb: current?.axisAb ?? null,
			});
			if (venueId !== null) {
				fetchGroups(venueId);
			}
		} catch (error) {
			console.error("Failed to update group name:", error);
		}
		setEditingGroupId(null);
	};

	const cancelEdit = () => {
		setEditingGroupId(null);
		setEditingValue("");
	};

	// Drop handlers - accept fixtures from PatchSchedule
	const handleDragOver = (e: React.DragEvent, groupId: number) => {
		e.preventDefault();
		e.dataTransfer.dropEffect = "copy";
		setDragOverGroupId(groupId);
	};

	const handleDragLeave = () => {
		setDragOverGroupId(null);
	};

	const handleDrop = async (e: React.DragEvent, targetGroupId: number) => {
		e.preventDefault();
		setDragOverGroupId(null);

		const fixtureId = e.dataTransfer.getData("fixtureId");
		if (!fixtureId) return;

		try {
			// Add fixture to group (fixtures can be in multiple groups)
			await invoke("add_fixture_to_group", {
				fixtureId,
				groupId: targetGroupId,
			});
			if (venueId !== null) {
				fetchGroups(venueId);
			}
		} catch (error) {
			console.error("Failed to add fixture to group:", error);
		}
	};

	const renderGroup = (group: FixtureGroupNode, index: number) => {
		const isSelected = selectedGroupId === group.groupId;
		const isDragOver = dragOverGroupId === group.groupId;
		const isEditing = editingGroupId === group.groupId;
		const color = GROUP_COLORS[index % GROUP_COLORS.length];

		return (
			<fieldset
				key={group.groupId}
				className="border-none p-0 m-0 min-w-0"
				onDragOver={(e) => handleDragOver(e, group.groupId)}
				onDragLeave={handleDragLeave}
				onDrop={(e) => handleDrop(e, group.groupId)}
			>
				<div
					className={cn(
						"flex items-center py-1.5 px-2 text-sm cursor-pointer transition-colors",
						isSelected
							? "bg-primary/20 text-primary"
							: isDragOver
								? "bg-primary/10 ring-1 ring-primary/50"
								: "hover:bg-muted",
					)}
				>
					{/* Color indicator */}
					<div
						className="w-2 h-2 rounded-full mr-2 flex-shrink-0"
						style={{ backgroundColor: color }}
					/>

					{isEditing ? (
						<>
							<FolderOpen
								size={14}
								className="mr-2 text-yellow-500 flex-shrink-0"
							/>
							<input
								ref={inputRef}
								value={editingValue}
								onChange={(e) => setEditingValue(e.target.value)}
								onBlur={commitEdit}
								onKeyDown={(e) => {
									if (e.key === "Enter") {
										e.preventDefault();
										void commitEdit();
									} else if (e.key === "Escape") {
										e.preventDefault();
										cancelEdit();
									}
								}}
								onClick={(e) => e.stopPropagation()}
								className="flex-1 truncate text-sm font-medium bg-transparent border-none outline-none focus:outline-none focus:ring-0"
							/>
						</>
					) : (
						<button
							type="button"
							className="flex flex-1 items-center bg-transparent p-0 text-left"
							onClick={() => handleGroupClick(group.groupId)}
							onDoubleClick={() => {
								startEditingGroup(
									group.groupId,
									group.groupName ?? "Unnamed Group",
								);
							}}
						>
							<FolderOpen
								size={14}
								className="mr-2 text-yellow-500 flex-shrink-0"
							/>
							<span className="flex-1 truncate font-medium">
								{group.groupName ?? "Unnamed Group"}
							</span>
							<span className="text-xs text-muted-foreground ml-2 flex-shrink-0">
								{group.fixtures.length}
							</span>
						</button>
					)}
				</div>

				{/* Show tags inline for selected group */}
				{isSelected && group.tags.length > 0 && (
					<div className="flex flex-wrap gap-1 px-6 pb-1">
						{group.tags.map((tag) => (
							<span
								key={tag}
								className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground"
							>
								{tag}
							</span>
						))}
					</div>
				)}
			</fieldset>
		);
	};

	const canDeleteSelectedGroup = () => {
		if (selectedGroupId === null) return false;
		const group = groups.find((g) => g.groupId === selectedGroupId);
		return group && group.fixtures.length === 0;
	};

	if (isLoading) {
		return (
			<div className="flex flex-col w-full h-full bg-background p-4 text-muted-foreground text-sm">
				Loading groups...
			</div>
		);
	}

	return (
		<div className="flex flex-col w-full h-full bg-background">
			<div className="px-3 py-2 border-b border-border text-xs font-medium tracking-[0.08em] text-muted-foreground uppercase">
				Groups
			</div>

			<div className="flex-1 overflow-y-auto min-h-0">
				{groups.length === 0 ? (
					<div className="p-4 text-sm text-muted-foreground">
						No groups yet. Drag fixtures here.
					</div>
				) : (
					groups.map((group, i) => renderGroup(group, i))
				)}
			</div>

			{/* Tags Panel - shows when group selected */}
			{selectedGroupId && (
				<div className="border-t border-border">
					<div className="px-3 py-1.5 border-b border-border text-[10px] font-medium tracking-[0.08em] text-muted-foreground uppercase flex items-center gap-2">
						<Tag size={10} />
						Tags
					</div>
					<div className="p-2 space-y-2">
						{/* Current tags */}
						<div className="flex flex-wrap gap-1">
							{groupTags.length === 0 ? (
								<span className="text-xs text-muted-foreground">No tags</span>
							) : (
								groupTags.map((tag) => (
									<span
										key={tag}
										className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs bg-amber-500/20 text-amber-400"
									>
										{tag}
										<button
											type="button"
											onClick={() => handleRemoveTag(tag)}
											className="hover:text-red-400"
										>
											<X size={10} />
										</button>
									</span>
								))
							)}
						</div>

						{/* Add tag from predefined list */}
						{PREDEFINED_TAGS.filter((t) => !groupTags.includes(t)).length >
							0 && (
							<div className="flex flex-wrap gap-1">
								{PREDEFINED_TAGS.filter((t) => !groupTags.includes(t)).map(
									(tag) => (
										<button
											key={tag}
											type="button"
											onClick={() => handleAddTag(tag)}
											className="px-1.5 py-0.5 rounded text-[10px] bg-muted hover:bg-accent text-muted-foreground hover:text-foreground"
										>
											{tag}
										</button>
									),
								)}
							</div>
						)}
					</div>
				</div>
			)}

			<div className="p-2 border-t border-border flex gap-2">
				<button
					type="button"
					className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
					onClick={handleAddGroup}
				>
					<Plus size={12} />
					Add
				</button>
				<button
					type="button"
					className={cn(
						"flex items-center gap-1 text-xs",
						canDeleteSelectedGroup()
							? "text-muted-foreground hover:text-red-500"
							: "text-muted-foreground/30 cursor-not-allowed",
					)}
					onClick={handleDeleteGroup}
					disabled={!canDeleteSelectedGroup()}
				>
					<Minus size={12} />
					Remove
				</button>
			</div>
		</div>
	);
}
