import { ChevronDown, ChevronUp } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { PatchedFixture } from "@/bindings/fixtures";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../stores/use-fixture-store";
import { useGroupStore } from "../stores/use-group-store";

type SortKey =
	| "id"
	| "fixture"
	| "vendor"
	| "mode"
	| "universe"
	| "addr"
	| "range"
	| "ch"
	| "group";

type SortDir = "asc" | "desc";

interface Row {
	id: number;
	fixture: PatchedFixture;
	label: string;
	vendor: string;
	mode: string;
	universe: number;
	addr: number;
	rangeStart: number;
	rangeEnd: number;
	ch: number;
	group: string;
}

const COLS: Array<{
	key: SortKey;
	label: string;
	align?: "left" | "right" | "center";
	width: string;
}> = [
	{ key: "id", label: "ID", align: "right", width: "32px" },
	{ key: "fixture", label: "Fixture", align: "left", width: "minmax(0,2fr)" },
	{ key: "vendor", label: "Vendor", align: "left", width: "minmax(0,1.4fr)" },
	{ key: "mode", label: "Mode", align: "left", width: "minmax(0,1fr)" },
	{ key: "universe", label: "Universe", align: "right", width: "56px" },
	{ key: "addr", label: "Addr", align: "right", width: "48px" },
	{ key: "range", label: "Range", align: "right", width: "72px" },
	{ key: "ch", label: "Ch", align: "right", width: "36px" },
	{ key: "group", label: "Group", align: "left", width: "minmax(0,1.4fr)" },
];

const GRID_COLS = COLS.map((c) => c.width).join(" ");

