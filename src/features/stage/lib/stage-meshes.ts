/**
 * Registry of placeable stage meshes.
 *
 * Each entry binds a stable `meshPath` (stored in the DB) to its bundled
 * GLB URL plus palette metadata and a **hand-authored socket list**.
 *
 * Socket positions are expressed relative to the mesh's measured bounding
 * box via {@link BboxAnchor} primitives — so they stay correct regardless
 * of where the modeller placed the GLB pivot.
 *
 * If a snap doesn't land where you expect, tune the socket's `anchor` /
 * `offset` here. The geometry is the source of truth; this file is the
 * adapter that tells the snap solver where the meaningful points are.
 */

import cableCoverUrl from "../../../../resources/meshes/stage_lab/cable_cover.glb?url";
import cdj3000xUrl from "../../../../resources/meshes/stage_lab/cdj_3000x.glb?url";
import guardrailUrl from "../../../../resources/meshes/stage_lab/guardrail.glb?url";
import mixerDjmA9Url from "../../../../resources/meshes/stage_lab/mixer_djm_a9.glb?url";
import speakerDbr15Url from "../../../../resources/meshes/stage_lab/speaker_dbr15.glb?url";
import speakerDual18subUrl from "../../../../resources/meshes/stage_lab/speaker_dual18sub.glb?url";
import speakerEvent212aUrl from "../../../../resources/meshes/stage_lab/speaker_event_212a.glb?url";
import speakerJblVtxV20Url from "../../../../resources/meshes/stage_lab/speaker_jbl_vtx_v20.glb?url";
import speakerStandUrl from "../../../../resources/meshes/stage_lab/speaker_stand.glb?url";
import stagePraticavel1x1Url from "../../../../resources/meshes/stage_lab/stage_praticavel_1x1.glb?url";
import stagePraticavel2x1x1Url from "../../../../resources/meshes/stage_lab/stage_praticavel_2x1x1.glb?url";
import trussQ30_122Url from "../../../../resources/meshes/stage_lab/truss_q30_1.22m.glb?url";
import trussQ30BoxUrl from "../../../../resources/meshes/stage_lab/truss_q30_box.glb?url";
import trussQ30x45RectUrl from "../../../../resources/meshes/stage_lab/truss_q30x45_rect.glb?url";
import trussQ40_183Url from "../../../../resources/meshes/stage_lab/truss_q40_1.83m.glb?url";
import type { SocketDef } from "./sockets";

export type StageKind =
	| "floor"
	| "truss"
	| "speaker"
	| "cdj"
	| "mixer"
	| "guardrail"
	| "stand"
	| "cable_cover";

export type PaletteGroup =
	| "Stage"
	| "Trusses"
	| "Speakers"
	| "Equipment"
	| "Accessories";

export interface StageMeshDef {
	meshPath: string;
	url: string;
	kind: StageKind;
	displayName: string;
	paletteGroup: PaletteGroup;
	/** Hand-authored sockets. Resolved to local-space positions via Box3 at load. */
	sockets: SocketDef[];
}

// Truss corner inset: how far inward (in metres) from a stage corner the
// corner socket sits, so a 30mm-section truss visually rests at the corner.
const TRUSS_INSET = 0.15;

