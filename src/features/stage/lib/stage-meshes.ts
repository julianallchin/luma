/**
 * The stage catalog, as the React app sees it.
 *
 * The catalog itself is Rust — `gpui/crates/scene/src/catalog.rs` — projected
 * into `./catalog.generated.ts`. This module adds the one thing Rust cannot
 * supply: the bundler's URL for each GLB. Everything else (ids, display names,
 * palette groups, socket definitions) is read from the generated file, so
 * there is no second copy to drift.
 *
 * If a snap doesn't land where you expect, tune the socket's anchor / offset
 * in the Rust catalog and rerun `cargo test -p luma-scene`. The geometry is
 * the source of truth; the catalog is the adapter that tells the snap solver
 * where the meaningful points are.
 */

import {
	CATALOG,
	type CatalogPiece,
	PALETTE_GROUP_ORDER,
	type PaletteGroup,
	type PieceKind,
} from "./catalog.generated";

export type { CatalogPiece, PaletteGroup, PieceKind };
export { PALETTE_GROUP_ORDER };

/**
 * Every bundled stage GLB, by path relative to `resources/meshes/`.
 *
 * A glob rather than a hand-written import list: the mapping is "which file",
 * not a design decision, and a list of fifteen imports next to a catalog of
 * fifteen pieces is two places to add a mesh. It also keeps URLs available for
 * meshes the catalog no longer offers — the ripped truss GLBs are out of the
 * palette but venues built before the procedural family still reference them,
 * and a venue that cannot draw its own trusses is worse than one that cannot
 * add more.
 */
const MESH_URLS: Record<string, string> = Object.fromEntries(
	Object.entries(
		import.meta.glob("../../../../resources/meshes/**/*.glb", {
			query: "?url",
			import: "default",
			eager: true,
		}) as Record<string, string>,
	).map(([path, url]) => [path.replace(/^.*\/resources\/meshes\//, ""), url]),
);

/** The bundled URL for a mesh path, or `null` if no such GLB ships. */
export function meshUrl(meshPath: string): string | null {
	return MESH_URLS[meshPath] ?? null;
}

const BY_ID = new Map(CATALOG.map((p) => [p.id, p]));

/**
 * The catalog entry with this id, or `null`. A venue may hold pieces the
 * catalog has dropped (the ripped trusses), so callers must handle `null`
 * rather than assume placement implies a catalog entry.
 */
export function getStageMesh(id: string): CatalogPiece | null {
	return BY_ID.get(id) ?? null;
}

export function listStageMeshes(): CatalogPiece[] {
	return CATALOG;
}

/**
 * Whether a piece is drawable by this app. React has no truss generator, so a
 * procedural piece has no geometry here — it is placed and rendered by the
 * gpui builder.
 */
export function stageMeshUrl(piece: CatalogPiece): string | null {
	return piece.geometry.kind === "mesh" ? meshUrl(piece.geometry.path) : null;
}
