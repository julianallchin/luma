import { invoke } from "@tauri-apps/api/core";
import { Euler, Matrix4, Quaternion, Vector3 } from "three";
import { create } from "zustand";
import type { StagePiece } from "@/bindings/stage";
import { getPieceGroup } from "../lib/piece-refs";
import { descendantIdsOf } from "../lib/tree";

interface StagePieceState {
	venueId: string | null;
	pieces: StagePiece[];

	/**
	 * Set of *user-picked* piece IDs. The cluster a picked piece belongs
	 * to is expanded at read time (via `clusterMembersOf`) — we only
	 * store the actual picks here so the set doesn't grow stale when
	 * pieces get reparented.
	 */
	selectedIds: Set<string>;

	/** Most-recently picked piece — drives the gizmo pivot and "primary" outline. */
	lastSelectedId: string | null;

	/** Back-compat alias for the primary selection. Same as `lastSelectedId`. */
	selectedId: string | null;

	// Hover (for cluster-outline preview)
	hoveredId: string | null;

	// Placement (arm-from-palette flow)
	armedMeshPath: string | null;
	/**
	 * World-space (three.js Y-up) snapshot of the latest ghost target.
	 * Read by the visualizer's commit handler to know where the cursor
	 * was when the user clicked.
	 */
	ghost: {
		position: [number, number, number];
		quaternion: [number, number, number, number];
		parentId: string | null;
	} | null;

	// Lifecycle
	initialize: (venueId: string) => Promise<void>;
	refresh: () => Promise<void>;

	// Placement flow
	armPlace: (meshPath: string) => void;
	cancelPlace: () => void;
	setGhost: (ghost: StagePieceState["ghost"]) => void;
	commitPlace: () => Promise<StagePiece | null>;

	/**
	 * Commit a piece's pose given its three.js world transform. Resolves
	 * parent-local storage using `parentPieceId` from the current piece
	 * (live group refs are walked to invert the parent's world matrix).
	 *
	 * Note: this does NOT itself trigger snap evaluation; callers run the
	 * solver and pass `parentPieceId` via {@link reparentAndMove} when
	 * attachment changes.
	 */
	movePieceWorld: (
		id: string,
		worldPosition: Vector3,
		worldQuaternion: Quaternion,
	) => Promise<void>;

	/**
	 * Like {@link movePieceWorld} but explicit about which parent the
	 * piece should attach to after the move. Pass `parentId = null` to
	 * detach.
	 */
	reparentAndMove: (
		id: string,
		parentId: string | null,
		worldPosition: Vector3,
		worldQuaternion: Quaternion,
	) => Promise<void>;

	renamePiece: (id: string, label: string) => Promise<void>;
	deletePiece: (id: string) => Promise<void>;

	/**
	 * Delete every selected piece (and recursively, every descendant of
	 * each selected piece — selecting a cluster member effectively
	 * targets the whole subtree below it). Backend `delete_stage_piece`
	 * has no cascade, so we explicitly enumerate the closure here.
	 */
	removeSelectedPieces: () => Promise<void>;

	/**
	 * Select a single piece (replace mode) or toggle it into the existing
	 * selection (shift mode). Passing `id = null` clears selection.
	 */
	selectPieceById: (id: string | null, opts?: { shift?: boolean }) => void;

	/** Bulk-select (e.g. marquee). `primaryId` defaults to the last id. */
	selectPiecesByIds: (ids: string[], primaryId?: string | null) => void;

	clearSelection: () => void;

	/** @deprecated Use selectPieceById. Kept so older call sites compile. */
	selectPiece: (id: string | null) => void;

	// Hover
	setHoveredId: (id: string | null) => void;
}