const FLOOR_1X1_SOCKETS: SocketDef[] = [
	// Centroid grab — the placement reference cursor follows.
	{ name: "grab", type: "grab", anchor: "center" },
	// Bottom socket so a deck placed on an empty stage rests on the
	// ground (Y=0) instead of being half-buried.
	{
		name: "bottom",
		type: "bottom_mount",
		anchor: "bottom",
		normal: [0, -1, 0],
	},
	// No discrete floor_top socket: equipment/speakers land on a deck top
	// via the cursor-raycast "surface" fallback, which places them at the
	// actual hit point and parents them to the deck.
	// 4 edge mid-points along the top. `mode: "edge"` keeps the held deck
	// upright when it snaps to an adjacent deck — only the in-edge tangent
	// flips, not the up-axis.
	{
		name: "edge_front",
		type: "floor_edge",
		anchor: "top_front",
		tangent: [1, 0, 0],
		mode: "edge",
	},
	{
		name: "edge_back",
		type: "floor_edge",
		anchor: "top_back",
		tangent: [1, 0, 0],
		mode: "edge",
	},
	{
		name: "edge_left",
		type: "floor_edge",
		anchor: "top_left",
		tangent: [0, 0, 1],
		mode: "edge",
	},
	{
		name: "edge_right",
		type: "floor_edge",
		anchor: "top_right",
		tangent: [0, 0, 1],
		mode: "edge",
	},
	// 4 corners on the top, inset by truss radius. Normal +Y so trusses
	// hang vertically. (Explicit normal since corner anchors are ambiguous.)
	{
		name: "corner_fl",
		type: "floor_corner",
		anchor: "top_front_left",
		offset: [TRUSS_INSET, 0, -TRUSS_INSET],
		normal: [0, 1, 0],
	},
	{
		name: "corner_fr",
		type: "floor_corner",
		anchor: "top_front_right",
		offset: [-TRUSS_INSET, 0, -TRUSS_INSET],
		normal: [0, 1, 0],
	},
	{
		name: "corner_bl",
		type: "floor_corner",
		anchor: "top_back_left",
		offset: [TRUSS_INSET, 0, TRUSS_INSET],
		normal: [0, 1, 0],
	},
	{
		name: "corner_br",
		type: "floor_corner",
		anchor: "top_back_right",
		offset: [-TRUSS_INSET, 0, TRUSS_INSET],
		normal: [0, 1, 0],
	},
];

// 2×1 stage uses the same socket topology; bbox does the size scaling.
const FLOOR_2X1_SOCKETS: SocketDef[] = FLOOR_1X1_SOCKETS;

// Straight truss: two endpoints along the longest axis (assumed to be X
// in the GLB). truss_end normal points outward along the truss axis so
// that two trusses joined end-to-end face each other (180° opposing).
const STRAIGHT_TRUSS_SOCKETS: SocketDef[] = [
	{ name: "grab", type: "grab", anchor: "center" },
	{ name: "end_a", type: "truss_end", anchor: "left", normal: [-1, 0, 0] },
	{ name: "end_b", type: "truss_end", anchor: "right", normal: [1, 0, 0] },
];

// Box truss (closed frame, 30cm cube): one truss_end socket at the centre
// of each of the cube's six faces. Trusses (straight or another box)
// connect face-to-face from any direction.
const BOX_TRUSS_SOCKETS: SocketDef[] = [
	{ name: "grab", type: "grab", anchor: "center" },
	{ name: "face_top", type: "truss_end", anchor: "top", normal: [0, 1, 0] },
	{
		name: "face_bottom",
		type: "truss_end",
		anchor: "bottom",
		normal: [0, -1, 0],
	},
	{ name: "face_left", type: "truss_end", anchor: "left", normal: [-1, 0, 0] },
	{ name: "face_right", type: "truss_end", anchor: "right", normal: [1, 0, 0] },
	{ name: "face_front", type: "truss_end", anchor: "front", normal: [0, 0, 1] },
	{ name: "face_back", type: "truss_end", anchor: "back", normal: [0, 0, -1] },
];

const SPEAKER_SOCKETS: SocketDef[] = [
	{ name: "grab", type: "grab", anchor: "center" },
	{
		name: "mount",
		type: "speaker_mount",
		anchor: "bottom",
		normal: [0, -1, 0],
	},
];

// Speaker stand pole axis sits ~10cm off the bbox X-center: the three
// tripod feet aren't placed symmetrically about the bbox, so the bbox
// extreme on +X is dominated by one leg while -X is dominated by another.
// Shift the anchors onto the actual pole so a mounted speaker sits centred
// on it, and the base snaps to where the pole meets the floor.
const SPEAKER_STAND_POLE_OFFSET: [number, number, number] = [0.1, 0, 0];

