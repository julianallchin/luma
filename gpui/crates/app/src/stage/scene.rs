//! The builder's shapes in the renderer's scene: ghosts of what the hand would
//! place, a run's measure, and the socket marks, written by [`install`].

use std::collections::{BTreeMap, HashMap};

use luma_lib::services::stage_ops::CopyStep;

use super::build::Build;
use super::hand::{self, Hand, Holding, Landed, Landing};

/// One body a held thing would put in the room, ready to be drawn.
pub(super) struct GhostBody {
    geometry: luma_render::scene_desc::Geometry,
    /// The definition key when the body is a light — the overlay draws the
    /// housing itself and keeps `geometry` as the not-yet-loaded fallback.
    fixture: Option<String>,
    pub(super) world: glam::DMat4,
    scale: f32,
}

impl Build {
    /// Every body the held thing would put in the room, at the pose it would
    /// put it.
    ///
    /// A ghost is a *preview*, so what it draws is what lands: a truss draws a
    /// truss, a fixture draws a housing, and a duplicated wing draws the whole
    /// wing. A single mark under the cursor is enough to know something is held
    /// and not enough to know what — which is the one question a ghost exists
    /// to answer, and the reason "no catalog entry" cannot mean "draw nothing".
    ///
    /// The copy's poses come from [`Self::copy_plan`] fed through the ordinary
    /// resolver, not from a transform composed here: flip is written in the
    /// rows, so a preview that reproduced it geometrically would be a second
    /// implementation of handedness that could disagree with the commit.
    pub(super) fn ghost_bodies(&self, what: &Holding, landed: &Landed) -> Vec<GhostBody> {
        use luma_render::scene_desc::Geometry;
        match what {
            Holding::Piece {
                catalog_ref,
                params,
                ..
            } => geometry_with(catalog_ref, params)
                .map(|geometry| GhostBody {
                    geometry,
                    fixture: None,
                    world: landed.world,
                    scale: 1.0,
                })
                .into_iter()
                .collect(),
            // A fixture's housing, in the shape the row preview already draws
            // one in ([`install`]'s stations): the ghost and the stations are
            // the same body seen before and after the count is set, and two
            // spellings of "a light goes here" would be one too many.
            Holding::Fixture { path, .. } => vec![GhostBody {
                geometry: Geometry::Procedural(luma_scene::catalog::default_params(
                    luma_scene::catalog::Family::Corner,
                )),
                fixture: Some(path.clone()),
                world: landed.world,
                scale: STATION_GHOST_SCALE,
            }],
            Holding::Duplicate { root, flip, .. } => self.copy_ghosts(root, *flip, landed),
        }
    }

    /// The duplicated subtree, solved where it would land.
    ///
    /// The plan is inserted into a *clone* of the graph under ids no venue can
    /// mint, and the whole thing is resolved. That is a solve per pointer
    /// sample while a copy is held — a depth-first walk over a rig the design
    /// bounds at ~500 nodes, against the alternative of a second, divergent
    /// notion of where the copy goes.
    fn copy_ghosts(&self, root: &str, flip: bool, landed: &Landed) -> Vec<GhostBody> {
        let Some(plan) = self.copy_plan(root, flip, &landed.how) else {
            return Vec::new();
        };
        let mut graph = self.graph.clone();
        let mut minted: HashMap<String, String> = HashMap::new();
        for (index, step) in plan.iter().enumerate() {
            let id = format!("{}{index}", hand::GHOST_NODE);
            let parent = if index == 0 {
                step.parent.clone()
            } else {
                match minted.get(&step.parent) {
                    Some(parent) => parent.clone(),
                    None => continue,
                }
            };
            let mut params = luma_scene::venue::Params::default();
            for (key, value) in &step.params {
                params.set(key.clone(), *value);
            }
            graph.insert_placed(
                luma_scene::venue::Node {
                    id: id.clone(),
                    kind: step.kind,
                    catalog_ref: step.catalog_ref.clone(),
                    label: step.label.clone(),
                    params,
                },
                luma_scene::venue::Edge {
                    parent,
                    my_socket: step.my_socket.clone(),
                    their_socket: step.their_socket.clone(),
                    roll: step.yaw,
                },
            );
            minted.insert(step.source.clone(), id);
        }
        let solved = luma_scene::venue::resolve(&graph, &self.sockets);
        minted
            .values()
            .filter_map(|id| {
                let pose = solved.pose(id)?;
                let geometry = pose
                    .catalog_ref
                    .as_deref()
                    .and_then(geometry_of)
                    .unwrap_or_else(|| {
                        luma_render::scene_desc::Geometry::Procedural(
                            luma_scene::catalog::default_params(
                                luma_scene::catalog::Family::Corner,
                            ),
                        )
                    });
                Some(GhostBody {
                    geometry,
                    fixture: None,
                    world: pose.world,
                    scale: if pose.catalog_ref.is_some() {
                        1.0
                    } else {
                        STATION_GHOST_SCALE
                    },
                })
            })
            .collect()
    }

