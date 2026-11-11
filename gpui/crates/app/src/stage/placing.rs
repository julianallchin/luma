//! What the hand does with a held thing: aim it at the room, drop it,
//! run a truss out of a socket, and duplicate or flip a selected subtree.

use gpui::{Context, Pixels, Point};
use luma_lib::models::venue_graph::PlacementReport;

use crate::library::LibraryError;
use crate::Luma;

use super::hand::{self, Extending, Hand, Held, Holding, Landing};
use super::layer::project_socket;

/// One verb in flight. Boxed because the four placements are four different
/// futures of one shape, and the caller only ever awaits them.
type Verb = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<PlacementReport, LibraryError>> + Send>,
>;

impl Luma {
    /// The cursor moved over the room while something is held.
    ///
    /// Both halves of the aim are used and they answer different questions:
    /// the pointer's *pixel* chooses which joint the gesture means, and the
    /// world point it met is where a free seat actually goes. The projection
    /// is taken from the same camera the marks are drawn through
    /// ([`project_socket`]), so a pointer and a mark cannot disagree.
    pub(crate) fn stage_aim(&mut self, aimed: hand::Aimed, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer.as_ref() else {
            return;
        };
        let camera = state.camera();
        let pane = state.stage_pane();
        let size = (f32::from(pane.size.width), f32::from(pane.size.height));
        let cursor = glam::DVec2::new(
            f64::from(f32::from(aimed.at.x - pane.origin.x)),
            f64::from(f32::from(aimed.at.y - pane.origin.y)),
        );
        let project = |at: glam::DVec3| {
            project_socket(&camera, size, at).map(|(p, depth)| {
                glam::DVec3::new(f64::from(f32::from(p.x)), f64::from(f32::from(p.y)), depth)
            })
        };
        // No pane yet is no camera to point through, and then the ladder falls
        // back to the world-space radii rather than aiming at nothing.
        let aim = (size.0 > 1.0 && size.1 > 1.0).then_some(luma_scene::snap::Aim {
            cursor,
            project: &project,
        });
        let hand::Aimed { world, hit, .. } = &aimed;
        let (world, hit) = (*world, hit.clone());
        let Some(build) = self.build_mut() else {
            return;
        };
        let held = build.held_sockets();
        let body = build.held_bounds();
        let Some(hand::Held { what, latched, .. }) = build.hand.held() else {
            return;
        };
        let kind = what.kind();
        let footing = what.footing().map(str::to_string);
        let exclude = match what {
            Holding::Duplicate { root, .. } => Some(root.clone()),
            _ => None,
        };
        let latch = latched.clone();
        let solved = build.room.land(
            &held,
            kind,
            footing.as_deref(),
            world,
            aim.as_ref(),
            latch.as_ref(),
            hit.as_ref(),
            exclude.as_deref(),
            body,
        );
        if let Hand::Placing(held) = &mut build.hand {
            held.cursor = Some(aimed);
            match solved {
                Some((landed, latch)) => {
                    held.landed = Some(landed);
                    held.latched = latch;
                }
                None => {
                    held.landed = None;
                    held.latched = None;
                }
            }
        }
        cx.notify();
    }

    /// Edit one parameter of the thing in the hand — the span scrub a held
    /// truss offers. The landing re-solves from the remembered cursor, so the
    /// ghost grows in place instead of waiting for the pointer to move.
    pub(crate) fn stage_set_held_param(&mut self, key: &str, value: f64, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Hand::Placing(held) = &mut build.hand else {
            return;
        };
        let Holding::Piece { params, .. } = &mut held.what else {
            return;
        };
        params.insert(key.to_string(), value);
        // A changed parameter moves the sockets, so last frame's latch may
        // describe a joint the piece no longer reaches.
        held.latched = None;
        let cursor = held.cursor.clone();
        match cursor {
            Some(aimed) => self.stage_aim(aimed, cx),
            None => cx.notify(),
        }
    }

    /// Aim the hand at whatever the pointer is over: the picture's own mesh
    /// hit when a frame is up, the floor plane when none is.
    pub(crate) fn stage_aim_from_pointer(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((world, hit)) = self
            .visualizer
            .as_ref()
            .and_then(|state| state.stage_cursor(at))
        else {
            return;
        };
        self.stage_aim(hand::Aimed { at, world, hit }, cx);
    }