function dataPoseFromWorld(
	worldPosition: Vector3,
	worldQuaternion: Quaternion,
	parentId: string | null,
): {
	posX: number;
	posY: number;
	posZ: number;
	rotX: number;
	rotY: number;
	rotZ: number;
} {
	// World matrix in three.js Y-up
	const worldMat = new Matrix4().compose(
		worldPosition,
		worldQuaternion,
		new Vector3(1, 1, 1),
	);

	// If attached, express in parent's local frame.
	let localMat = worldMat;
	if (parentId) {
		const parentGroup = getPieceGroup(parentId);
		if (parentGroup) {
			parentGroup.updateMatrixWorld(true);
			const parentInv = parentGroup.matrixWorld.clone().invert();
			localMat = new Matrix4().multiplyMatrices(parentInv, worldMat);
		} else {
			// Parent ref not available — fall back to treating as world (rare
			// race condition; the piece is about to re-render with the fresh
			// ref anyway).
			console.warn(`[stage] parent group ${parentId} missing during move`);
		}
	}

	const localPos = new Vector3();
	const localQ = new Quaternion();
	const localScale = new Vector3();
	localMat.decompose(localPos, localQ, localScale);
	const localEuler = new Euler().setFromQuaternion(localQ, "XYZ");

	// three.js Y-up → data Z-up: data.x = three.x, data.y = three.z, data.z = three.y.
	return {
		posX: localPos.x,
		posY: localPos.z,
		posZ: localPos.y,
		rotX: localEuler.x,
		rotY: localEuler.z,
		rotZ: localEuler.y,
	};
}