export function PatchTable() {
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);
	const selectedPatchedIds = useFixtureStore((s) => s.selectedPatchedIds);
	const lastSelectedPatchedId = useFixtureStore((s) => s.lastSelectedPatchedId);
	const selectFixtureById = useFixtureStore((s) => s.selectFixtureById);
	const selectFixturesByIds = useFixtureStore((s) => s.selectFixturesByIds);
	const clearSelection = useFixtureStore((s) => s.clearSelection);
	const duplicateSelectedFixtures = useFixtureStore(
		(s) => s.duplicateSelectedFixtures,
	);
	const removePatchedFixture = useFixtureStore((s) => s.removePatchedFixture);
	const duplicatePatchedFixture = useFixtureStore(
		(s) => s.duplicatePatchedFixture,
	);
	const updatePatchedFixtureLabel = useFixtureStore(
		(s) => s.updatePatchedFixtureLabel,
	);
	const groups = useGroupStore((s) => s.groups);
	const isReadOnly = useAppViewStore((s) => s.currentVenue?.role) === "member";

	const [sortKey, setSortKey] = useState<SortKey>("addr");
	const [sortDir, setSortDir] = useState<SortDir>("asc");

	const [editingId, setEditingId] = useState<string | null>(null);
	const [editingValue, setEditingValue] = useState("");
	const inputRef = useRef<HTMLInputElement | null>(null);

	const [contextMenu, setContextMenu] = useState<{
		x: number;
		y: number;
		fixtureId: string;
	} | null>(null);

	// Index fixture → the deepest group holding it. The tree arrives
	// parents-first, so the last node a fixture appears in is its leaf — and
	// that is the set a human means: `spots_left_wing_top` says more than
	// `spots`, and every ancestor is implied by it.
	const fixtureGroups = useMemo(() => {
		const map = new Map<string, string>();
		for (const g of groups) {
			if (!g.name) continue;
			for (const f of g.fixtures) {
				map.set(f.id, g.name);
			}
		}
		return map;
	}, [groups]);

	const rows: Row[] = useMemo(() => {
		const base = patchedFixtures.map((fixture, i) => {
			const addr = Number(fixture.address);
			const ch = Number(fixture.numChannels);
			return {
				id: i + 1,
				fixture,
				label: fixture.label ?? fixture.model,
				vendor: fixture.manufacturer,
				mode: fixture.modeName ?? "",
				universe: Number(fixture.universe),
				addr,
				rangeStart: addr,
				rangeEnd: addr + ch - 1,
				ch,
				group: fixtureGroups.get(fixture.id) ?? "",
			};
		});

		const dir = sortDir === "asc" ? 1 : -1;
		base.sort((a, b) => {
			const cmp = (() => {
				switch (sortKey) {
					case "id":
						return a.id - b.id;
					case "fixture":
						return a.label.localeCompare(b.label);
					case "vendor":
						return a.vendor.localeCompare(b.vendor);
					case "mode":
						return a.mode.localeCompare(b.mode);
					case "universe":
						return a.universe - b.universe;
					case "addr":
						return a.addr - b.addr;
					case "range":
						return a.rangeStart - b.rangeStart;
					case "ch":
						return a.ch - b.ch;
					case "group":
						return a.group.localeCompare(b.group);
				}
			})();
			return cmp * dir;
		});
		return base;
	}, [patchedFixtures, sortKey, sortDir, fixtureGroups]);

	const handleSort = (key: SortKey) => {
		if (sortKey === key) {
			setSortDir(sortDir === "asc" ? "desc" : "asc");
		} else {
			setSortKey(key);
			setSortDir("asc");
		}
	};

	// Keyboard: ctrl+d duplicates fixtures. Delete / Backspace is handled
	// by the visualizer-level keymap so it can operate on stage pieces
	// too (and avoid double-firing across both handlers).
	useEffect(() => {
		const handleKey = (e: KeyboardEvent) => {
			if (isReadOnly) return;
			const target = e.target as HTMLElement | null;
			const isEditing =
				target &&
				(["INPUT", "TEXTAREA"].includes(target.tagName) ||
					target.isContentEditable);
			if (isEditing) return;

			if (
				(e.ctrlKey || e.metaKey) &&
				e.key === "d" &&
				selectedPatchedIds.size > 0
			) {
				e.preventDefault();
				duplicateSelectedFixtures();
			}
		};
		window.addEventListener("keydown", handleKey);
		return () => window.removeEventListener("keydown", handleKey);
	}, [duplicateSelectedFixtures, selectedPatchedIds, isReadOnly]);

	// Focus the input when editing starts
	useEffect(() => {
		if (editingId && inputRef.current) {
			inputRef.current.focus();
			inputRef.current.select();
		}
	}, [editingId]);

	// Close context menu on click outside / esc
	useEffect(() => {
		if (!contextMenu) return;
		const handleClick = () => setContextMenu(null);
		const handleKey = (e: KeyboardEvent) => {
			if (e.key === "Escape") setContextMenu(null);
		};
		window.addEventListener("click", handleClick);
		window.addEventListener("keydown", handleKey);
		return () => {
			window.removeEventListener("click", handleClick);
			window.removeEventListener("keydown", handleKey);
		};
	}, [contextMenu]);

	const startEditing = (fixtureId: string, label: string) => {
		if (isReadOnly) return;
		setEditingId(fixtureId);
		setEditingValue(label);
		selectFixtureById(fixtureId);
	};

	const commitEdit = async () => {
		if (!editingId) return;
		const next = editingValue.trim();
		if (!next) {
			setEditingId(null);
			return;
		}
		const current = patchedFixtures.find((f) => f.id === editingId);
		const currentLabel = current?.label ?? current?.model ?? "";
		if (currentLabel === next) {
			setEditingId(null);
			return;
		}
		await updatePatchedFixtureLabel(editingId, next);
		setEditingId(null);
	};

	const cancelEdit = () => {
		setEditingId(null);
		setEditingValue("");
	};

	const handleContextMenuAction = (action: "duplicate" | "unpatch") => {
		if (!contextMenu) return;
		const id = contextMenu.fixtureId;
		setContextMenu(null);
		if (action === "duplicate") {
			duplicatePatchedFixture(id);
		} else if (action === "unpatch") {
			removePatchedFixture(id);
		}
	};

	return (
		<div className="flex flex-col h-full bg-card">
			{/* Header */}
			<div
				className="grid items-center gap-2 px-3 h-6 border-b border-trim bg-card sticky top-0 z-10"
				style={{ gridTemplateColumns: GRID_COLS }}
			>
				{COLS.map((c) => {
					const active = sortKey === c.key;
					return (
						<button
							key={c.key}
							type="button"
							onClick={() => handleSort(c.key)}
							className={cn(
								"flex items-center gap-1 text-[9px] uppercase tracking-wider font-bold text-foreground/70 hover:text-foreground bg-transparent border-0 p-0 outline-none",
								c.align === "right"
									? "justify-end"
									: c.align === "center"
										? "justify-center"
										: "justify-start",
							)}
						>
							<span>{c.label}</span>
							{active &&
								(sortDir === "asc" ? (
									<ChevronUp className="size-3" />
								) : (
									<ChevronDown className="size-3" />
								))}
						</button>
					);
				})}
			</div>

			{/* Body */}
			{/* biome-ignore lint/a11y/noStaticElementInteractions: empty-area click is a mouse-only deselect target */}
			{/* biome-ignore lint/a11y/useKeyWithClickEvents: empty-area click is a mouse-only deselect target */}
			<div
				className="flex-1 overflow-auto"
				onClick={(e) => {
					if (e.target === e.currentTarget) clearSelection();
				}}
			>
				{rows.length === 0 ? (
					<div className="text-[10px] text-foreground/40 px-3 py-6 uppercase tracking-wider">
						No patched fixtures
					</div>
				) : (
					rows.map((row, idx) => {
						const isInSelection = selectedPatchedIds.has(row.fixture.id);
						const isEditing = editingId === row.fixture.id;
						const stripe = idx % 2 === 1;
						return (
							// biome-ignore lint/a11y/useSemanticElements: drag-drop requires div
							<div
								key={row.fixture.id}
								role="row"
								tabIndex={0}
								draggable={!isReadOnly}
								onDragStart={(e) => {
									const ids = isInSelection
										? [...selectedPatchedIds]
										: [row.fixture.id];

									const ghost = document.createElement("div");
									ghost.style.position = "absolute";
									ghost.style.top = "-1000px";
									ghost.style.left = "-1000px";
									ghost.style.paddingTop = "18px";
									ghost.style.paddingLeft = "18px";

									if (ids.length > 1) {
										const SIZE = 32;
										const OFFSET = 3;
										const LAYERS = Math.min(3, ids.length);
										const stack = document.createElement("div");
										stack.style.position = "relative";
										stack.style.width = `${SIZE + OFFSET * (LAYERS - 1)}px`;
										stack.style.height = `${SIZE + OFFSET * (LAYERS - 1)}px`;
										for (let i = LAYERS - 1; i >= 0; i--) {
											const layer = document.createElement("div");
											layer.className =
												"bg-control border border-control-border pointer-events-none";
											layer.style.position = "absolute";
											layer.style.top = `${i * OFFSET}px`;
											layer.style.left = `${i * OFFSET}px`;
											layer.style.width = `${SIZE}px`;
											layer.style.height = `${SIZE}px`;
											if (i === 0) {
												layer.style.display = "flex";
												layer.style.alignItems = "center";
												layer.style.justifyContent = "center";
												layer.className +=
													" text-foreground text-[12px] font-mono tabular-nums font-bold";
												layer.textContent = `${ids.length}`;
											}
											stack.appendChild(layer);
										}
										ghost.appendChild(stack);
									} else {
										const badge = document.createElement("div");
										badge.textContent = row.label;
										badge.className =
											"px-2 py-1 bg-control border border-control-border text-foreground text-[10px] font-mono tabular-nums pointer-events-none whitespace-nowrap";
										ghost.appendChild(badge);
									}

									document.body.appendChild(ghost);
									e.dataTransfer.setDragImage(ghost, 0, 0);
									setTimeout(() => ghost.remove(), 0);

									e.dataTransfer.setData("fixtureIds", JSON.stringify(ids));
									e.dataTransfer.setData("fixtureId", row.fixture.id);
									e.dataTransfer.setData(
										"fixtureLabel",
										row.fixture.label ?? "",
									);
									e.dataTransfer.effectAllowed = "copy";
								}}
								className={cn(
									"grid items-center gap-2 px-3 h-6 text-[10px] font-mono tabular-nums cursor-default border-b border-trim/40 outline-none",
									isInSelection
										? "bg-primary/15"
										: stripe
											? "bg-stripe hover:bg-hover"
											: "bg-card hover:bg-hover",
								)}
								style={{ gridTemplateColumns: GRID_COLS }}
								onClick={(e) => {
									const id = row.fixture.id;
									if (e.shiftKey && lastSelectedPatchedId) {
										const ids = rows.map((r) => r.fixture.id);
										const anchorIdx = ids.indexOf(lastSelectedPatchedId);
										const clickedIdx = ids.indexOf(id);
										if (anchorIdx >= 0 && clickedIdx >= 0) {
											const [start, end] =
												anchorIdx < clickedIdx
													? [anchorIdx, clickedIdx]
													: [clickedIdx, anchorIdx];
											selectFixturesByIds(
												ids.slice(start, end + 1),
												lastSelectedPatchedId,
											);
											return;
										}
									}
									if (e.metaKey || e.ctrlKey) {
										selectFixtureById(id, { shift: true });
										return;
									}
									selectFixtureById(id);
								}}
								onKeyDown={(e) => {
									if (e.key === "Enter" || e.key === " ") {
										e.preventDefault();
										selectFixtureById(row.fixture.id);
									}
								}}
								onContextMenu={(e) => {
									e.preventDefault();
									selectFixtureById(row.fixture.id);
									setContextMenu({
										x: e.clientX,
										y: e.clientY,
										fixtureId: row.fixture.id,
									});
								}}
							>
								<span className="text-right text-foreground/50">{row.id}</span>
								{isEditing ? (
									<input
										ref={inputRef}
										value={editingValue}
										onChange={(e) => setEditingValue(e.target.value)}
										onBlur={commitEdit}
										onClick={(e) => e.stopPropagation()}
										onKeyDown={(e) => {
											if (e.key === "Enter") {
												e.preventDefault();
												void commitEdit();
											} else if (e.key === "Escape") {
												e.preventDefault();
												cancelEdit();
											}
										}}
										className="w-full bg-transparent text-foreground text-[10px] font-mono outline-none border-0 p-0"
									/>
								) : (
									<button
										type="button"
										className="truncate text-left text-foreground bg-transparent border-0 p-0 outline-none"
										onDoubleClick={(e) => {
											e.stopPropagation();
											startEditing(row.fixture.id, row.label);
										}}
									>
										{row.label}
									</button>
								)}
								<span className="truncate text-foreground/70">
									{row.vendor}
								</span>
								<span className="truncate text-foreground/70">{row.mode}</span>
								<span className="text-right text-foreground/70">
									{row.universe}
								</span>
								<span className="text-right text-foreground/70">
									{row.addr}
								</span>
								<span className="text-right text-foreground/70">
									{row.rangeStart}-{row.rangeEnd}
								</span>
								<span className="text-right text-foreground/70">{row.ch}</span>
								<span className="truncate text-foreground/70">
									{row.group || <span className="text-foreground/30">—</span>}
								</span>
							</div>
						);
					})
				)}
			</div>

			{contextMenu && (
				<div
					role="menu"
					className="fixed z-50 min-w-[120px] bg-control border border-control-border"
					style={{ left: contextMenu.x, top: contextMenu.y }}
					onClick={(e) => e.stopPropagation()}
					onKeyDown={(e) => e.stopPropagation()}
				>
					<button
						type="button"
						className="flex items-center w-full px-3 h-[22px] text-left text-[9px] uppercase tracking-wider font-bold leading-none text-foreground/90 hover:bg-hover hover:text-foreground"
						onClick={() => handleContextMenuAction("duplicate")}
					>
						Duplicate
					</button>
					<button
						type="button"
						className="flex items-center w-full px-3 h-[22px] text-left text-[9px] uppercase tracking-wider font-bold leading-none text-destructive hover:bg-destructive/20"
						onClick={() => handleContextMenuAction("unpatch")}
					>
						Unpatch
					</button>
				</div>
			)}
		</div>
	);
}
