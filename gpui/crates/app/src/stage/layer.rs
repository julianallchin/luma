//! The element layer over the viewport: socket beads, ghost and station
//! marks, the rotate pair and the run's controls, in window pixels.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, Pixels, Point};
use luma_scene::coords;
use luma_ui::float;
use luma_ui::icons::IconName;
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::Luma;

use super::build::{Build, Freedom};
use super::hand::{self, Hand, Holding};

/// Air between a floating control and the corner of the room it sits in.
///
/// Measured from the *picture*, not from the tab: the venue's own header row
/// sits above the room, and chrome that floated over it would cover the one
/// line naming what is being built.
const INSET: f32 = 12.0;

/// Half the rotate pair's width, for centring it on the joint.
const ROTATE_PAIR_HALF: f32 = 28.0;
/// How far below the joint the rotate pair hangs, in pixels — clear of the
/// bead drawn on the same socket.
const ROTATE_PAIR_GAP: f32 = 14.0;

/// One thing the hand is doing, projected onto the picture.
pub(crate) struct Mark {
    pub(crate) at: Point<Pixels>,
    pub(crate) label: String,
    pub(crate) refused: bool,
}

/// Every placement the hand is currently proposing, in window pixels: the
/// ghost under the cursor, or one per body of a row being fitted.
///
/// The element-layer twin of the ghosts [`install`] pushes at the renderer —
/// same poses, same count, from the same state — because a preview drawn only
/// in pixels has no evidence under a headless run, and the spec asks that a
/// value change move these *before* Apply.
pub(crate) fn marks(build: &Build, camera: &luma_scene::Camera, size: (f32, f32)) -> Vec<Mark> {
    let mut out = Vec::new();
    if let Some(held) = build.hand.held() {
        if let Some(landed) = held.landed.as_ref().filter(|l| l.refused.is_none()) {
            for body in build.ghost_bodies(&held.what, landed) {
                if let Some(at) = project(camera, size, body.world.w_axis.truncate()) {
                    out.push(Mark {
                        at,
                        label: format!("Ghost {}", held.what.label()),
                        refused: false,
                    });
                }
            }
        }
    }
    if let Some(row) = build.hand.configuring() {
        if let Ok(stations) = &row.preview {
            for (index, station) in stations.iter().enumerate() {
                if let Some(at) = project(camera, size, station.w_axis.truncate()) {
                    out.push(Mark {
                        at,
                        label: format!("Station {} {}", index + 1, row.what.label()),
                        refused: false,
                    });
                }
            }
        }
    }
    out
}

/// Where the run's own controls sit, and the gap the ray found there.
///
/// `Some` for the whole of [`Hand::Extending`], because a run with nothing in
/// front of it is still a run being built: it gets its length box and its
/// commit, and what a ray hitting nothing takes away is only the *measurement*.
/// The design's fourth extend case — "ray hits nothing → ghost at
/// [`hand::STUB_LENGTH_M`], type a length" — is unreachable if the card is
/// gated on there being a gap to report.
fn run_card(build: &Build, camera: &luma_scene::Camera, size: (f32, f32)) -> Option<RunCard> {
    let run = build.hand.extending()?;
    let from = build.room.socket_world(&run.from_node, &run.from_socket)?;
    build.room.socket(&run.from_node, &run.from_socket)?;
    // The socket the run comes *out* of, not the middle of the line.
    //
    // A dimension belongs at the middle, and that is where this sat until the
    // controls joined it: the midpoint is a function of the length, so dragging
    // the length box slid the box out from under the pointer that was dragging
    // it. The socket does not move while a run grows, which is the whole
    // requirement for anchoring a control to it.
    let at = project(camera, size, from)?;
    Some(RunCard {
        at,
        measured: run.measurement().zip(
            run.reach
                .as_ref()
                .map(|reach| hand::feet_and_inches(reach.gap_m)),
        ),
    })
}