export const useStagePieceStore = create<StagePieceState>((set, get) => ({
	venueId: null,
	pieces: [],
	selectedIds: new Set<string>(),
	lastSelectedId: null,
	selectedId: null,
	hoveredId: null,
	armedMeshPath: null,
	ghost: null,

	initialize: async (venueId) => {
		set({ venueId });
		await get().refresh();
	},

	refresh: async () => {
		const { venueId } = get();
		if (!venueId) return;
		try {
			const pieces = await invoke<StagePiece[]>("list_stage_pieces", {
				venueId,
			});
			set((state) => {
				const validIds = new Set(pieces.map((p) => p.id));
				const nextSelected = new Set<string>();
				for (const id of state.selectedIds)
					if (validIds.has(id)) nextSelected.add(id);
				const nextLast =
					state.lastSelectedId && validIds.has(state.lastSelectedId)
						? state.lastSelectedId
						: (nextSelected.values().next().value ?? null);
				return {
					pieces,
					selectedIds: nextSelected,
					lastSelectedId: nextLast,
					selectedId: nextLast,
					hoveredId:
						state.hoveredId && validIds.has(state.hoveredId)
							? state.hoveredId
							: null,
				};
			});
		} catch (err) {
			console.error("[stage] list_stage_pieces failed", err);
		}
	},

	armPlace: (meshPath) => set({ armedMeshPath: meshPath, ghost: null }),

	cancelPlace: () => set({ armedMeshPath: null, ghost: null }),

	setGhost: (ghost) => set({ ghost }),

	commitPlace: async () => {
		const { armedMeshPath, ghost, venueId } = get();
		if (!armedMeshPath || !ghost || !venueId) return null;

		const { getStageMesh } = await import("../lib/stage-meshes");
		const def = getStageMesh(armedMeshPath);
		if (!def) {
			console.error(`[stage] unknown meshPath: ${armedMeshPath}`);
			return null;
		}

		// Ghost stores world-space pose. Convert to parent-local for storage.
		const worldPos = new Vector3(...ghost.position);
		const worldQ = new Quaternion(...ghost.quaternion);
		const local = dataPoseFromWorld(worldPos, worldQ, ghost.parentId);

		const piece = await invoke<StagePiece>("place_stage_piece", {
			venueId,
			meshPath: armedMeshPath,
			kind: def.kind,
			parentPieceId: ghost.parentId,
			posX: local.posX,
			posY: local.posY,
			posZ: local.posZ,
			rotX: local.rotX,
			rotY: local.rotY,
			rotZ: local.rotZ,
			scale: 1.0,
			label: null,
		});
		// Keep `armedMeshPath` set so the user can place multiple instances
		// of the same prop in a row. ESC clears it via cancelPlace().
		set((state) => ({
			pieces: [...state.pieces, piece],
			ghost: null,
		}));
		return piece;
	},

	movePieceWorld: async (id, worldPosition, worldQuaternion) => {
		const piece = get().pieces.find((p) => p.id === id);
		if (!piece) return;
		await get().reparentAndMove(
			id,
			piece.parentPieceId,
			worldPosition,
			worldQuaternion,
		);
	},

	reparentAndMove: async (id, parentId, worldPosition, worldQuaternion) => {
		const local = dataPoseFromWorld(worldPosition, worldQuaternion, parentId);

		await invoke("move_stage_piece", {
			id,
			parentPieceId: parentId,
			posX: local.posX,
			posY: local.posY,
			posZ: local.posZ,
			rotX: local.rotX,
			rotY: local.rotY,
			rotZ: local.rotZ,
		});
		set((state) => ({
			pieces: state.pieces.map((p) =>
				p.id === id
					? {
							...p,
							parentPieceId: parentId,
							posX: local.posX,
							posY: local.posY,
							posZ: local.posZ,
							rotX: local.rotX,
							rotY: local.rotY,
							rotZ: local.rotZ,
						}
					: p,
			),
		}));
	},

	renamePiece: async (id, label) => {
		await invoke("rename_stage_piece", { id, label });
		set((state) => ({
			pieces: state.pieces.map((p) => (p.id === id ? { ...p, label } : p)),
		}));
	},

	deletePiece: async (id) => {
		// Backend cascades via FK (`parent_piece_id ON DELETE CASCADE`),
		// so one call removes the whole subtree on the server. Mirror that
		// in local state by filtering out every descendant too — otherwise
		// children linger as orphans until the next refresh.
		const subtree = descendantIdsOf(get().pieces, id);
		await invoke("delete_stage_piece", { id });
		set((state) => {
			const nextSelected = new Set<string>();
			for (const sid of state.selectedIds)
				if (!subtree.has(sid)) nextSelected.add(sid);
			const nextLast =
				state.lastSelectedId && subtree.has(state.lastSelectedId)
					? (nextSelected.values().next().value ?? null)
					: state.lastSelectedId;
			return {
				pieces: state.pieces.filter((p) => !subtree.has(p.id)),
				selectedIds: nextSelected,
				lastSelectedId: nextLast,
				selectedId: nextLast,
				hoveredId:
					state.hoveredId && subtree.has(state.hoveredId)
						? null
						: state.hoveredId,
			};
		});
	},

	removeSelectedPieces: async () => {
		const { selectedIds, pieces } = get();
		if (selectedIds.size === 0) return;

		// Expand to include every descendant so local state stays in sync
		// with the backend's FK cascade. (We only need to invoke
		// `delete_stage_piece` on each selected root — children cascade —
		// but enumerating the closure lets us filter local state in one
		// pass and survive duplicate selections.)
		const toDelete = new Set<string>();
		for (const id of selectedIds) {
			for (const d of descendantIdsOf(pieces, id)) toDelete.add(d);
		}

		try {
			await Promise.all(
				[...selectedIds].map((id) => invoke("delete_stage_piece", { id })),
			);
			set((state) => ({
				pieces: state.pieces.filter((p) => !toDelete.has(p.id)),
				selectedIds: new Set<string>(),
				lastSelectedId: null,
				selectedId: null,
				hoveredId:
					state.hoveredId && toDelete.has(state.hoveredId)
						? null
						: state.hoveredId,
			}));
		} catch (err) {
			console.error("[stage] removeSelectedPieces failed", err);
			await get().refresh();
		}
	},

	selectPieceById: (id, opts) => {
		set((state) => {
			if (id === null) {
				return {
					selectedIds: new Set<string>(),
					lastSelectedId: null,
					selectedId: null,
				};
			}
			if (opts?.shift) {
				const next = new Set(state.selectedIds);
				if (next.has(id)) {
					next.delete(id);
					const nextLast =
						state.lastSelectedId === id
							? (next.values().next().value ?? null)
							: state.lastSelectedId;
					return {
						selectedIds: next,
						lastSelectedId: nextLast,
						selectedId: nextLast,
					};
				}
				next.add(id);
				return { selectedIds: next, lastSelectedId: id, selectedId: id };
			}
			return {
				selectedIds: new Set<string>([id]),
				lastSelectedId: id,
				selectedId: id,
			};
		});
	},

	selectPiecesByIds: (ids, primaryId) => {
		const fallback = ids.length > 0 ? ids[ids.length - 1] : null;
		const primary =
			primaryId !== undefined && primaryId !== null && ids.includes(primaryId)
				? primaryId
				: fallback;
		set({
			selectedIds: new Set(ids),
			lastSelectedId: primary,
			selectedId: primary,
		});
	},

	clearSelection: () => {
		set({
			selectedIds: new Set<string>(),
			lastSelectedId: null,
			selectedId: null,
		});
	},

	selectPiece: (id) => {
		get().selectPieceById(id);
	},

	setHoveredId: (id) => set({ hoveredId: id }),
}));