    /// A click on the room while the hand is placing: aim there, then commit.
    ///
    /// The viewport's release handler calls this for a *click* — a left drag
    /// is the camera's, which is what makes an orbit available with a ghost
    /// in the air.
    pub(crate) fn stage_click_room(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self
            .build_state()
            .is_some_and(|build| build.hand.aims_with_pointer())
        {
            return;
        }
        self.stage_aim_from_pointer(at, cx);
        self.stage_drop(at, cx);
    }

    /// Release: commit whatever the ghost is standing on.
    ///
    /// `at` is where the release landed in window pixels, which only a fixture
    /// needs: it is the point the configure popover anchors to.
    pub(crate) fn stage_drop(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        if build.committing {
            return;
        }
        let venue = build.venue_id.clone();
        let root = build.room.root().to_string();
        let Some(held) = build.hand.held() else {
            return;
        };
        let Some(landed) = held.landed.clone() else {
            return;
        };
        if landed.refused.is_some() {
            return;
        }
        let what = held.what.clone();
        // A fixture on the venue's own floor is one light at the point under
        // the pointer, which is what the ghost showed. Anywhere else it is a
        // *row*, and a row needs a face to lie along: whatever the ghost is
        // standing on. The bead is how a *named* socket is aimed at, but a
        // truss's underside and a deck top are areas, and a face a person can
        // point at is a face they can distribute along. Landing on something
        // that is not a feature — a bolt circle, a clamp — leaves the ghost
        // in the hand, because "one of them there" is not what a row means.
        if let (
            Holding::Fixture { path, mode, .. },
            Landing::Free {
                surface: None,
                seat,
                ..
            },
        ) = (&what, &landed.how)
        {
            // No mode until the definition lands; the ghost stays in the hand.
            let Some(mode) = mode.clone() else {
                return;
            };
            let pending = self.library.distribute(
                &venue,
                None,
                path,
                &mode,
                1,
                luma_lib::models::distribute::DistributeLayout::At {
                    metres: seat.u,
                    across: seat.v,
                },
                None,
            );
            self.stage_place_row(pending, Hand::Placing(Held::again(&what)), cx);
            return;
        }
        if matches!(what, Holding::Fixture { .. }) {
            let (node, socket) = match &landed.how {
                Landing::Free { surface, .. } => surface
                    .clone()
                    .unwrap_or((root, luma_scene::venue::FLOOR_SOCKET.to_string())),
                Landing::Socket {
                    parent,
                    their_socket,
                    ..
                } => (parent.clone(), their_socket.clone()),
            };
            if build
                .room
                .socket(&node, &socket)
                .is_some_and(hand::is_feature)
            {
                self.stage_configure(what, node, socket, at, cx);
            }
            return;
        }
        // Place mode is sticky, and only for a catalog piece: that is a stamp,
        // and an operator rigging a row of them should not have to walk back
        // to the dialog between each. A duplicate is one specific thing —
        // once placed there is no second one to hold.
        build.hand = match &what {
            Holding::Piece { .. } => Hand::Placing(Held::again(&what)),
            Holding::Duplicate { .. } | Holding::Fixture { .. } => Hand::Idle,
        };
        let pending: Verb = match (&what, &landed.how) {
            // Handled above: a fixture is created by `distribute`.
            (Holding::Fixture { .. }, _) => return,
            (
                Holding::Piece {
                    catalog_ref,
                    kind,
                    display_name,
                    params,
                    ..
                },
                Landing::Socket {
                    parent,
                    my_socket,
                    their_socket,
                    yaw,
                },
            ) => Box::pin(self.library.attach(
                &venue,
                kind.as_str(),
                Some(catalog_ref),
                Some(display_name),
                parent,
                my_socket,
                their_socket,
                *yaw,
                params.clone(),
            )),
            (
                Holding::Piece {
                    catalog_ref,
                    kind,
                    display_name,
                    params,
                    ..
                },
                Landing::Free {
                    surface,
                    my_socket,
                    seat,
                },
            ) => Box::pin(self.library.place_free(
                &venue,
                kind.as_str(),
                Some(catalog_ref),
                Some(display_name),
                surface.as_ref().map(|(n, s)| (n.as_str(), s.as_str())),
                my_socket,
                *seat,
                params.clone(),
            )),
            (Holding::Duplicate { root, flip, .. }, how) => {
                self.stage_duplicate_commit(&venue, root, *flip, how, cx);
                return;
            }
        };
        self.stage_verb(pending, cx);
    }

