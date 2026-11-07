//! The last camera pose in each venue, kept on this device so a venue opens
//! where it was left.
//!
//! A session item in this device's local store (see [`crate::session_store`]),
//! beside each venue's mode and the last venue. Nothing here is synced: where
//! one operator was looking is not a fact about the venue.
//!
//! The pose is the camera's spherical form — target, radius and the two
//! angles — because that is what the orbit controller works in. Storing an eye
//! and a target and deriving the angles back is how a camera drifts.

use std::collections::BTreeMap;

use gpui::Context;
use luma_scene::Camera;
use serde::{Deserialize, Serialize};

use crate::Luma;

/// The session item the poses are stored under.
const KEY: &str = "camera-poses";

/// Every venue's last pose, as stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
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

/// Every venue's last pose, in the shared session store.
pub(crate) type SavedCameras = crate::session_store::Stored<Snapshot>;

/// Nothing saved yet: what the shell holds until the launch read lands.
pub(crate) fn empty() -> SavedCameras {
    SavedCameras::new(KEY)
}

/// Take in what the store held at launch.
pub(crate) fn read(stored: Option<&str>) -> SavedCameras {
    SavedCameras::read(KEY, stored)
}

impl Snapshot {
    /// `venue_id`'s last pose, if it has one.
    pub(crate) fn pose(&self, venue_id: &str) -> Option<Pose> {
        self.venues.get(venue_id).copied()
    }
}

impl Luma {
    /// The store's value, for [`read`].
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
        if self.saved_cameras.value().pose(&venue) == Some(pose) {
            return;
        }
        let mut snapshot = self.saved_cameras.value().clone();
        snapshot.venues.insert(venue, pose);
        self.store_session(|this| &mut this.saved_cameras, snapshot, cx);
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
        let stored = read(Some(&json));
        let restored = stored
            .value()
            .pose("club")
            .unwrap()
            .applied(Camera::default());
        assert_eq!(restored, camera);
        assert_eq!(stored.value().pose("elsewhere"), None);
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
}