const STAND_SOCKETS: SocketDef[] = [
	{
		name: "grab",
		type: "grab",
		anchor: "center",
		offset: SPEAKER_STAND_POLE_OFFSET,
	},
	{
		name: "top",
		type: "stand_top",
		anchor: "top",
		offset: SPEAKER_STAND_POLE_OFFSET,
		normal: [0, 1, 0],
	},
	{
		name: "base",
		type: "stand_bottom",
		anchor: "bottom",
		offset: SPEAKER_STAND_POLE_OFFSET,
		normal: [0, -1, 0],
	},
];

const EQUIPMENT_SOCKETS: SocketDef[] = [
	{ name: "grab", type: "grab", anchor: "center" },
	{
		name: "mount",
		type: "equipment_mount",
		anchor: "bottom",
		normal: [0, -1, 0],
	},
];

const GUARDRAIL_SOCKETS: SocketDef[] = [
	{ name: "grab", type: "grab", anchor: "center" },
	{
		name: "bottom",
		type: "bottom_mount",
		anchor: "bottom",
		normal: [0, -1, 0],
	},
	{
		name: "end_a",
		type: "rail_end",
		anchor: "left",
		offset: [0.012, 0, 0],
		normal: [-1, 0, 0],
		tangent: [0, 0, 1],
	},
	{
		name: "end_b",
		type: "rail_end",
		anchor: "right",
		offset: [-0.012, 0, 0],
		normal: [1, 0, 0],
		tangent: [0, 0, 1],
	},
];

// Cable cover lies flat on the floor with its long axis along Z; the two
// short faces (+Z "front" and -Z "back") snap to other cable covers'
// matching ends so they chain into runs. Tiny inset on the normal axis
// keeps the meshes from z-fighting when butted together.
const CABLE_COVER_END_INSET = 0.005;

const CABLE_COVER_SOCKETS: SocketDef[] = [
	{ name: "grab", type: "grab", anchor: "center" },
	{
		name: "mount",
		type: "equipment_mount",
		anchor: "bottom",
		normal: [0, -1, 0],
	},
	{
		name: "end_front",
		type: "cable_end",
		anchor: "front",
		offset: [0, 0, -CABLE_COVER_END_INSET],
		normal: [0, 0, 1],
		tangent: [1, 0, 0],
	},
	{
		name: "end_back",
		type: "cable_end",
		anchor: "back",
		offset: [0, 0, CABLE_COVER_END_INSET],
		normal: [0, 0, -1],
		tangent: [1, 0, 0],
	},
];

