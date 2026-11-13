//! A venue's laser rig (`docs/design/lasers.md`).

use crate::database::local::lasers;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::laser::Window;
use std::collections::BTreeMap;

/// The venue's projectors, posed and set up, with their limits, and its
/// window: what the visualizer solves each frame's beams with, as a DAC
/// would.
pub async fn get_laser_rig(
    services: &AppServices,
    venue_id: String,
) -> Result<crate::laser::LaserRig, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let rig = crate::laser::LaserRig::load(&mut access, &services.fixtures_root).await?;
    // As the patch is: the venue last read is the one output plays.
    if let Some(output) = services.output.as_deref() {
        output.update_lasers(rig.clone());
    }
    Ok(rig)
}

/// Open the venue to write its lasers.
async fn write<'a>(
    services: &'a AppServices,
    venue_id: &'a str,
) -> Result<VenueAccess<'a, Write>, CommandError> {
    Ok(VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(venue_id)).await?)
}

/// Commit a change to the venue's lasers and tell everything that draws or
/// plays them.
async fn commit(
    services: &AppServices,
    venue_id: &str,
    access: VenueAccess<'_, Write>,
) -> Result<(), CommandError> {
    crate::venue_graph::commit_graph(access).await?;
    if services.output.is_some() {
        get_laser_rig(services, venue_id.to_owned()).await?;
    }
    Ok(())
}

/// Set the window the venue's laser clips draw in, or with `None` go back
/// to the one fitted to its projectors.
pub async fn set_laser_window(
    services: &AppServices,
    venue_id: String,
    window: Option<Window>,
) -> Result<(), CommandError> {
    let mut access = write(services, &venue_id).await?;
    lasers::set_window(&mut access, window)
        .await
        .map_err(CommandError::Invalid)?;
    commit(services, &venue_id, access).await
}

/// Change some laser settings, by parameter name: one projector's, or with
/// no projector the ones the venue's projectors share.
pub async fn set_laser_settings(
    services: &AppServices,
    venue_id: String,
    projector_id: Option<String>,
    settings: BTreeMap<String, f64>,
) -> Result<(), CommandError> {
    let mut access = write(services, &venue_id).await?;
    lasers::set_settings(&mut access, projector_id.as_deref(), &settings)
        .await
        .map_err(CommandError::Invalid)?;
    commit(services, &venue_id, access).await
}

/// Give a projector a kind of its own, starting from the venue's, or have
/// it follow the venue's again.
pub async fn set_laser_own(
    services: &AppServices,
    venue_id: String,
    projector_id: String,
    own: bool,
) -> Result<(), CommandError> {
    let kind = if own {
        Some(get_laser_rig(services, venue_id.clone()).await?.shared)
    } else {
        None
    };
    let mut access = write(services, &venue_id).await?;
    lasers::set_own(&mut access, &projector_id, kind.as_ref())
        .await
        .map_err(CommandError::Invalid)?;
    commit(services, &venue_id, access).await
}