/// Where the run's controls hang, and the gap they hang beside.
struct RunCard {
    at: Point<Pixels>,
    /// The gap, in metres and in feet, when the ray found one to report.
    measured: Option<(String, String)>,
}

/// A socket-layer point in window pixels, or `None` behind the camera or well
/// off screen. The projection [`beads`] does, shared so a station and a socket
/// cannot disagree about where the same metre is.
fn project(
    camera: &luma_scene::Camera,
    size: (f32, f32),
    at: glam::DVec3,
) -> Option<Point<Pixels>> {
    let (width, height) = size;
    if width <= 1.0 || height <= 1.0 {
        return None;
    }
    let world = coords::world_from_three(at.as_vec3());
    let forward = (camera.target - camera.position()).normalize_or_zero();
    if (world - camera.position()).dot(forward) <= 0.0 {
        return None;
    }
    let ndc = camera.project(world, width / height);
    if ndc.x.abs() > 1.2 || ndc.y.abs() > 1.2 {
        return None;
    }
    Some(Point::new(
        px((ndc.x * 0.5 + 0.5) * width),
        px((1.0 - (ndc.y * 0.5 + 0.5)) * height),
    ))
}

/// The widest a run may be scrubbed to, in metres. A bound rather than a
/// limit: a scrub maps a box to a range, so the range is the control's scale,
/// and a truss longer than this is asked for by number rather than swept to.
pub(super) const MAX_RUN_M: f64 = 12.0;

/// How wide the run's length box is.
pub(super) const RUN_SCRUB_W: f32 = 84.0;

/// How wide the run's whole control strip is: the measurement with its
/// imperial small print, the length box and the commit, inside the card's own
/// padding.
///
/// Fixed rather than fitted, because the strip has to be *placed* before it
/// could be measured — see the clamp in [`build_layer`] — and a card that had
/// to be laid out before it could be kept reachable is a card that cannot be.
/// Its three children are a fixed set, so this is a fact about the strip.
const RUN_CARD_W: f32 = 360.0;

/// The hit box a station mark carries, and the dot inside it.
const MARK_HALF: f32 = 9.0;
const STATION_DOT: f32 = 6.0;

/// A socket nobody is aiming at: small enough to read as a mark on the room
/// rather than as a control laid over it.
const BEAD_QUIET: f32 = 5.0;

/// A socket that would take what is in the hand, or has taken it.
const BEAD_LIVE: f32 = 7.0;

/// Where a socket falls on the pane, and how far along the view it is.
///
/// The one projection the builder has: the marks are drawn through it and the
/// snap chooses through it, so a pointer and a mark cannot disagree about
/// where a joint is. `None` for a socket behind the eye, which is a socket no
/// gesture can mean.
pub(crate) fn project_socket(
    camera: &luma_scene::Camera,
    size: (f32, f32),
    at: glam::DVec3,
) -> Option<(Point<Pixels>, f64)> {
    let (width, height) = size;
    if width <= 1.0 || height <= 1.0 {
        return None;
    }
    let world = coords::world_from_three(at.as_vec3());
    let forward = (camera.target - camera.position()).normalize_or_zero();
    let depth = (world - camera.position()).dot(forward);
    if depth <= 0.0 {
        return None;
    }
    let ndc = camera.project(world, width / height);
    Some((
        Point::new(
            px((ndc.x * 0.5 + 0.5) * width),
            px((1.0 - (ndc.y * 0.5 + 0.5)) * height),
        ),
        f64::from(depth),
    ))
}

/// One socket, projected.
pub(crate) struct Bead {
    pub(crate) node: String,
    pub(crate) socket: String,
    pub(crate) label: String,
    pub(crate) at: Point<Pixels>,
    pub(crate) state: luma_render::scene_desc::SocketMarkState,
}

