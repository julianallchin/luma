use serde::{Deserialize, Serialize};

/// A venue's named set of songs. It holds links, not songs: a song's scores
/// belong to the song (track + venue), so a song in two folders shows the same
/// scores in both, and deleting a folder deletes only its links.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub venue_id: String,
    pub name: String,
    /// The songs it holds, by track id.
    pub track_ids: Vec<String>,
}
