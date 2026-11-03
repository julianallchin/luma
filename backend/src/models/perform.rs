//! Wire types for live follow: which DJ source to follow, and what the
//! session following it currently knows.

use serde::{Deserialize, Serialize};

/// A DJ source live follow can listen to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PerformSource {
    /// Rekordbox, through rkbx_link's OSC output. `address` is where to
    /// listen, rkbx_link's `osc.destination`; `127.0.0.1:4460` when absent.
    RkbxLink {
        #[serde(default)]
        address: Option<String>,
    },
    /// Pioneer CDJs. `device_num` is the virtual player number Luma claims,
    /// which must not collide with a real player on the network.
    ProDjLink { device_num: u8 },
    /// Denon / Engine DJ hardware.
    StageLinq,
}

/// Everything live follow knows, as of the instant it was read. Positions are
/// extrapolated to that instant.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformState {
    /// The source being followed, or `None` when live follow is off.
    pub source: Option<PerformSource>,
    pub venue_id: Option<String>,
    /// Whether the source is up: listening or connected.
    pub connected: bool,
    /// What the source is doing, for a status line ("listening on …").
    pub detail: String,
    /// Milliseconds since the source last sent anything.
    pub last_heard_ms: Option<u64>,
    /// Why following may not work, when something is wrong.
    pub problem: Option<String>,
    /// Shifts every deck's lights earlier (positive) or later, to cover the
    /// source's and the DJ software's output latency.
    pub latency_ms: f32,
    pub decks: Vec<PerformDeck>,
}

impl PerformState {
    pub(crate) fn off(latency_ms: f32) -> Self {
        Self {
            source: None,
            venue_id: None,
            connected: false,
            detail: String::new(),
            last_heard_ms: None,
            problem: None,
            latency_ms,
            decks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformDeck {
    /// Numbered from 1, as players and mixers are.
    pub id: u8,
    pub title: String,
    pub artist: String,
    /// The deck's current (pitched) tempo.
    pub bpm: f64,
    /// The playhead, in track seconds.
    pub position: f64,
    pub playing: bool,
    pub track: DeckMatch,
}

/// How a deck's track resolved against the library.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DeckMatch {
    /// The source has not said what is loaded.
    Unknown,
    Matching,
    /// `has_score` is false when the track has no score in this venue, so the
    /// deck drives nothing. `manual` marks a track picked by hand.
    #[serde(rename_all = "camelCase")]
    Matched {
        track_id: String,
        title: String,
        artist: String,
        has_score: bool,
        manual: bool,
    },
    Unmatched,
    /// Matching or loading the scene failed.
    Failed {
        message: String,
    },
}