/// Every socket the pointer may name, at its own projected pixel.
///
/// **Not a second pick system.** Nothing here nudges a mark off its socket and
/// nothing here aims by name: the pixel a mark sits on is the socket's own
/// projection, and the snap chooses through the very same
/// [`project_socket`] — so pointing at a mark and pointing at the pixel under
/// it are the same gesture, and a script that finds a socket by name reaches
/// it by pointing like a hand does.
///
/// While a piece is in the air these are *positions only* — see
/// [`build_layer`] for why nothing is painted on them.
pub(crate) fn beads(build: &Build, camera: &luma_scene::Camera, size: (f32, f32)) -> Vec<Bead> {
    use luma_render::scene_desc::SocketMarkState;
    let held = build.held_sockets();
    let held_kind = build
        .hand
        .held()
        .map_or(luma_scene::venue::NodeKind::Fixture, |held| {
            held.what.kind()
        });
    let latched = build
        .hand
        .held()
        .and_then(|held| held.latched.as_ref())
        .cloned();
    let mut out = Vec::new();
    for (node, socket) in build.room.open_sockets() {
        if !build.socket_shown(node) || !hand::can_host(socket) {
            continue;
        }
        let Some(world) = build.room.socket_world(node, &socket.name) else {
            continue;
        };
        let Some((at, _)) = project_socket(camera, size, world) else {
            continue;
        };
        // A tenth of a pane clear of it: a mark whose socket is off screen is
        // a claim about a joint nobody can point at.
        let margin = (px(size.0 * 0.1), px(size.1 * 0.1));
        if at.x < -margin.0
            || at.y < -margin.1
            || at.x > px(size.0) + margin.0
            || at.y > px(size.1) + margin.1
        {
            continue;
        }
        let state = if latched
            .as_ref()
            .is_some_and(|m| m.host_id.as_deref() == Some(node) && m.host_socket == socket.name)
        {
            SocketMarkState::Latched
        } else if !held.is_empty() && hand::compatible(socket, &held, held_kind) {
            SocketMarkState::Compatible
        } else {
            SocketMarkState::Open
        };
        out.push(Bead {
            node: node.to_string(),
            socket: socket.name.clone(),
            label: format!("Socket {} {}", build.label_of(node), socket.name),
            at,
            state,
        });
    }
    out
}