    /// The `attach` list a duplicate of `root` would owe, landing on `how`.
    ///
    /// [`luma_lib::services::stage_ops::duplicate_plan`]'s answer, not a second
    /// one: the ghost previews exactly the rows the verb will write, so a
    /// flipped wing cannot draw one way and land another.
    fn copy_plan(&self, root: &str, flip: bool, how: &Landing) -> Option<Vec<CopyStep>> {
        // A duplicate is placed on a socket: every compatible open socket
        // lights up precisely so the copy lands on a joint, and a copy seated
        // free would be a second, different gesture.
        let Landing::Socket {
            parent,
            their_socket,
            ..
        } = how
        else {
            return None;
        };
        luma_lib::services::stage_ops::duplicate_plan(&self.graph, root, parent, their_socket, flip)
    }
}

/// How big a preview body is drawn, as a fraction of the catalog block it
/// borrows its shape from.
///
/// A stand-in: a library fixture has no mesh in this catalog, and loading one
/// per preview frame to answer "roughly here, roughly this big" would be a
/// download in a drag loop. The station marks on the element layer are the
/// precise claim; this is the mass that makes a row read as a row.
const STATION_GHOST_SCALE: f32 = 0.5;

/// Push the builder's shapes into the scene the renderer draws.
///
/// The eye's half of the same two facts the element layer carries: the ghost
/// and the measurement. Written straight onto `scene.editor` so the idle gate
/// sees them — `IdleKey` compares the whole editor, so a ghost that moved is a
/// frame the stage owes.
pub(crate) fn install(build: &Build, editor: &mut luma_render::scene_desc::Editor) {
    use luma_render::scene_desc::{Build as SceneBuild, Ghost, Measure, SocketMark};
    let mut out = SceneBuild::default();
    if let Some(held) = build.hand.held() {
        // A refused landing draws nothing: the hand still knows where the
        // cursor is, but a piece that cannot go there is not previewed there —
        // the ghost disappearing *is* the refusal.
        if let Some(landed) = held.landed.as_ref().filter(|l| l.refused.is_none()) {
            out.host = match &landed.how {
                Landing::Socket { parent, .. } => Some(parent.clone()),
                Landing::Free {
                    surface: Some((node, _)),
                    ..
                } => Some(node.clone()),
                Landing::Free { surface: None, .. } => None,
            };
            for body in build.ghost_bodies(&held.what, landed) {
                let (pos, rot) = luma_scene::coords::data_pose_of_d(body.world);
                out.ghosts.push(Ghost {
                    geometry: body.geometry,
                    fixture: body.fixture,
                    pos: pos.map(|v| v as f32),
                    rot: rot.map(|v| v as f32),
                    scale: body.scale,
                    refused: false,
                });
            }
        }
    }
    // Every body of a row being fitted, at the poses the band math just
    // answered with. A refused row previews nothing — there are no stations to
    // draw, which is itself the picture of "this will not fit".
    if let Some(row) = build.hand.configuring() {
        if let Ok(stations) = &row.preview {
            for station in stations {
                let (pos, rot) = luma_scene::coords::data_pose_of_d(*station);
                out.ghosts.push(Ghost {
                    geometry: luma_render::scene_desc::Geometry::Procedural(
                        luma_scene::catalog::default_params(luma_scene::catalog::Family::Corner),
                    ),
                    fixture: match &row.what {
                        Holding::Fixture { path, .. } => Some(path.clone()),
                        _ => None,
                    },
                    pos: pos.map(|v| v as f32),
                    rot: rot.map(|v| v as f32),
                    scale: STATION_GHOST_SCALE,
                    refused: false,
                });
            }
        }
    }
    if let Some(run) = build.hand.extending() {
        if let Some(from) = build.room.socket_world(&run.from_node, &run.from_socket) {
            let socket = build.room.socket(&run.from_node, &run.from_socket);
            let pose = build.room.pose(&run.from_node);
            if let (Some(socket), Some(pose)) = (socket, pose) {
                let refused = build.run_refused().is_some();
                let direction = pose.transform_vector3(socket.normal).normalize_or_zero();
                let to = from + direction * run.length_m;
                out.measure = Some(Measure {
                    from: point_of(from),
                    to: point_of(to),
                    refused,
                });
                // The run's own ghost, at the length being asked for. This is
                // the design's red case: a length past the measured gap is one
                // of the two hard errors, and the ghost is where it is said.
                if let Some((params, world)) = build.run_ghost() {
                    let (pos, rot) = luma_scene::coords::data_pose_of_d(world);
                    out.ghosts.push(Ghost {
                        geometry: luma_render::scene_desc::Geometry::Procedural(params),
                        fixture: None,
                        pos: pos.map(|v| v as f32),
                        rot: rot.map(|v| v as f32),
                        scale: 1.0,
                        refused,
                    });
                }
            }
        }
    }
    // The selected piece's own dimension: a run at rest wears the measure the
    // extend flow draws, along its span — the "[ 3.0 m ]" the picture owes a
    // selected stick without a sheet to print it in.
    if out.measure.is_none() {
        if let (Hand::Idle, Some(node)) = (&build.hand, build.selected.as_ref()) {
            if let (Some(from), Some(to)) = (
                build.room.socket_world(node, "end_a"),
                build.room.socket_world(node, "end_b"),
            ) {
                out.measure = Some(Measure {
                    from: point_of(from),
                    to: point_of(to),
                    refused: false,
                });
            }
        }
    }
    let held = build.held_sockets();
    let held_kind = build
        .hand
        .held()
        .map_or(luma_scene::venue::NodeKind::Fixture, |held| {
            held.what.kind()
        });
    out.sockets = build
        .room
        .open_sockets()
        .filter(|(node, socket)| build.socket_marked(node, &socket.name) && hand::can_host(socket))
        .filter_map(|(node, socket)| {
            let at = build.room.socket_world(node, &socket.name)?;
            let pose = build.room.pose(node)?;
            Some(SocketMark {
                pos: point_of(at),
                normal: point_of(pose.transform_vector3(socket.normal).normalize_or_zero()),
                state: if build.latched_at(node, &socket.name) {
                    luma_render::scene_desc::SocketMarkState::Latched
                } else if !held.is_empty() && hand::compatible(socket, &held, held_kind) {
                    luma_render::scene_desc::SocketMarkState::Compatible
                } else {
                    luma_render::scene_desc::SocketMarkState::Open
                },
            })
        })
        .collect();
    editor.build = out;
}