/// Set where a projector may send light, or with `None` lift its limit.
pub async fn set_laser_limit(
    services: &AppServices,
    venue_id: String,
    projector_id: String,
    points: Option<Vec<[f64; 2]>>,
) -> Result<(), CommandError> {
    let mut access = write(services, &venue_id).await?;
    lasers::set_limit(&mut access, &projector_id, points.as_deref())
        .await
        .map_err(CommandError::Invalid)?;
    commit(services, &venue_id, access).await
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::dispatch::{dispatch, AppServices};
    use crate::laser::{Key, Preset, Settings};
    use crate::models::fixtures::LASER_PROJECTOR_PATH;

    async fn run(services: &AppServices, command: &str, args: Value) -> Value {
        dispatch(services, command, &args)
            .await
            .unwrap_or_else(|error| panic!("{command}: {error}"))
    }

    /// A venue with two projectors on a truss; their ids.
    async fn rig() -> (tempfile::TempDir, AppServices, String, Vec<String>) {
        let directory = tempfile::tempdir().unwrap();
        let fixtures_root = crate::resources::source_fixtures_root().expect("the shipped bundle");
        let services = crate::test_support::signed_in(directory.path(), fixtures_root).await;
        let venue = run(
            &services,
            "create_venue",
            json!({ "name": "Club", "description": null }),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let deck = run(
            &services,
            "place_free",
            json!({
                "venueId": venue, "kind": "stage",
                "catalogRef": "stage_lab/stage_praticavel_2x1x1.glb", "label": null,
                "surfaceNodeId": null, "surfaceSocket": null, "mySocket": "bottom",
                "u": 0.0, "v": 0.0, "yaw": null, "trim": null,
            }),
        )
        .await["nodeId"]
            .as_str()
            .unwrap()
            .to_owned();
        let truss = run(
            &services,
            "attach",
            json!({
                "venueId": venue, "kind": "run", "catalogRef": "truss/straight", "label": null,
                "parentId": deck, "mySocket": "end_a", "theirSocket": "corner_fl",
                "yaw": null, "params": { "span": 4.0 },
            }),
        )
        .await["nodeId"]
            .as_str()
            .unwrap()
            .to_owned();
        let report = run(
            &services,
            "distribute",
            json!({
                "venueId": venue, "hostNodeId": truss, "hostSocket": "face_-y",
                "fixturePath": LASER_PROJECTOR_PATH, "modeName": "ILDA", "count": 2,
                "layout": { "kind": "even" }, "labelPrefix": null,
            }),
        )
        .await;
        assert_eq!(report["refusal"], json!(null), "{report}");
        let projectors = report["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fixture| {
                // A projector takes no DMX address.
                assert_eq!(fixture["address"], json!(null), "{fixture}");
                fixture["id"].as_str().unwrap().to_owned()
            })
            .collect();
        (directory, services, venue, projectors)
    }

    async fn laser_rig(services: &AppServices, venue: &str) -> crate::laser::LaserRig {
        serde_json::from_value(run(services, "get_laser_rig", json!({ "venueId": venue })).await)
            .unwrap()
    }

    /// A projector is a fixture of the rig and no part of the patch: it has
    /// no channels, so the next light's address is the first.
    #[tokio::test]
    async fn a_projector_is_in_the_rig_and_not_in_the_patch() {
        let (_directory, services, venue, projectors) = rig().await;
        let rig = laser_rig(&services, &venue).await;
        assert_eq!(rig.projectors.len(), 2);
        assert!(rig
            .projectors
            .iter()
            .all(|p| p.settings == Settings::default()));
        let patch = run(
            &services,
            "get_patched_fixtures",
            json!({ "venueId": venue }),
        )
        .await;
        assert!(patch
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["numChannels"] == json!(0)));
        // It cannot be given an address either.
        let refused = dispatch(
            &services,
            "set_fixture_address",
            &json!({ "venueId": venue, "id": projectors[0], "universe": 1, "address": 10 }),
        )
        .await;
        assert!(refused.is_err(), "{refused:?}");
    }

    /// A projector's settings are its own, kept inside their ranges, and a
    /// name that is no setting changes nothing.
    #[tokio::test]
    async fn settings_are_stored_per_projector() {
        let (_directory, services, venue, projectors) = rig().await;
        let preset = Settings::preset(Preset::Watts10);
        let mut settings: serde_json::Map<String, Value> =
            [Key::PowerRed, Key::PowerGreen, Key::PowerBlue]
                .into_iter()
                .map(|key| (key.param().to_owned(), json!(preset.get(key))))
                .collect();
        settings.insert(Key::Angle.param().into(), json!(40.0));
        settings.insert(Key::Cap.param().into(), json!(7.0));
        run(
            &services,
            "set_laser_settings",
            json!({ "venueId": venue, "projectorId": projectors[0], "settings": settings }),
        )
        .await;
        let rig = laser_rig(&services, &venue).await;
        let changed = &rig.projector(&projectors[0]).unwrap().settings;
        assert_eq!(changed.power(), preset.power());
        assert_eq!(changed.get(Key::Angle), 40.);
        assert_eq!(changed.get(Key::Cap), 1., "kept inside its range");
        assert_eq!(
            rig.projector(&projectors[1]).unwrap().settings,
            Settings::default()
        );

        let refused = dispatch(
            &services,
            "set_laser_settings",
            &json!({ "venueId": venue, "projectorId": projectors[0],
                     "settings": { "laser_angle": 20.0, "trim": 1.0 } }),
        )
        .await;
        assert!(refused.is_err());
        let rig = laser_rig(&services, &venue).await;
        assert_eq!(
            rig.projector(&projectors[0])
                .unwrap()
                .settings
                .get(Key::Angle),
            40.
        );
    }

    /// A venue's projectors are one kind until one is given its own: a
    /// change to the venue's reaches every projector that follows it, a
    /// projector's own picture stays its own, and one that stops following
    /// starts from what it had.
    #[tokio::test]
    async fn projectors_share_a_kind_until_one_has_its_own() {
        let (_directory, services, venue, projectors) = rig().await;
        let set = |projector: Value, settings: Value| json!({ "venueId": venue, "projectorId": projector, "settings": settings });
        run(
            &services,
            "set_laser_settings",
            set(json!(null), json!({ "laser_angle": 40.0 })),
        )
        .await;
        let rig = laser_rig(&services, &venue).await;
        assert_eq!(rig.shared.get(Key::Angle), 40.);
        assert!(rig
            .projectors
            .iter()
            .all(|p| !p.own && p.settings.get(Key::Angle) == 40.));

        // How one box hangs is that box's, and no venue's.
        run(
            &services,
            "set_laser_settings",
            set(json!(projectors[0]), json!({ "laser_flip_x": 1.0 })),
        )
        .await;
        assert!(dispatch(
            &services,
            "set_laser_settings",
            &set(json!(null), json!({ "laser_flip_x": 1.0 })),
        )
        .await
        .is_err());
        let rig = laser_rig(&services, &venue).await;
        assert!(rig
            .projector(&projectors[0])
            .unwrap()
            .settings
            .is_on(Key::FlipX));
        assert!(!rig.projector(&projectors[0]).unwrap().own);
        assert!(!rig
            .projector(&projectors[1])
            .unwrap()
            .settings
            .is_on(Key::FlipX));

        // Its own kind starts as the venue's and stops following it.
        let own = |own: bool| json!({ "venueId": venue, "projectorId": projectors[1], "own": own });
        run(&services, "set_laser_own", own(true)).await;
        run(
            &services,
            "set_laser_settings",
            set(json!(null), json!({ "laser_angle": 50.0 })),
        )
        .await;
        let rig = laser_rig(&services, &venue).await;
        assert!(rig.projector(&projectors[1]).unwrap().own);
        assert_eq!(
            rig.projector(&projectors[1])
                .unwrap()
                .settings
                .get(Key::Angle),
            40.
        );
        assert_eq!(
            rig.projector(&projectors[0])
                .unwrap()
                .settings
                .get(Key::Angle),
            50.
        );

        run(&services, "set_laser_own", own(false)).await;
        let rig = laser_rig(&services, &venue).await;
        assert!(!rig.projector(&projectors[1]).unwrap().own);
        assert_eq!(
            rig.projector(&projectors[1])
                .unwrap()
                .settings
                .get(Key::Angle),
            50.
        );
    }

    /// A venue draws in the window fitted to its projectors until it sets
    /// one, and again once it clears it; what is no window is refused.
    #[tokio::test]
    async fn a_venue_sets_and_clears_its_window() {
        let (_directory, services, venue, _) = rig().await;
        let fitted = laser_rig(&services, &venue).await;
        assert_eq!(fitted.window, None);

        let window = json!({ "turn": [-40.0, 40.0], "tilt": [10.0, 25.0] });
        run(
            &services,
            "set_laser_window",
            json!({ "venueId": venue, "window": window }),
        )
        .await;
        let set = laser_rig(&services, &venue).await;
        assert_eq!(serde_json::to_value(set.window()).unwrap(), window);

        let upside_down = json!({ "turn": [-40.0, 40.0], "tilt": [25.0, 10.0] });
        assert!(dispatch(
            &services,
            "set_laser_window",
            &json!({ "venueId": venue, "window": upside_down }),
        )
        .await
        .is_err());

        run(
            &services,
            "set_laser_window",
            json!({ "venueId": venue, "window": null }),
        )
        .await;
        assert_eq!(laser_rig(&services, &venue).await.window(), fitted.window());
    }

    /// A limit is one projector's, replaced when set again and lifted with
    /// none; what is no polygon in the scan is refused.
    #[tokio::test]
    async fn a_projector_has_one_limit() {
        let (_directory, services, venue, projectors) = rig().await;
        let limit = |points: Value| json!({ "venueId": venue, "projectorId": projectors[0], "points": points });
        let lower = json!([[-1.0, -1.0], [1.0, -1.0], [1.0, 0.0], [-1.0, 0.0]]);
        run(
            &services,
            "set_laser_limit",
            limit(json!([[-1.0, -1.0], [1.0, -1.0], [0.0, 1.0]])),
        )
        .await;
        run(&services, "set_laser_limit", limit(lower.clone())).await;
        let rig = laser_rig(&services, &venue).await;
        assert_eq!(
            serde_json::to_value(&rig.projector(&projectors[0]).unwrap().limit).unwrap(),
            lower
        );
        assert_eq!(rig.projector(&projectors[1]).unwrap().limit, None);

        for bad in [
            json!([[0.0, 0.0], [1.0, 1.0]]),
            json!([[0.0, 0.0], [2.0, 0.0], [0.0, 1.0]]),
        ] {
            assert!(dispatch(&services, "set_laser_limit", &limit(bad))
                .await
                .is_err());
        }
        run(&services, "set_laser_limit", limit(json!(null))).await;
        let rig = laser_rig(&services, &venue).await;
        assert_eq!(rig.projector(&projectors[0]).unwrap().limit, None);
    }
}