    /// Start measuring a run out of `socket` — the node menu's "Extend run".
    pub(crate) fn stage_extend_from(
        &mut self,
        node: String,
        socket: String,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        // The measurement is the verb's, not a ray of the page's own: `extend`
        // refuses against exactly this number, and a second cast here could
        // offer a length the command would then reject. Asked once, on the
        // click — the answer is a property of the room, and re-casting it while
        // a length is being typed would move the number under the operator.
        let venue = build.venue_id.clone();
        build.hand = Hand::Extending(Box::new(Extending {
            from_node: node.clone(),
            from_socket: socket.clone(),
            reach: None,
            length_m: hand::STUB_LENGTH_M,
        }));
        let measuring = self.library.extend_reach(&venue, &node, &socket);
        cx.spawn(async move |this, cx| {
            let reach = measuring.await.ok().flatten();
            this.update(cx, |this, cx| {
                if let Some(Hand::Extending(run)) = this.build_mut().map(|b| &mut b.hand) {
                    // Only if the same run is still in hand: the operator may
                    // have escaped or clicked elsewhere while this was in
                    // flight, and a measurement for a socket nobody is pointing
                    // at is worse than none.
                    if run.from_node == node && run.from_socket == socket {
                        run.length_m = reach.as_ref().map_or(hand::STUB_LENGTH_M, |r| r.gap_m);
                        run.reach = reach;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Type or drag a length for the run in hand.
    pub(crate) fn stage_set_length(&mut self, metres: f64, cx: &mut Context<Self>) {
        if let Some(Hand::Extending(run)) = self.build_mut().map(|build| &mut build.hand) {
            run.length_m = hand::quantize(metres);
        }
        cx.notify();
    }

    /// Commit the run.
    ///
    /// One verb: `extend` re-measures the gap, refuses a length past it, and
    /// writes the far-end check itself when the run bridges — so the page does
    /// not compose an attach and a constrain of its own, and the Python facade
    /// and this button cannot disagree about what an extend is.
    pub(crate) fn stage_commit_run(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Some(run) = build.hand.extending() else {
            return;
        };
        if run.refused().is_some() {
            return;
        }
        let venue = build.venue_id.clone();
        let parent = run.from_node.clone();
        let socket = run.from_socket.clone();
        let length = run.length_m;
        build.hand = Hand::Idle;
        let pending = self.library.extend(&venue, &parent, &socket, Some(length));
        self.stage_verb(Box::pin(pending), cx);
    }

    /// ⌘D: copy the selected subtree and put it in the hand.
    pub(crate) fn stage_duplicate(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Some(root) = build.selected.clone() else {
            return;
        };
        let display_name = build.label_of(&root);
        build.hand = Hand::Placing(Held::new(Holding::Duplicate {
            root,
            display_name,
            flip: false,
        }));
        cx.notify();
    }

    /// Invert the handedness of the copy in hand.
    pub(crate) fn stage_flip(&mut self, cx: &mut Context<Self>) {
        if let Some(Hand::Placing(held)) = self.build_mut().map(|build| &mut build.hand) {
            if let Holding::Duplicate { flip, .. } = &mut held.what {
                *flip = !*flip;
            }
        }
        cx.notify();
    }

    /// Commit a held duplicate.
    ///
    /// One verb: `duplicate` walks the same plan the ghost previewed and writes
    /// every copy in one call, so the page owns no loop of its own to drift
    /// from the one the Python facade calls.
    fn stage_duplicate_commit(
        &mut self,
        venue: &str,
        root: &str,
        flip: bool,
        how: &Landing,
        cx: &mut Context<Self>,
    ) {
        // A duplicate is placed on a socket: every compatible open socket
        // lights up precisely so the copy lands on a joint, and a copy seated
        // free would be a second, different gesture.
        let Landing::Socket {
            parent,
            their_socket,
            ..
        } = how
        else {
            return;
        };
        let pending = self
            .library
            .duplicate(venue, root, parent, their_socket, flip);
        self.stage_verb(Box::pin(pending), cx);
    }
}