/// A socket-layer point as the data-space triple `scene_desc` carries.
fn point_of(at: glam::DVec3) -> [f32; 3] {
    let data = luma_scene::coords::data_pose_of_d(glam::DMat4::from_translation(at)).0;
    data.map(|v| v as f32)
}

/// The geometry a catalog entry draws as, at the palette's own defaults.
fn geometry_of(catalog_ref: &str) -> Option<luma_render::scene_desc::Geometry> {
    use luma_render::scene_desc::Geometry;
    match luma_scene::catalog::piece(catalog_ref)?.geometry {
        luma_scene::catalog::Geometry::Mesh { path } => Some(Geometry::mesh(path)),
        luma_scene::catalog::Geometry::Procedural(family) => Some(Geometry::Procedural(
            luma_scene::catalog::default_params(family),
        )),
        luma_scene::catalog::Geometry::Assembly(_) => {
            Some(Geometry::Assembly(catalog_ref.to_string()))
        }
    }
}

/// The geometry a *held* piece draws as, at its own parameters — a truss whose
/// span was scrubbed ghosts at that span, not at the palette default.
fn geometry_with(
    catalog_ref: &str,
    params: &BTreeMap<String, f64>,
) -> Option<luma_render::scene_desc::Geometry> {
    use luma_render::scene_desc::Geometry;
    match luma_scene::catalog::piece(catalog_ref)?.geometry {
        luma_scene::catalog::Geometry::Mesh { path } => Some(Geometry::mesh(path)),
        luma_scene::catalog::Geometry::Procedural(family) => Some(Geometry::Procedural(
            luma_scene::catalog::node_params(family, &params_of(params)),
        )),
        // An assembly has no parameters to hold at, so the palette default and
        // the held shape are the same shape.
        luma_scene::catalog::Geometry::Assembly(_) => {
            Some(Geometry::Assembly(catalog_ref.to_string()))
        }
    }
}

