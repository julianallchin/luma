import { Minus, Plus, Tag, X } from "lucide-react";
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
	"high",
	"low",
	"circular",
	// Purpose
	"hit",
	"wash",
	"accent",
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
	const {
		groups,
		fetchGroups,
		createGroup,
		deleteGroup,
		updateGroup,
		removeFixtureFromGroup,
		addFixtureToGroup,
		addTagToGroup,
		removeTagFromGroup,
		isLoading,
	} = useGroupStore();
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
		await addTagToGroup(selectedGroupId, tag);
	};

	const handleRemoveTag = async (tag: string) => {
		if (!selectedGroupId) return;
		await removeTagFromGroup(selectedGroupId, tag);
	};

	const handleGroupClick = (groupId: number) => {
		setSelectedGroupId(groupId);
	};

	const handleRemoveFixture = async (fixtureId: string, groupId: number) => {
		await removeFixtureFromGroup(fixtureId, groupId);
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

		await updateGroup(
			editingGroupId,
			next,
			current?.axisLr ?? null,
			current?.axisFb ?? null,
			current?.axisAb ?? null,
		);
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
		const fixtureLabel = e.dataTransfer.getData("fixtureLabel");
		if (!fixtureId) return;

		await addFixtureToGroup(fixtureId, targetGroupId, {
			id: fixtureId,
			label: fixtureLabel || fixtureId,
		});
	};

	const renderGroup = (group: FixtureGroupNode, index: number) => {
		const isSelected = selectedGroupId === group.groupId;
		const isDragOver = dragOverGroupId === group.groupId;
		const isEditing = editingGroupId === group.groupId;
		const color = GROUP_COLORS[index % GROUP_COLORS.length];
		const hasFixtures = group.fixtures.length > 0;

		return (
			<section
				key={group.groupId}
				aria-label={group.groupName ?? "Unnamed Group"}
				className={cn(
					"m-2 rounded-lg border bg-card transition-colors",
					isSelected
						? "border-primary ring-1 ring-primary/50"
						: isDragOver
							? "border-primary/50 bg-primary/5"
							: "border-border",
				)}
				onDragOver={(e) => handleDragOver(e, group.groupId)}
				onDragLeave={handleDragLeave}
				onDrop={(e) => handleDrop(e, group.groupId)}
			>
				{/* Header */}
				<button
					type="button"
					className="flex items-center py-2 px-3 cursor-pointer w-full text-left"
					onClick={() => handleGroupClick(group.groupId)}
					onDoubleClick={() => {
						startEditingGroup(
							group.groupId,
							group.groupName ?? "Unnamed Group",
						);
					}}
				>
					{/* Color indicator */}
					<div
						className="w-3 h-3 rounded-full mr-2 flex-shrink-0"
						style={{ backgroundColor: color }}
					/>

					{isEditing ? (
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
					) : (
						<>
							<span className="flex-1 truncate text-sm font-medium">
								{group.groupName ?? "Unnamed Group"}
							</span>
							<span className="text-xs text-muted-foreground ml-2 flex-shrink-0">
								{group.fixtures.length}
							</span>
						</>
					)}
				</button>

				{/* Tags - always visible */}
				{group.tags.length > 0 && (
					<div className="flex flex-wrap gap-1 px-3 pb-2">
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

				{/* Fixtures list */}
				{hasFixtures && (
					<div className="border-t border-border">
						{group.fixtures.map((fixture) => (
							<div
								key={fixture.id}
								className="flex items-center py-1.5 px-3 text-sm text-muted-foreground hover:bg-muted/50 group"
							>
								<span className="flex-1 truncate">{fixture.label}</span>
								<button
									type="button"
									onClick={() => handleRemoveFixture(fixture.id, group.groupId)}
									className="opacity-0 group-hover:opacity-100 p-0.5 hover:text-red-500 transition-opacity"
									title="Remove from group"
								>
									<X size={12} />
								</button>
							</div>
						))}
					</div>
				)}
			</section>
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
