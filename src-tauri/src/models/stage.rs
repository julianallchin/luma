use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// A placeable set-design object (stage floor, truss, speaker, ...).
///
/// Coordinates are Z-up to match fixtures; the renderer swaps Y<->Z when
/// drawing into three.js (Y-up).
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/stage.ts")]
#[ts(rename_all = "camelCase")]
pub struct StagePiece {
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,

    /// GLB asset path relative to `resources/meshes/`
    /// (e.g. `"stage_lab/truss_q30_box.glb"`).
    pub mesh_path: String,

    /// Snap/palette taxonomy. One of:
    /// `floor`, `truss`, `speaker`, `cdj`, `mixer`, `guardrail`, `stand`, `cable_cover`.
    pub kind: String,

    pub label: Option<String>,

    pub pos_x: f64,
    pub pos_y: f64,
    pub pos_z: f64,
    pub rot_x: f64,
    pub rot_y: f64,
    pub rot_z: f64,
    pub scale: f64,

    /// If set, this piece is attached to another piece and its pos/rot are in
    /// parent-local space. Moving the parent moves this piece.
    pub parent_piece_id: Option<String>,
}
