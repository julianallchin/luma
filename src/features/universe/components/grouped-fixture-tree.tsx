import { invoke } from "@tauri-apps/api/core";
import { ChevronDown, ChevronRight, Minus, Move, Plus, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { FixtureGroupNode, MovementConfig } from "@/bindings/groups";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../stores/use-fixture-store";
import { useGroupStore } from "../stores/use-group-store";

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
	const groups = useGroupStore((s) => s.groups);
	const fetchGroups = useGroupStore((s) => s.fetchGroups);
	const createGroup = useGroupStore((s) => s.createGroup);
	const deleteGroup = useGroupStore((s) => s.deleteGroup);
	const updateGroup = useGroupStore((s) => s.updateGroup);
	const removeFixtureFromGroup = useGroupStore((s) => s.removeFixtureFromGroup);
	const addFixtureToGroup = useGroupStore((s) => s.addFixtureToGroup);
	const updateMovementConfig = useGroupStore((s) => s.updateMovementConfig);
	const selectedGroupId = useGroupStore((s) => s.selectedGroupId);
	const setSelectedGroupId = useGroupStore((s) => s.setSelectedGroupId);
	const isLoading = useGroupStore((s) => s.isLoading);
	const [editingGroupId, setEditingGroupId] = useState<string | null>(null);
	const [editingValue, setEditingValue] = useState("");
	const [dragOverGroupId, setDragOverGroupId] = useState<string | null>(null);
	// "groupId/fixtureId" keys of fixtures whose head list is expanded
	const [expandedFixtures, setExpandedFixtures] = useState<Set<string>>(
		new Set(),
	);
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

	const selectedGroup = groups.find((g) => g.groupId === selectedGroupId);

	// Movement config — only a group with something to aim has a pyramid.
	const isMovingGroup = selectedGroup?.moves ?? false;
	const storeConfig = selectedGroup?.movementConfig ?? null;

	// Local state for immediate slider feedback; synced from store when selection changes
	const [localConfig, setLocalConfig] = useState<MovementConfig | null>(null);
	const prevGroupIdRef = useRef<string | null>(null);
	if (prevGroupIdRef.current !== selectedGroupId) {
		prevGroupIdRef.current = selectedGroupId;
		setLocalConfig(storeConfig);
	}
	const movementConfig = localConfig ?? storeConfig;

	const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const handleMovementConfigChange = useCallback(
		(patch: Partial<MovementConfig>) => {
			if (!selectedGroupId) return;
			const current: MovementConfig = movementConfig ?? {
				baseDirX: 0,
				baseDirY: 0,
				baseDirZ: -1,
				extentU: 30,
				extentV: 30,
				uvRotation: 0,
			};
			const updated = { ...current, ...patch };
			// Update local state immediately for responsive sliders
			setLocalConfig(updated);
			// Debounce the backend persist
			if (debounceRef.current) clearTimeout(debounceRef.current);
			debounceRef.current = setTimeout(() => {
				void updateMovementConfig(selectedGroupId, updated);
			}, 300);
		},
		[selectedGroupId, movementConfig, updateMovementConfig],
	);

	const selectFixturesByIds = useFixtureStore((s) => s.selectFixturesByIds);
	const clearFixtureSelection = useFixtureStore((s) => s.clearSelection);

	const handleGroupClick = (groupId: string) => {
		setSelectedGroupId(groupId);
		const group = groups.find((g) => g.groupId === groupId);
		const ids = group?.fixtures.map((f) => f.id) ?? [];
		// Blink only what the group controls: whole fixtures blink whole, but
		// partially-membered fixtures blink just their member heads.
		const blinkTargets =
			group?.fixtures.flatMap((f) => {
				const headCount = Number(f.headCount);
				const partial = headCount > 0 && f.heads.length < headCount;
				return partial
					? f.heads.map((h) => `${f.id}:${Number(h.headIndex)}`)
					: [f.id];
			}) ?? [];
		selectFixturesByIds(ids, undefined, blinkTargets);
	};

	const handleEmptyAreaClick = (e: React.MouseEvent<HTMLDivElement>) => {
		if (e.target !== e.currentTarget) return;
		setSelectedGroupId(null);
		clearFixtureSelection();
	};

	const fetchUngroupedFixtures = useFixtureStore(
		(state) => state.fetchUngroupedFixtures,
	);

	const handleRemoveFixture = async (
		fixtureId: string,
		groupId: string,
		headIndex?: number,
	) => {
		await removeFixtureFromGroup(fixtureId, groupId, headIndex);
		fetchUngroupedFixtures();
	};

	const toggleFixtureExpanded = (key: string) => {
		setExpandedFixtures((prev) => {
			const next = new Set(prev);
			if (next.has(key)) {
				next.delete(key);
			} else {
				next.add(key);
			}
			return next;
		});
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

	const startEditingGroup = (groupId: string, currentName: string) => {
		setEditingGroupId(groupId);
		setEditingValue(currentName);
	};

	const commitEdit = async () => {
		if (editingGroupId === null) return;
		const raw = editingValue.trim();
		if (!raw) {
			setEditingGroupId(null);
			return;
		}
		const next = raw
			.toLowerCase()
			.replace(/[\s-]+/g, "_")
			.replace(/[^a-z0-9_]/g, "")
			.replace(/_+/g, "_")
			.replace(/^_|_$/g, "");
		if (!next) {
			setEditingGroupId(null);
			return;
		}
		setEditingValue(next); // show normalized name

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
	const handleDragOver = (e: React.DragEvent, groupId: string) => {
		e.preventDefault();
		e.dataTransfer.dropEffect = "copy";
		setDragOverGroupId(groupId);
	};

	const handleDragLeave = () => {
		setDragOverGroupId(null);
	};

	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);

	const handleDrop = async (e: React.DragEvent, targetGroupId: string) => {
		e.preventDefault();
		setDragOverGroupId(null);

		// Single head dragged from another group's head list
		const headRefJson = e.dataTransfer.getData("headRef");
		if (headRefJson) {
			try {
				const ref: { fixtureId: string; headIndex: number; label: string } =
					JSON.parse(headRefJson);
				await addFixtureToGroup(
					ref.fixtureId,
					targetGroupId,
					{ id: ref.fixtureId, label: ref.label },
					ref.headIndex,
				);
				return;
			} catch (_) {
				// Fall through
			}
		}

		// Try multi-fixture drop first
		const idsJson = e.dataTransfer.getData("fixtureIds");
		if (idsJson) {
			try {
				const ids: string[] = JSON.parse(idsJson);
				for (const id of ids) {
					const fixture = patchedFixtures.find((f) => f.id === id);
					await addFixtureToGroup(id, targetGroupId, {
						id,
						label: fixture?.label ?? fixture?.model ?? id,
					});
				}
				fetchUngroupedFixtures();
				return;
			} catch (_) {
				// Fall through to single fixture
			}
		}

		// Fallback: single fixture
		const fixtureId = e.dataTransfer.getData("fixtureId");
		const fixtureLabel = e.dataTransfer.getData("fixtureLabel");
		if (!fixtureId) return;

		await addFixtureToGroup(fixtureId, targetGroupId, {
			id: fixtureId,
			label: fixtureLabel || fixtureId,
		});
		fetchUngroupedFixtures();
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
							: "border-trim",
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
							autoCapitalize="off"
							autoCorrect="off"
							spellCheck={false}
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

				{/* Fixtures list */}
				{hasFixtures && (
					<div className="border-t border-trim">
						{group.fixtures.map((fixture) => {
							const headCount = Number(fixture.headCount);
							const memberHeads = new Set(
								fixture.heads.map((h) => Number(h.headIndex)),
							);
							const isPartial = headCount > 0 && memberHeads.size < headCount;
							const expandKey = `${group.groupId}/${fixture.id}`;
							const isExpanded = expandedFixtures.has(expandKey);
							const canExpand = headCount > 1;

							return (
								<div key={fixture.id}>
									<div className="flex items-center py-1.5 px-3 text-sm text-muted-foreground hover:bg-muted/50 group">
										{canExpand && (
											<button
												type="button"
												onClick={() => toggleFixtureExpanded(expandKey)}
												className="mr-1 -ml-1 p-0.5 hover:text-foreground"
												title={isExpanded ? "Collapse heads" : "Expand heads"}
											>
												{isExpanded ? (
													<ChevronDown size={12} />
												) : (
													<ChevronRight size={12} />
												)}
											</button>
										)}
										<span className="flex-1 truncate">{fixture.label}</span>
										{isPartial && (
											<span className="text-[10px] text-muted-foreground/70 mr-1 flex-shrink-0">
												{memberHeads.size}/{headCount}
											</span>
										)}
										<button
											type="button"
											onClick={() =>
												handleRemoveFixture(fixture.id, group.groupId)
											}
											className="opacity-0 group-hover:opacity-100 p-0.5 hover:text-red-500 transition-opacity"
											title="Remove from group"
										>
											<X size={12} />
										</button>
									</div>

									{/* Head list: members are draggable to other groups;
									    non-members are dimmed with an add button. */}
									{isExpanded &&
										Array.from({ length: headCount }, (_, headIndex) => {
											const isMember = memberHeads.has(headIndex);
											const headLabel = `Head ${headIndex + 1}`;
											return (
												// biome-ignore lint/a11y/noStaticElementInteractions: drag handle is a mouse-only affordance; add/remove buttons are the keyboard path
												<div
													// biome-ignore lint/suspicious/noArrayIndexKey: head index is the head's identity
													key={headIndex}
													draggable={isMember}
													onDragStart={(e) => {
														e.dataTransfer.setData(
															"headRef",
															JSON.stringify({
																fixtureId: fixture.id,
																headIndex,
																label: `${fixture.label} · ${headLabel}`,
															}),
														);
														e.dataTransfer.effectAllowed = "copy";
													}}
													className={cn(
														"flex items-center py-1 pl-9 pr-3 text-xs group/head",
														isMember
															? "text-muted-foreground hover:bg-muted/50 cursor-grab"
															: "text-muted-foreground/40",
													)}
												>
													<button
														type="button"
														onClick={() =>
															invoke("render_identify", {
																targets: [`${fixture.id}:${headIndex}`],
															}).catch(() => {})
														}
														className="flex-1 truncate text-left"
														title="Blink this head"
													>
														{headLabel}
													</button>
													{isMember ? (
														<button
															type="button"
															onClick={() =>
																handleRemoveFixture(
																	fixture.id,
																	group.groupId,
																	headIndex,
																)
															}
															className="opacity-0 group-hover/head:opacity-100 p-0.5 hover:text-red-500 transition-opacity"
															title="Remove head from group"
														>
															<X size={10} />
														</button>
													) : (
														<button
															type="button"
															onClick={() =>
																addFixtureToGroup(
																	fixture.id,
																	group.groupId,
																	{ id: fixture.id, label: fixture.label },
																	headIndex,
																)
															}
															className="opacity-0 group-hover/head:opacity-100 p-0.5 hover:text-foreground transition-opacity"
															title="Add head to group"
														>
															<Plus size={10} />
														</button>
													)}
												</div>
											);
										})}
								</div>
							);
						})}
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
			<div className="flex flex-col w-full h-full bg-gutter p-4 text-muted-foreground text-sm">
				Loading groups...
			</div>
		);
	}

	return (
		<div className="flex flex-col w-full h-full bg-gutter">
			<div className="px-3 py-2 bg-trim text-xs font-medium tracking-[0.08em] text-muted-foreground uppercase">
				Groups
			</div>

			{/* biome-ignore lint/a11y/noStaticElementInteractions: empty-area click is a mouse-only deselect target */}
			{/* biome-ignore lint/a11y/useKeyWithClickEvents: empty-area click is a mouse-only deselect target */}
			<div
				className="flex-1 overflow-y-auto min-h-0 bg-gutter"
				onClick={handleEmptyAreaClick}
			>
				{groups.length === 0 ? (
					<div className="p-4 text-sm text-muted-foreground">
						No groups yet. Drag fixtures here.
					</div>
				) : (
					groups.map((group, i) => renderGroup(group, i))
				)}
			</div>

			{/* Movement Config - shows for mover groups */}
			{selectedGroupId && isMovingGroup && (
				<div className="border-t border-trim">
					<div className="px-3 py-1.5 border-b border-trim text-[10px] font-medium tracking-[0.08em] text-muted-foreground uppercase flex items-center gap-2">
						<Move size={10} />
						Movement
					</div>
					<div className="p-2 space-y-2">
						{/* Base Direction presets */}
						<div>
							<span className="text-[10px] text-muted-foreground">
								Base Direction
							</span>
							<div className="flex gap-1 mt-1">
								{[
									{
										label: "Down",
										dir: { baseDirX: 0, baseDirY: 0, baseDirZ: -1 },
									},
									{
										label: "Forward",
										dir: { baseDirX: 0, baseDirY: 1, baseDirZ: 0 },
									},
									{
										label: "Up",
										dir: { baseDirX: 0, baseDirY: 0, baseDirZ: 1 },
									},
								].map(({ label, dir }) => (
									<button
										key={label}
										type="button"
										onClick={() => handleMovementConfigChange(dir)}
										className="px-1.5 py-0.5 rounded text-[10px] bg-muted hover:bg-accent text-muted-foreground hover:text-foreground"
									>
										{label}
									</button>
								))}
							</div>
						</div>

						{/* Extent U */}
						<label className="block">
							<span className="text-[10px] text-muted-foreground flex justify-between">
								<span>Extent U</span>
								<span>{movementConfig?.extentU ?? 30}&deg;</span>
							</span>
							<input
								type="range"
								min={1}
								max={90}
								step={1}
								value={movementConfig?.extentU ?? 30}
								onChange={(e) =>
									handleMovementConfigChange({
										extentU: Number(e.target.value),
									})
								}
								className="w-full h-1 accent-primary"
							/>
						</label>

						{/* Extent V */}
						<label className="block">
							<span className="text-[10px] text-muted-foreground flex justify-between">
								<span>Extent V</span>
								<span>{movementConfig?.extentV ?? 30}&deg;</span>
							</span>
							<input
								type="range"
								min={1}
								max={90}
								step={1}
								value={movementConfig?.extentV ?? 30}
								onChange={(e) =>
									handleMovementConfigChange({
										extentV: Number(e.target.value),
									})
								}
								className="w-full h-1 accent-primary"
							/>
						</label>

						{/* UV Rotation */}
						<label className="block">
							<span className="text-[10px] text-muted-foreground flex justify-between">
								<span>UV Rotation</span>
								<span>{movementConfig?.uvRotation ?? 0}&deg;</span>
							</span>
							<input
								type="range"
								min={0}
								max={360}
								step={1}
								value={movementConfig?.uvRotation ?? 0}
								onChange={(e) =>
									handleMovementConfigChange({
										uvRotation: Number(e.target.value),
									})
								}
								className="w-full h-1 accent-primary"
							/>
						</label>
					</div>
				</div>
			)}

			<div className="p-2 border-t border-trim flex gap-2">
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
