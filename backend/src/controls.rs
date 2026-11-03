//! A venue's live controls: named values 0–1 that faders, pads, MIDI, OSC
//! and any other input set while the lights play. A clip graph reads one
//! through a `control` node; see `luma_patterns::ControlSource`.
//!
//! A control exists by being named. A clip that reads `drop` makes `drop` a
//! control of its venue, and it reads 0 until something sets it.
//!
//! Values are live process state, like the render engine's, so they live
//! here rather than in the database: every scene compiled for a venue reads
//! the one bank of that venue, and setting a value never recompiles.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::database::local::venue_access::AuthorizedVenue;

/// One venue's control values.
#[derive(Debug, Default)]
pub struct VenueControls {
    values: RwLock<BTreeMap<String, f64>>,
}

impl VenueControls {
    pub fn set(&self, name: &str, value: f64) {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.values
            .write()
            .expect("controls poisoned")
            .insert(name.to_owned(), value);
    }

    /// Every control set so far, with its value.
    pub fn values(&self) -> BTreeMap<String, f64> {
        self.values.read().expect("controls poisoned").clone()
    }
}

impl luma_patterns::ControlSource for VenueControls {
    fn value(&self, name: &str) -> f64 {
        self.values
            .read()
            .expect("controls poisoned")
            .get(name)
            .copied()
            .unwrap_or(0.0)
    }
}

fn banks() -> &'static Mutex<HashMap<String, Arc<VenueControls>>> {
    static BANKS: OnceLock<Mutex<HashMap<String, Arc<VenueControls>>>> = OnceLock::new();
    BANKS.get_or_init(Default::default)
}

/// The bank of `venue_id`, made on first use.
pub fn venue(venue_id: &str) -> Arc<VenueControls> {
    banks()
        .lock()
        .expect("controls poisoned")
        .entry(venue_id.to_owned())
        .or_default()
        .clone()
}

/// Forget every venue's values: another person signed in.
pub(crate) fn clear() {
    banks().lock().expect("controls poisoned").clear();
}

/// One control as the Perform screen lists it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    pub name: String,
    pub value: f64,
}

/// The venue's controls: every one a clip of the venue names, and every one
/// set, by name.
pub(crate) async fn list(access: &mut impl AuthorizedVenue) -> Result<Vec<Control>, String> {
    let venue_id = access.venue_id().to_owned();
    let graphs: Vec<String> = sqlx::query_scalar(
        "SELECT clip.graph_json FROM clips clip
         JOIN scores score ON score.id = clip.score_id
         WHERE score.venue_id = ?",
    )
    .bind(&venue_id)
    .fetch_all(access.connection())
    .await
    .map_err(|error| format!("Failed to read the venue's clips: {error}"))?;
    let mut names: BTreeSet<String> = graphs
        .iter()
        .filter_map(|json| luma_patterns::ClipGraph::from_json(json).ok())
        .flat_map(|graph| {
            graph
                .controls()
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();
    let values = venue(&venue_id).values();
    names.extend(values.keys().cloned());
    Ok(names
        .into_iter()
        .map(|name| Control {
            value: values.get(&name).copied().unwrap_or(0.0),
            name,
        })
        .collect())
}
