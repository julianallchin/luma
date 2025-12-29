import { invoke } from "@tauri-apps/api/core";
import {
	Box,
	ChevronDown,
	ChevronRight,
	FolderOpen,
	Minus,
	Plus,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type {
	FixtureGroupNode,
	FixtureType,
	GroupedFixtureNode,
} from "@/bindings/groups";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { cn } from "@/shared/lib/utils";
import { useGroupStore } from "../stores/use-group-store";

const FIXTURE_TYPE_LABELS: Record<FixtureType, string> = {
	moving_head: "Moving Head",
	pixel_bar: "Pixel Bar",
	par_wash: "Par Wash",
	scanner: "Scanner",
	strobe: "Strobe",
	static: "Static",
	unknown: "Unknown",
};

export function GroupedFixtureTree() {
	const { groups, fetchGroups, createGroup, deleteGroup, isLoading } =
		useGroupStore();
	const [expanded, setExpanded] = useState<Set<number>>(new Set());
	const [selectedGroupId, setSelectedGroupId] = useState<number | null>(null);
	const [selectedFixtureId, setSelectedFixtureId] = useState<string | null>(
		null,
	);
	const [editingGroupId, setEditingGroupId] = useState<number | null>(null);
	const [editingValue, setEditingValue] = useState("");
	const [draggedFixtureId, setDraggedFixtureId] = useState<string | null>(null);
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

	const toggleExpand = (groupId: number) => {
		const next = new Set(expanded);
		if (next.has(groupId)) {
			next.delete(groupId);
		} else {
			next.add(groupId);
		}
		setExpanded(next);
	};

	const handleGroupClick = (groupId: number) => {
		setSelectedGroupId(groupId);
		setSelectedFixtureId(null);
	};

	const handleFixtureClick = (fixtureId: string, groupId: number) => {
		setSelectedFixtureId(fixtureId);
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
				axisLr: null,
				axisFb: null,
				axisAb: null,
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

	// Drag and drop handlers
	const handleDragStart = (
		e: React.DragEvent,
		fixtureId: string,
		sourceGroupId: number,
	) => {
		setDraggedFixtureId(fixtureId);
		e.dataTransfer.setData("fixtureId", fixtureId);
		e.dataTransfer.setData("sourceGroupId", sourceGroupId.toString());
		e.dataTransfer.effectAllowed = "move";
	};

	const handleDragOver = (e: React.DragEvent, groupId: number) => {
		e.preventDefault();
		e.dataTransfer.dropEffect = "move";
		setDragOverGroupId(groupId);
	};

	const handleDragLeave = () => {
		setDragOverGroupId(null);
	};

	const handleDrop = async (e: React.DragEvent, targetGroupId: number) => {
		e.preventDefault();
		setDragOverGroupId(null);

		const fixtureId = e.dataTransfer.getData("fixtureId");
		const sourceGroupId = Number.parseInt(
			e.dataTransfer.getData("sourceGroupId"),
			10,
		);

		if (!fixtureId || sourceGroupId === targetGroupId) {
			setDraggedFixtureId(null);
			return;
		}

		try {
			// Remove from source group
			await invoke("remove_fixture_from_group", {
				fixtureId,
				groupId: sourceGroupId,
			});
			// Add to target group
			await invoke("add_fixture_to_group", {
				fixtureId,
				groupId: targetGroupId,
			});
			// Refresh
			if (venueId !== null) {
				fetchGroups(venueId);
			}
		} catch (error) {
			console.error("Failed to move fixture:", error);
		}

		setDraggedFixtureId(null);
	};

	const handleDragEnd = () => {
		setDraggedFixtureId(null);
		setDragOverGroupId(null);
	};

	const renderFixture = (fixture: GroupedFixtureNode, groupId: number) => {
		const isSelected = selectedFixtureId === fixture.id;
		const isDragging = draggedFixtureId === fixture.id;

		return (
			<button
				key={fixture.id}
				type="button"
				draggable
				onDragStart={(e) => handleDragStart(e, fixture.id, groupId)}
				onDragEnd={handleDragEnd}
				className={cn(
					"flex w-full items-center py-1 px-2 pl-8 text-left text-sm",
					isSelected ? "bg-primary/20 text-primary" : "hover:bg-muted",
					isDragging && "opacity-50",
				)}
				onClick={() => handleFixtureClick(fixture.id, groupId)}
			>
				<Box size={14} className="mr-2 text-muted-foreground flex-shrink-0" />
				<span className="truncate flex-1">{fixture.label}</span>
				<span className="text-xs text-muted-foreground ml-2 flex-shrink-0">
					{FIXTURE_TYPE_LABELS[fixture.fixtureType]}
				</span>
			</button>
		);
	};

	const renderGroup = (group: FixtureGroupNode) => {
		const isExpanded = expanded.has(group.groupId);
		const hasFixtures = group.fixtures.length > 0;
		const isSelected = selectedGroupId === group.groupId;
		const isDragOver = dragOverGroupId === group.groupId;
		const isEditing = editingGroupId === group.groupId;

		return (
			// biome-ignore lint/a11y/useSemanticElements: Drag-and-drop grouping needs role semantics.
			<div
				key={group.groupId}
				role="group"
				aria-label={group.groupName ?? "Unnamed Group"}
				onDragOver={(e) => handleDragOver(e, group.groupId)}
				onDragLeave={handleDragLeave}
				onDrop={(e) => handleDrop(e, group.groupId)}
			>
				<div
					className={cn(
						"flex items-center py-1 px-2 text-sm cursor-pointer",
						isSelected
							? "bg-primary/20 text-primary"
							: isDragOver
								? "bg-primary/10"
								: "hover:bg-muted",
					)}
				>
					<button
						type="button"
						className="p-0.5 hover:text-white mr-1"
						onClick={(e) => {
							e.stopPropagation();
							toggleExpand(group.groupId);
						}}
					>
						{hasFixtures ? (
							isExpanded ? (
								<ChevronDown size={14} />
							) : (
								<ChevronRight size={14} />
							)
						) : (
							<div className="w-[14px]" />
						)}
					</button>

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
							<span className="text-xs text-muted-foreground ml-2 flex-shrink-0">
								{group.fixtures.length}
							</span>
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

				{isExpanded &&
					group.fixtures.map((fixture) =>
						renderFixture(fixture, group.groupId),
					)}
			</div>
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
				Fixture Groups
			</div>

			<div className="flex-1 overflow-y-auto">
				{groups.length === 0 ? (
					<div className="p-4 text-sm text-muted-foreground">
						No groups yet. Add fixtures to create groups.
					</div>
				) : (
					groups.map((group) => renderGroup(group))
				)}
			</div>

			<div className="p-2 border-t border-border flex gap-2">
				<button
					type="button"
					className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
					onClick={handleAddGroup}
				>
					<Plus size={14} />
					Add
				</button>
				<button
					type="button"
					className={cn(
						"flex items-center gap-1 text-sm",
						canDeleteSelectedGroup()
							? "text-muted-foreground hover:text-red-500"
							: "text-muted-foreground/30 cursor-not-allowed",
					)}
					onClick={handleDeleteGroup}
					disabled={!canDeleteSelectedGroup()}
				>
					<Minus size={14} />
					Remove
				</button>
			</div>
		</div>
	);
}
