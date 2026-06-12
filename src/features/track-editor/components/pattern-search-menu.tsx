import { Globe, Plus, Search } from "lucide-react";
import {
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { createPortal } from "react-dom";
import type { PatternSummary, SearchPatternRow } from "@/bindings/schema";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { CreatePatternDialog } from "@/features/patterns/components/create-pattern-dialog";
import { usePatternsStore } from "@/features/patterns/stores/use-patterns-store";
import { cn } from "@/shared/lib/utils";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { getPatternColor } from "../utils/timeline-constants";
import { fetchPreviewFrames, PreviewCanvas } from "./pattern-preview";

const MENU_WIDTH = 460;
const MENU_HEIGHT = 360;

// One unified row whether it came from the local registry or a remote search.
type Row = {
	id: string;
	name: string;
	authorName: string | null;
	isVerified: boolean;
	isOwn: boolean;
	source: "local" | "remote";
};

type PatternSearchMenuProps = {
	anchor: { x: number; y: number };
	onSelect: (patternId: string) => void;
	onClose: () => void;
	/** Fired as the highlighted row changes, so the canvas ghost can adopt its
	 *  name/colour. Receives null when nothing is highlighted. */
	onActiveChange?: (row: { id: string; name: string } | null) => void;
};

export function PatternSearchMenu({
	anchor,
	onSelect,
	onClose,
	onActiveChange,
}: PatternSearchMenuProps) {
	const patterns = useTrackEditorStore((s) => s.patterns);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);
	const trackId = useTrackEditorStore((s) => s.trackId);
	const venueId = useTrackEditorStore((s) => s.venueId);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
	const currentUserId = useAuthStore((s) => s.user?.id ?? null);

	const searchRemote = usePatternsStore((s) => s.searchRemote);
	const searchResults = usePatternsStore((s) => s.searchResults);

	const [query, setQuery] = useState("");
	const [activeIndex, setActiveIndex] = useState(0);

	const menuRef = useRef<HTMLDivElement>(null);
	const inputRef = useRef<HTMLInputElement>(null);

	// ── Position: clamp the menu inside the viewport, flipping when it would
	// overflow the right / bottom edge. ──
	const [pos, setPos] = useState({ left: anchor.x, top: anchor.y });
	useLayoutEffect(() => {
		const pad = 8;
		const w = menuRef.current?.offsetWidth ?? MENU_WIDTH;
		const h = menuRef.current?.offsetHeight ?? MENU_HEIGHT;
		let left = anchor.x;
		let top = anchor.y;
		if (left + w + pad > window.innerWidth) left = anchor.x - w;
		if (top + h + pad > window.innerHeight) top = window.innerHeight - h - pad;
		setPos({ left: Math.max(pad, left), top: Math.max(pad, top) });
	}, [anchor.x, anchor.y]);

	useEffect(() => {
		inputRef.current?.focus();
	}, []);

	// Debounced remote search — only when the user has typed something.
	useEffect(() => {
		const q = query.trim();
		if (!q) return;
		const t = setTimeout(() => searchRemote(q), 250);
		return () => clearTimeout(t);
	}, [query, searchRemote]);

	const localRows = useMemo<Row[]>(() => {
		const q = query.trim().toLowerCase();
		return patterns
			.filter((p: PatternSummary) =>
				q ? p.name.toLowerCase().includes(q) : true,
			)
			.sort((a, b) => a.name.localeCompare(b.name))
			.map((p) => ({
				id: p.id,
				name: p.name,
				authorName: p.authorName,
				isVerified: p.isVerified,
				isOwn: p.uid === currentUserId,
				source: "local" as const,
			}));
	}, [patterns, query, currentUserId]);

	const remoteRows = useMemo<Row[]>(() => {
		if (!query.trim()) return [];
		const localIds = new Set(localRows.map((r) => r.id));
		return searchResults
			.filter((p: SearchPatternRow) => !localIds.has(p.id))
			.map((p) => ({
				id: p.id,
				name: p.name,
				authorName: p.authorName,
				isVerified: p.isVerified,
				isOwn: p.uid === currentUserId,
				source: "remote" as const,
			}));
	}, [searchResults, query, localRows, currentUserId]);

	const flatRows = useMemo(
		() => [...localRows, ...remoteRows],
		[localRows, remoteRows],
	);

	// Keep the highlight in range as the result set changes.
	useEffect(() => {
		setActiveIndex((i) => (i >= flatRows.length ? 0 : i));
	}, [flatRows.length]);

	const activeRow = flatRows[activeIndex] ?? null;
	const activeId = activeRow?.id ?? null;
	const activeName = activeRow?.name ?? null;

	// Keep the parent callback in a ref so notifying it (which sets state in the
	// parent, handing us a fresh callback identity) doesn't re-fire our effects.
	const onActiveChangeRef = useRef(onActiveChange);
	useEffect(() => {
		onActiveChangeRef.current = onActiveChange;
	}, [onActiveChange]);

	// Tell the parent which pattern is highlighted (for the canvas ghost).
	useEffect(() => {
		onActiveChangeRef.current?.(
			activeId ? { id: activeId, name: activeName ?? "" } : null,
		);
	}, [activeId, activeName]);

	// ── Preview of the highlighted pattern (reuses the shared WebGL canvas). ──
	const previewDataRef = useRef<{
		frames: import("@/bindings/universe").UniverseState[];
		durationSec: number;
	} | null>(null);
	const [, setPreviewTick] = useState(0);
	useEffect(() => {
		previewDataRef.current = null;
		setPreviewTick((t) => t + 1);
		if (!activeId || !trackId || !venueId) return;
		const { promise, cancel } = fetchPreviewFrames(
			activeId,
			trackId,
			venueId,
			beatGrid,
			playheadPosition,
		);
		promise.then((data) => {
			if (data) {
				previewDataRef.current = data;
				setPreviewTick((t) => t + 1);
			}
		});
		return cancel;
	}, [activeId, trackId, venueId, beatGrid, playheadPosition]);

	const commit = useCallback(
		(row: Row | null) => {
			if (!row) return;
			onSelect(row.id);
			onClose();
		},
		[onSelect, onClose],
	);

	const handleKeyDown = (e: React.KeyboardEvent) => {
		if (e.key === "ArrowDown") {
			e.preventDefault();
			setActiveIndex((i) => Math.min(i + 1, flatRows.length - 1));
		} else if (e.key === "ArrowUp") {
			e.preventDefault();
			setActiveIndex((i) => Math.max(i - 1, 0));
		} else if (e.key === "Enter") {
			e.preventDefault();
			commit(activeRow);
		} else if (e.key === "Escape") {
			e.preventDefault();
			onClose();
		}
	};

	let flatIdx = -1;
	const renderRow = (row: Row) => {
		flatIdx += 1;
		const idx = flatIdx;
		const active = idx === activeIndex;
		return (
			// biome-ignore lint/a11y/noStaticElementInteractions: command-palette row
			<div
				key={`${row.source}:${row.id}`}
				onMouseEnter={() => setActiveIndex(idx)}
				onMouseDown={(e) => {
					e.preventDefault();
					commit(row);
				}}
				className={cn(
					"flex items-center gap-2 px-2 py-1.5 cursor-pointer select-none text-xs",
					active ? "bg-hover" : "hover:bg-hover/60",
				)}
			>
				<div className="relative w-3 h-3 flex-shrink-0">
					<div
						className="w-3 h-3"
						style={{ backgroundColor: getPatternColor(row.id) }}
					/>
					{row.isVerified && (
						<Globe className="absolute -top-1 -right-1 w-2 h-2 text-primary" />
					)}
				</div>
				<span className="flex-1 min-w-0 truncate text-foreground/90">
					{row.name}
				</span>
				<span className="text-[10px] text-muted-foreground truncate max-w-[40%]">
					{row.isOwn
						? "by you"
						: row.authorName
							? `by ${row.authorName}`
							: row.isVerified
								? "verified"
								: ""}
				</span>
			</div>
		);
	};

	return createPortal(
		<>
			{/* Backdrop: click anywhere to dismiss. Sits below the menu. */}
			{/* biome-ignore lint/a11y/noStaticElementInteractions: dismiss layer */}
			<div className="fixed inset-0 z-40" onMouseDown={onClose} />

			{/* biome-ignore lint/a11y/noStaticElementInteractions: floating menu */}
			<div
				ref={menuRef}
				className="fixed z-50 flex flex-col border border-[rgb(8_8_8)] bg-card text-xs"
				style={{ left: pos.left, top: pos.top, width: MENU_WIDTH }}
				onMouseDown={(e) => e.stopPropagation()}
				onKeyDown={handleKeyDown}
			>
				{/* Search input */}
				<div className="flex items-center gap-2 px-2 border-b border-trim">
					<Search className="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
					<input
						ref={inputRef}
						type="text"
						value={query}
						onChange={(e) => setQuery(e.target.value)}
						placeholder="search patterns…"
						autoCapitalize="off"
						autoCorrect="off"
						spellCheck={false}
						className="flex-1 bg-transparent py-2 text-xs placeholder:text-muted-foreground focus:outline-none"
					/>
				</div>

				{/* Body: result list + live preview */}
				<div className="flex" style={{ height: MENU_HEIGHT - 64 }}>
					<div className="w-1/2 overflow-y-auto border-r border-trim py-1">
						{flatRows.length === 0 ? (
							<div className="px-3 py-4 text-[11px] text-muted-foreground">
								No patterns
							</div>
						) : (
							<>
								{localRows.map(renderRow)}
								{remoteRows.length > 0 && (
									<div className="px-2 pt-2 pb-0.5 text-[9px] uppercase tracking-wider text-muted-foreground/70">
										Community
									</div>
								)}
								{remoteRows.map(renderRow)}
							</>
						)}
					</div>
					<div className="w-1/2 relative bg-black">
						{previewDataRef.current ? (
							<PreviewCanvas previewDataRef={previewDataRef} />
						) : (
							<div className="absolute inset-0 flex items-center justify-center text-[10px] text-muted-foreground">
								{activeRow ? "rendering…" : "no preview"}
							</div>
						)}
					</div>
				</div>

				{/* Footer */}
				<div className="flex items-center justify-between gap-2 px-2 py-1.5 border-t border-trim">
					<CreatePatternDialog
						trigger={
							<button
								type="button"
								className="flex items-center gap-1 text-[10px] uppercase tracking-wider font-bold text-muted-foreground hover:text-foreground"
							>
								<Plus className="w-3 h-3" /> create pattern
							</button>
						}
						onCreated={() => loadPatterns()}
					/>
					<span className="text-[10px] text-muted-foreground/70">
						↵ insert · esc cancel
					</span>
				</div>
			</div>
		</>,
		document.body,
	);
}
