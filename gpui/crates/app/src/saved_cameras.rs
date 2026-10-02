//! The last camera pose in each venue, kept on this device so a venue opens
//! where it was left.
//!
//! A session item in this device's local store, beside the open tabs and the
//! last venue. Nothing here is synced: where one operator was looking is not a
//! fact about the venue.
//!
//! The pose is the camera's spherical form — target, radius and the two
//! angles — because that is what the orbit controller works in. Storing an eye
//! and a target and deriving the angles back is how a camera drifts.

use std::collections::BTreeMap;
use std::time::Duration;

use gpui::Context;
use luma_scene::Camera;
use serde::{Deserialize, Serialize};

use crate::Luma;

/// The session item the poses are stored under.
const KEY: &str = "camera-poses";

/// How long a change waits for the next one before it is written. An orbit
/// drag changes the pose every frame; only where it comes to rest is written.
const SAVE_WAIT: Duration = Duration::from_millis(500);

/// Every venue's last pose, as stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct Snapshot {
    venues: BTreeMap<String, Pose>,
}

/// Where the camera was: what it orbits, how far out, and from which angle.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pose {
    target: [f32; 3],
    radius: f32,
    azimuth: f32,
    polar: f32,
}

impl Pose {
    pub(crate) fn of(camera: &Camera) -> Self {
        Self {
            target: camera.target.to_array(),
            radius: camera.radius,
            azimuth: camera.azimuth,
            polar: camera.polar,
        }
    }

    /// `camera` moved to this pose. The lens stays the camera's own.
    pub(crate) fn applied(self, camera: Camera) -> Camera {
        Camera {
            target: self.target.into(),
            radius: self.radius,
            azimuth: self.azimuth,
            polar: self.polar,
            ..camera
        }
    }
}

#[derive(Default)]
pub(crate) struct SavedCameras {
    /// What the store holds, as last read or written. A frame whose pose
    /// matches it writes nothing.
    stored: Snapshot,
    /// Correlates a debounced write with the change that scheduled it.
    writes: u64,
}

impl SavedCameras {
    /// Take in what the store held at launch. A value that does not parse is
    /// treated as nothing saved: it is a convenience, not a document.
    pub(crate) fn read(stored: Option<&str>) -> Self {
        Self {
            stored: stored
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default(),
            writes: 0,
        }
    }

    /// `venue_id`'s last pose, if it has one.
    pub(crate) fn pose(&self, venue_id: &str) -> Option<Pose> {
        self.stored.venues.get(venue_id).copied()
    }
}

impl Luma {
    /// The store's value, for [`SavedCameras::read`].
    pub(crate) fn read_saved_cameras(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<String>, crate::LibraryError>> + use<>
    {
        self.library.get_session_item(KEY)
    }

    /// Write the stage's pose when it changed. Called every frame; an orbit is
    /// written once, after it comes to rest.
    pub(crate) fn save_camera(&mut self, cx: &mut Context<Self>) {
        let Some((venue, pose)) = self
            .visualizer
            .as_ref()
            .and_then(|stage| Some((stage.venue_id.clone(), stage.settled_pose()?)))
        else {
            return;
        };
        if self.saved_cameras.stored.venues.get(&venue) == Some(&pose) {
            return;
        }
        self.saved_cameras.stored.venues.insert(venue, pose);
        let Ok(json) = serde_json::to_string(&self.saved_cameras.stored) else {
            return;
        };
        self.saved_cameras.writes += 1;
        let generation = self.saved_cameras.writes;
        let wait = self.library.debounce(SAVE_WAIT);
        cx.spawn(async move |this, cx| {
            wait.await;
            let Ok(Some(write)) = this.update(cx, |this, _| {
                (this.saved_cameras.writes == generation)
                    .then(|| this.library.set_session_item(KEY, &json))
            }) else {
                return;
            };
            let _ = write.await;
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pose_round_trips_through_its_stored_form() {
        let camera = Camera {
            target: glam::Vec3::new(1.0, -2.0, 3.0),
            radius: 12.5,
            azimuth: 0.7,
            polar: 1.1,
            ..Camera::default()
        };
        let mut venues = BTreeMap::new();
        venues.insert("club".to_string(), Pose::of(&camera));
        let json = serde_json::to_string(&Snapshot { venues }).unwrap();
        let read = SavedCameras::read(Some(&json));
        let restored = read.pose("club").unwrap().applied(Camera::default());
        assert_eq!(restored, camera);
        assert_eq!(read.pose("elsewhere"), None);
    }

    #[test]
    fn the_lens_stays_the_cameras_own() {
        let lens = Camera {
            fov_y_deg: 35.0,
            znear: 0.25,
            ..Camera::default()
        };
        let moved = Pose::of(&Camera::default()).applied(lens);
        assert_eq!(moved.fov_y_deg, 35.0);
        assert_eq!(moved.znear, 0.25);
    }

    #[test]
    fn an_unreadable_value_is_nothing_saved() {
        let read = SavedCameras::read(Some("not json"));
        assert_eq!(read.stored, Snapshot::default());
    }
}