/// The clickable half of the builder, laid over the viewport.
///
/// It mounts whether or not the renderer is running, because the claims it
/// carries — which sockets are open, which one the ghost is stuck to — are
/// facts about the room and not about the picture, and a builder whose only
/// evidence was pixels would have none at all under the headless harness.
pub(crate) fn build_layer(
    build: &Build,
    camera: &luma_scene::Camera,
    origin: Point<Pixels>,
    size: (f32, f32),
    wired: bool,
    app: &Entity<Luma>,
) -> AnyElement {
    let _ = origin;
    use luma_render::scene_desc::SocketMarkState;
    let mut layer = div().absolute().inset_0();
    // A held piece still *claims* the room — this card is the harness's
    // evidence for what the hand is doing — but with a live viewport it does
    // not take the pointer: the viewport's own listener routes a click to a
    // placement and a drag to the camera, which is what makes an orbit
    // available with a ghost in the air. With no viewport (`!wired`, the
    // headless state) there is no listener, so the card carries the handlers
    // itself and does not occlude — there is no camera gesture to protect.
    if build.hand.owns_pointer() {
        let mut surface = div().id("stage-drop-surface").absolute().inset_0();
        if !wired && build.hand.aims_with_pointer() {
            let aim = app.clone();
            let drop = app.clone();
            surface = surface
                .on_mouse_move(move |event, _, cx| {
                    let at = event.position;
                    aim.update(cx, |this, cx| this.stage_aim_from_pointer(at, cx));
                })
                .on_click(move |event, _, cx| {
                    let at = event.position();
                    drop.update(cx, |this, cx| this.stage_click_room(at, cx));
                });
        }
        layer = layer.child(surface.agent_node(Role::Card, "Stage drop surface"));
    } else if !wired && matches!(build.hand, Hand::Idle) {
        // The headless twin of the viewport's click-to-select: with no canvas
        // there is no listener, so the room at rest is a surface a press can
        // pick a piece off (`Room::pick`). Behind the beads, like the canvas.
        let pick = app.clone();
        layer = layer.child(
            div()
                .id("stage-room-surface")
                .absolute()
                .inset_0()
                .on_click(move |event, _, cx| {
                    let at = event.position();
                    pick.update(cx, |this, cx| this.stage_pick_at(at, cx));
                })
                .agent_node(Role::Card, "Stage room"),
        );
    }
    // Socket marks first, then everything that sits over them. gpui hit-tests
    // in paint order, so a control added before a mark is a control the mark
    // swallows — which is exactly what the run's length box did from under one.
    //
    // **While a piece is in the air, nothing is painted on them.** A field of
    // dots over every socket in the room was not an affordance but a second,
    // worse pick system: each dot *hovered* the ghost onto its own joint by
    // name, so the ghost latched a corner the operator was not pointing at and
    // the hysteresis then welded it there. The ghost is already the answer to
    // "where will this go", and the one mark left in the air is the joint it
    // is stuck to, drawn in the picture at its own depth (`SocketMark`). What
    // survives here is the socket's *position* — the claim the harness reads,
    // the anchor a mark at rest hangs on, and a press that means nothing more
    // than a press on the room at that pixel.
    let placing = matches!(build.hand, Hand::Placing(_));
    let socket_marks = beads(build, camera, size);
    // How far right the selected piece's own marks reach, so a caption beside
    // it does not sit on top of them. A short piece's ends are within a card's
    // width of its centre, and a card is `occlude` — it swallowed the very
    // right-press that begins a run.
    for bead in &socket_marks {
        let (node, socket) = (bead.node.clone(), bead.socket.clone());
        let (menu, menu_node, menu_socket) = (app.clone(), node.clone(), socket.clone());
        let label = bead.label.clone();
        let drop = app.clone();
        let mut mark = div()
            .id(gpui::SharedString::from(format!("bead:{node}:{socket}")))
            .absolute()
            .left(bead.at.x - px(hand::SOCKET_MARK_PICK_PX))
            .top(bead.at.y - px(hand::SOCKET_MARK_PICK_PX))
            .w(px(hand::SOCKET_MARK_PICK_PX * 2.0))
            .h(px(hand::SOCKET_MARK_PICK_PX * 2.0))
            .flex()
            .items_center()
            .justify_center()
            // A press on a mark is a press on the room at that pixel and
            // nothing more: it aims where the pointer is, exactly as a press
            // on bare floor does, and the screen-space search is what turns
            // that pixel into this joint. Aiming *by name* is what this used
            // to do, and it is what latched a corner nobody was pointing at.
            .on_click(move |event, _, cx| {
                let at = event.position();
                drop.update(cx, |this, cx| this.stage_click_room(at, cx));
            })
            // The joint's own menu, which is where a run is begun.
            .on_mouse_down(gpui::MouseButton::Right, move |event, _, cx| {
                cx.stop_propagation();
                let (node, socket, at) = (menu_node.clone(), menu_socket.clone(), event.position);
                menu.update(cx, |this, cx| {
                    if let Some(build) = this.build_mut() {
                        build.select(Some(node.clone()));
                    }
                    this.stage_open_menu(at, node, Some(socket), cx);
                });
            });
        if !placing {
            // Quiet unless it is a candidate. An open socket is a fact about
            // the room; a compatible one is an answer to what is in the hand.
            let (dot, colour, ring) = match bead.state {
                SocketMarkState::Open => (BEAD_QUIET, ladder::foreground_alpha(0.34), 0.0),
                SocketMarkState::Compatible => (BEAD_LIVE, ladder::accent().into(), 0.0),
                SocketMarkState::Latched => (BEAD_LIVE, ladder::foreground().into(), 3.0),
            };
            mark = mark
                // Not `occlude`: a mark owns the press, but the wheel is the
                // camera's everywhere.
                .block_mouse_except_scroll()
                .child(
                    // No border: a hairline around a 5px dot over a dark room
                    // is most of the dot. A halo instead — light *around* the
                    // mark rather than a line closing it in.
                    div().w(px(dot)).h(px(dot)).rounded_full().bg(colour).when(
                        ring > 0.0,
                        |mark| {
                            mark.shadow(vec![gpui::BoxShadow {
                                color: ladder::foreground_alpha(0.35),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(ring),
                                spread_radius: px(ring * 0.5),
                                inset: false,
                            }])
                        },
                    ),
                );
        }
        layer = layer.child(mark.agent_node(Role::Button, label));
    }
    // Keep the rotation gesture at its physical pivot.
    if matches!(build.hand, Hand::Idle) {
        if let Some(selected) = build.selected_view() {
            // The joint's turn, at the joint. A rolled piece rotates about
            // the socket it is mated by, so the two step buttons sit on that
            // socket — a control drawn anywhere else claims a pivot the
            // solver does not use. The step is the joint's own: a quarter at
            // a bolt circle, five degrees at a rail post.
            if selected.freedom == Freedom::Roll {
                if let Some(at) = selected
                    .joint
                    .and_then(|joint| project(camera, size, joint))
                {
                    let mut pair = div()
                        .absolute()
                        .left(px(
                            (f32::from(at.x) - ROTATE_PAIR_HALF).clamp(INSET, size.0 - INSET)
                        ))
                        .top(px(
                            (f32::from(at.y) + ROTATE_PAIR_GAP).clamp(INSET, size.1 - INSET)
                        ))
                        .flex()
                        .flex_row()
                        .gap(px(6.0))
                        .occlude();
                    for (icon, direction) in
                        [(IconName::RotateLeft, 1.0), (IconName::RotateRight, -1.0)]
                    {
                        let app = app.clone();
                        let node = selected.node.clone();
                        let turned = selected.turn + direction * selected.roll_step;
                        pair = pair.child(
                            float::key_cap_pressable(float::key_cap())
                                .child(gpui_component::Icon::new(icon).size(px(14.)))
                                .id(gpui::ElementId::Name(
                                    format!("stage-rotate-{direction}").into(),
                                ))
                                .on_click(move |_, _, cx| {
                                    let node = node.clone();
                                    app.update(cx, |this, cx| {
                                        this.stage_set_param(&node, "yaw", turned.to_radians(), cx);
                                    });
                                })
                                .agent_node(
                                    Role::Button,
                                    format!(
                                        "Rotate {}",
                                        if direction > 0.0 { "ccw" } else { "cw" }
                                    ),
                                ),
                        );
                    }
                    layer = layer.child(pair);
                }
            }
        }
    }
    // What the hand is doing, where the hand is doing it. These are the
    // builder's evidence — for an eye because they are drawn over the room,
    // and for a driver because they are nodes. Neither reads a caption.
    layer = layer.children(marks(build, camera, size).into_iter().map(|mark| {
        div()
            .absolute()
            .left(mark.at.x - px(MARK_HALF))
            .top(mark.at.y - px(MARK_HALF))
            .w(px(MARK_HALF * 2.0))
            .h(px(MARK_HALF * 2.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(STATION_DOT))
                    .h(px(STATION_DOT))
                    .rounded_full()
                    .bg(if mark.refused {
                        ladder::danger()
                    } else {
                        ladder::accent()
                    }),
            )
            .agent_node(Role::Text, mark.label)
    }));
    // The run's own controls, on the line the renderer draws. A length is a
    // property of *this* measurement, so the number and the press that commits
    // it ride the thing they are about rather than a bar somewhere else — which
    // is the same argument the configure popover makes about a face.
    if let Some(RunCard { at, measured }) = run_card(build, camera, size) {
        // Clear of the inspector, which is painted over this layer: a run out
        // of a socket on the house-right side of the room put its commit
        // *under* the sheet, where the pointer could not reach it and nothing
        // in the tree said so — the button reports its bounds and its
        // enablement either way. Anchored to the socket until that would hide
        // it, and pushed left exactly as far as it must be.
        let reachable = size.0 - INSET;
        let left = px(f32::from(at.x).min(reachable - RUN_CARD_W).max(INSET));
        let refused = build.run_refused().is_some();
        let length = build.hand.extending().map_or(0.0, |run| run.length_m);
        let (set, commit) = (app.clone(), app.clone());
        layer = layer.child(
            div().absolute().left(left).top(at.y).occlude().child(
                float::popover_card()
                    .w(px(RUN_CARD_W))
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .p(px(4.0))
                    // The measurement, when the ray found one to make. Nothing
                    // stands in for it when it did not: a readout echoing the
                    // length back as a "gap" would invent a measurement out of
                    // the number being typed.
                    .children(measured.map(|(metres, feet)| {
                        div()
                            .pl(px(6.0))
                            .flex()
                            .items_baseline()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .font_family(luma_ui::fonts::MONO)
                                    .text_size(px(11.0))
                                    .text_color(ladder::foreground())
                                    .child(metres.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(ladder::foreground_alpha(0.45))
                                    .child(feet.clone()),
                            )
                            .agent_node(Role::Text, format!("{metres} · {feet}"))
                    }))
                    .child(float::scrub(
                        "stage-length",
                        length,
                        hand::LENGTH_STEP_M,
                        MAX_RUN_M,
                        hand::LENGTH_STEP_M,
                        RUN_SCRUB_W,
                        move |metres, _, cx| {
                            set.update(cx, |this, cx| this.stage_set_length(metres, cx));
                        },
                    ))
                    .child(
                        float::btn_primary("Place run")
                            .id("stage-place-run")
                            .when(refused, |b| b.opacity(float::INERT_OPACITY))
                            .on_click(move |_, _, cx| {
                                commit.update(cx, |this, cx| this.stage_commit_run(cx));
                            })
                            .agent_node(Role::Button, "Place run")
                            .agent_disabled(refused),
                    )
                    .map(float::frosted_card),
            ),
        );
    }
    // A held truss's one parameter, editable in the hand: the ghost is the
    // preview and this is its dial. Top-centre, clear of the add button and
    // the room the ghost is over.
    if let Some(hand::Held {
        what: Holding::Piece {
            catalog_ref,
            params,
            ..
        },
        ..
    }) = build.hand.held()
    {
        let truss = matches!(
            luma_scene::catalog::piece(catalog_ref).map(|piece| piece.geometry),
            Some(luma_scene::catalog::Geometry::Procedural(
                luma_scene::catalog::Family::Truss
            ))
        );
        if truss {
            let span = params
                .get("span")
                .copied()
                .unwrap_or(f64::from(luma_scene::catalog::DEFAULT_TRUSS_SPAN_M));
            let set = app.clone();
            layer = layer.child(
                div()
                    .absolute()
                    .top(px(INSET))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        float::popover_card()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .p(px(4.0))
                            .occlude()
                            .child(float::label("Span"))
                            .child(float::scrub(
                                "stage-held-span",
                                span,
                                hand::LENGTH_STEP_M,
                                MAX_RUN_M,
                                hand::LENGTH_STEP_M,
                                RUN_SCRUB_W,
                                move |value, _, cx| {
                                    set.update(cx, |this, cx| {
                                        this.stage_set_held_param("span", value, cx);
                                    });
                                },
                            ))
                            .agent_node(Role::Card, "Held span")
                            .map(float::frosted_card),
                    ),
            );
        }
    }
    layer.into_any_element()
}
