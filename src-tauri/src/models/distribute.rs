//! The `distribute` command across the host boundary.
//!
//! [`crate::services::distribute`] owns the command; these are its argument and
//! its answer in the shapes `serde` and `ts-rs` can carry. A projection, not a
//! second declaration — nothing here computes.

use luma_scene::distribute::Layout;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::services::distribute::{FitFailure, Placed, Report};

/// How the caller pinned the row's layout down.
///
/// A tagged union rather than two nullable fields, because "spacing *and* a
/// span" is not a distribution anybody can mean and should not be a pair
/// anybody can send. `even` carries nothing: it is the whole face.
#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub enum DistributeLayout {
    /// Evenly across the whole face, a half-fixture margin at each end.
    Even,
    /// A fixed centre-to-centre pitch, in metres, centred on the face.
    Spacing { metres: f64 },
    /// Evenly across the fraction `from..to` of the face, `0` at its
    /// negative-tangent end.
    Span { from: f64, to: f64 },
}

impl From<DistributeLayout> for Layout {
    fn from(layout: DistributeLayout) -> Layout {
        match layout {
            DistributeLayout::Even => Layout::Even,
            DistributeLayout::Spacing { metres } => Layout::Spacing(metres),
            DistributeLayout::Span { from, to } => Layout::Span(from, to),
        }
    }
}

/// One fixture a distribution created.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub struct DistributedFixture {
    /// The `fixtures` row id, which is also its venue-graph node id.
    pub id: String,
    pub label: String,
    pub universe: u16,
    pub address: u16,
    /// Metres along the host face from its middle, ascending across the row.
    pub along_m: f64,
    /// The deepest derived group it landed in, as a display path.
    pub group_path: Vec<String>,
}

impl From<&Placed> for DistributedFixture {
    fn from(placed: &Placed) -> Self {
        DistributedFixture {
            id: placed.id.clone(),
            label: placed.label.clone(),
            universe: placed.universe,
            address: placed.address,
            along_m: placed.along_m,
            group_path: placed.group_path.clone(),
        }
    }
}

/// Why the row did not fit, and the call that would make it.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub struct DistributeFit {
    /// How long the host face would have to be. Already a length the host can
    /// be built at, so it can be passed straight to `set_params`.
    pub needed_m: f64,
    /// How long it is now.
    pub available_m: f64,
    /// The node to change the length of.
    pub extend_node_id: String,
    /// The fix, in words, for a page that shows the refusal rather than acting
    /// on it.
    pub suggestion: String,
}

impl From<&FitFailure> for DistributeFit {
    fn from(fit: &FitFailure) -> Self {
        DistributeFit {
            needed_m: fit.needed_m,
            available_m: fit.available_m,
            extend_node_id: fit.extend_node.clone(),
            suggestion: format!(
                "needs {:.2} m, the face is {:.2} m — extend({}, to={:.2})",
                fit.needed_m, fit.available_m, fit.extend_node, fit.needed_m
            ),
        }
    }
}

/// What one distribution did.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub struct DistributeReport {
    /// `false` carries a [`Self::fit`] and no fixtures — a distribution is all
    /// of its fixtures or none of them.
    pub ok: bool,
    pub host_node_id: String,
    pub host_socket: String,
    pub fixtures: Vec<DistributedFixture>,
    pub fit: Option<DistributeFit>,
    pub warnings: Vec<String>,
    /// Open structural sockets left in the venue.
    pub dangling: Vec<String>,
    /// Subtrees the solve could not reach — the tray, and anything detached.
    pub unplaced: Vec<String>,
}

impl From<Report> for DistributeReport {
    fn from(report: Report) -> Self {
        DistributeReport {
            ok: report.ok,
            host_node_id: report.host_node,
            host_socket: report.host_socket,
            fixtures: report
                .fixtures
                .iter()
                .map(DistributedFixture::from)
                .collect(),
            fit: report.fit.as_ref().map(DistributeFit::from),
            warnings: report.warnings,
            dangling: report.dangling,
            unplaced: report.unplaced,
        }
    }
}