/// A held piece's parameter map in the shape the generator reads.
pub(super) fn params_of(params: &BTreeMap<String, f64>) -> luma_scene::venue::Params {
    let mut out = luma_scene::venue::Params::default();
    for (key, value) in params {
        out.set(key.clone(), *value);
    }
    out
}

#[cfg(test)]
mod tests {
    use luma_scene::venue::{Node, NodeKind, Params, VenueGraph};

    use super::super::build::tests::{build_of, edge, piece};
    use super::{CopyStep, Landing};

    /// What one row of the plan says, with the minted id dropped: a duplicate
    /// mints new ids by construction, so ids are the one thing two builds of
    /// the same wing cannot agree on.
    fn row(step: &CopyStep, parent: &str) -> (String, String, String, String, i64, i64) {
        #[allow(clippy::cast_possible_truncation)]
        let round = |value: f64| (value * 1e6).round() as i64;
        (
            parent.to_string(),
            step.my_socket.clone(),
            step.their_socket.clone(),
            step.catalog_ref.clone().unwrap_or_default(),
            round(step.yaw),
            round(step.params.get("u").copied().unwrap_or_default()),
        )
    }

    /// Duplicate + flip of an asymmetric wing writes the same placements
    /// as building the opposite wing by hand.
    ///
    /// The wing is asymmetric on purpose — it bolts to its parent by a *sided*
    /// face, which is the case that a flip mirroring only the host's half of
    /// each joint got wrong: the copy met its parent on the same socket the
    /// original did, so the mirrored wing was bolted on inside-out.
    #[test]
    fn a_flipped_duplicate_writes_the_hand_built_opposite_wings_rows() {
        let root = Node {
            id: "venue".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        };
        let mut graph = VenueGraph::new(root);
        // The left wing: a stick on the downstage-left corner, an arm bolted to
        // its left face, a light clamped under the arm.
        graph.insert_placed(
            piece("wing_l", 0.4),
            edge("venue", "end_a", "corner_fl", 0.3),
        );
        graph.insert_placed(
            piece("arm_l", 0.25),
            edge("wing_l", "face_left", "face_right", 0.2),
        );
        graph.insert_placed(
            piece("light_l", -0.1),
            edge("arm_l", "clamp", "face_-y", 0.0),
        );
        // The same wing built by hand on the other side.
        graph.insert_placed(
            piece("wing_r", -0.4),
            edge("venue", "end_a", "corner_fr", -0.3),
        );
        graph.insert_placed(
            piece("arm_r", -0.25),
            edge("wing_r", "face_right", "face_left", -0.2),
        );
        graph.insert_placed(
            piece("light_r", 0.1),
            edge("arm_r", "clamp", "face_-y", 0.0),
        );

        let build = build_of(graph);
        let landing = Landing::Socket {
            parent: "venue".into(),
            my_socket: "end_a".into(),
            their_socket: "corner_fr".into(),
            yaw: -0.3,
        };
        let plan = build
            .copy_plan("wing_l", true, &landing)
            .expect("a socket landing plans a copy");

        // The plan, with each step's parent named by the *source* it copies —
        // a source id is a tree position, and tree position is the only thing
        // two builds of the same wing can be compared by.
        let copied: Vec<_> = plan.iter().map(|step| row(step, &step.parent)).collect();

        // The hand-built wing's rows, read back off the graph the same way.
        let hand: Vec<_> = ["wing_r", "arm_r", "light_r"]
            .into_iter()
            .map(|id| {
                let node = build.graph.node(id).expect("the hand-built node");
                let edge = build.graph.edge(id).expect("its edge");
                #[allow(clippy::cast_possible_truncation)]
                let round = |value: f64| (value * 1e6).round() as i64;
                (
                    // `wing_r`'s parent is the venue; the others' is the source
                    // id of the copy's corresponding parent, which is what the
                    // loop above spells for the copy.
                    match id {
                        "wing_r" => "venue",
                        "arm_r" => "wing_l",
                        _ => "arm_l",
                    }
                    .to_string(),
                    edge.my_socket.clone(),
                    edge.their_socket.clone(),
                    node.catalog_ref.clone().unwrap_or_default(),
                    round(edge.roll),
                    round(node.params.get("u", 0.0)),
                )
            })
            .collect();
        assert_eq!(copied, hand, "the flipped copy is not the opposite wing");
    }
}