const REGISTRY: Record<string, StageMeshDef> = {
	"stage_lab/stage_praticavel_1x1.glb": {
		meshPath: "stage_lab/stage_praticavel_1x1.glb",
		url: stagePraticavel1x1Url,
		kind: "floor",
		displayName: "Stage Deck 1×1m",
		paletteGroup: "Stage",
		sockets: FLOOR_1X1_SOCKETS,
	},
	"stage_lab/stage_praticavel_2x1x1.glb": {
		meshPath: "stage_lab/stage_praticavel_2x1x1.glb",
		url: stagePraticavel2x1x1Url,
		kind: "floor",
		displayName: "Stage Deck 2×1m",
		paletteGroup: "Stage",
		sockets: FLOOR_2X1_SOCKETS,
	},
	"stage_lab/truss_q30_1.22m.glb": {
		meshPath: "stage_lab/truss_q30_1.22m.glb",
		url: trussQ30_122Url,
		kind: "truss",
		displayName: "Truss Q30 · 1.22m",
		paletteGroup: "Trusses",
		sockets: STRAIGHT_TRUSS_SOCKETS,
	},
	"stage_lab/truss_q40_1.83m.glb": {
		meshPath: "stage_lab/truss_q40_1.83m.glb",
		url: trussQ40_183Url,
		kind: "truss",
		displayName: "Truss Q40 · 1.83m",
		paletteGroup: "Trusses",
		sockets: STRAIGHT_TRUSS_SOCKETS,
	},
	"stage_lab/truss_q30_box.glb": {
		meshPath: "stage_lab/truss_q30_box.glb",
		url: trussQ30BoxUrl,
		kind: "truss",
		displayName: "Truss Q30 Box",
		paletteGroup: "Trusses",
		sockets: BOX_TRUSS_SOCKETS,
	},
	"stage_lab/truss_q30x45_rect.glb": {
		meshPath: "stage_lab/truss_q30x45_rect.glb",
		url: trussQ30x45RectUrl,
		kind: "truss",
		displayName: "Truss Q30×45 Rect",
		paletteGroup: "Trusses",
		sockets: BOX_TRUSS_SOCKETS,
	},
	"stage_lab/speaker_dbr15.glb": {
		meshPath: "stage_lab/speaker_dbr15.glb",
		url: speakerDbr15Url,
		kind: "speaker",
		displayName: "Yamaha DBR15",
		paletteGroup: "Speakers",
		sockets: SPEAKER_SOCKETS,
	},
	"stage_lab/speaker_dual18sub.glb": {
		meshPath: "stage_lab/speaker_dual18sub.glb",
		url: speakerDual18subUrl,
		kind: "speaker",
		displayName: 'Dual 18" Sub',
		paletteGroup: "Speakers",
		sockets: SPEAKER_SOCKETS,
	},
	"stage_lab/speaker_event_212a.glb": {
		meshPath: "stage_lab/speaker_event_212a.glb",
		url: speakerEvent212aUrl,
		kind: "speaker",
		displayName: "Event 212A",
		paletteGroup: "Speakers",
		sockets: SPEAKER_SOCKETS,
	},
	"stage_lab/speaker_jbl_vtx_v20.glb": {
		meshPath: "stage_lab/speaker_jbl_vtx_v20.glb",
		url: speakerJblVtxV20Url,
		kind: "speaker",
		displayName: "JBL VTX V20",
		paletteGroup: "Speakers",
		sockets: SPEAKER_SOCKETS,
	},
	"stage_lab/speaker_stand.glb": {
		meshPath: "stage_lab/speaker_stand.glb",
		url: speakerStandUrl,
		kind: "stand",
		displayName: "Speaker Stand",
		paletteGroup: "Accessories",
		sockets: STAND_SOCKETS,
	},
	"stage_lab/cdj_3000x.glb": {
		meshPath: "stage_lab/cdj_3000x.glb",
		url: cdj3000xUrl,
		kind: "cdj",
		displayName: "CDJ-3000",
		paletteGroup: "Equipment",
		sockets: EQUIPMENT_SOCKETS,
	},
	"stage_lab/mixer_djm_a9.glb": {
		meshPath: "stage_lab/mixer_djm_a9.glb",
		url: mixerDjmA9Url,
		kind: "mixer",
		displayName: "DJM-A9 Mixer",
		paletteGroup: "Equipment",
		sockets: EQUIPMENT_SOCKETS,
	},
	"stage_lab/guardrail.glb": {
		meshPath: "stage_lab/guardrail.glb",
		url: guardrailUrl,
		kind: "guardrail",
		displayName: "Guardrail",
		paletteGroup: "Accessories",
		sockets: GUARDRAIL_SOCKETS,
	},
	"stage_lab/cable_cover.glb": {
		meshPath: "stage_lab/cable_cover.glb",
		url: cableCoverUrl,
		kind: "cable_cover",
		displayName: "Cable Cover",
		paletteGroup: "Accessories",
		sockets: CABLE_COVER_SOCKETS,
	},
};

export function getStageMesh(meshPath: string): StageMeshDef | null {
	return REGISTRY[meshPath] ?? null;
}

export function listStageMeshes(): StageMeshDef[] {
	return Object.values(REGISTRY);
}

export const PALETTE_GROUP_ORDER: PaletteGroup[] = [
	"Stage",
	"Trusses",
	"Speakers",
	"Equipment",
	"Accessories",
];
