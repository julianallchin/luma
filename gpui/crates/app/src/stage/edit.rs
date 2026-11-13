//! Verbs that change what is already placed — trim, parameters, a dragged
//! pose, a row's count, delete — the one commit path they share, and undo.

use std::collections::BTreeMap;

use gpui::Context;
use luma_lib::models::venue_graph::PlacementReport;
use luma_scene::venue::NodeSockets as _;

use crate::library::LibraryError;
use crate::Luma;

use super::build::{Build, ParamScrub, Snapshot};

impl Luma {
    pub(crate) fn build_mut(&mut self) -> Option<&mut Build> {
        self.visualizer.as_mut()?.build.as_mut()
    }

    pub(crate) fn build_state(&self) -> Option<&Build> {
        self.visualizer.as_ref()?.build.as_ref()
    }

    /// Run one verb and adopt the venue it hands back.
    ///
    /// Every mutating command returns the whole solved venue, so there is
    /// exactly one place a report is unpacked and exactly one place the room
    /// is re-read. `committing` is what keeps a second release during the
    /// round trip from placing a second copy.
    pub(super) fn stage_verb(
        &mut self,
        pending: impl std::future::Future<Output = Result<PlacementReport, LibraryError>>
            + Send
            + 'static,
        cx: &mut Context<Self>,
    ) {
        // What the verb is about to replace is what undo restores.
        let snapshot = self.build_state().map(Build::snapshot);
        if let Some(build) = self.build_mut() {
            build.committing = true;
        }
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    if let Ok(report) = &result {
                        if let Some(snapshot) = snapshot {
                            build.remember(snapshot);
                        }
                        // What the verb touched is what the inspector
                        // should be about: the piece just placed is the
                        // one about to be trimmed or flipped.
                        build.select(Some(report.node_id.clone()));
                    }
                }
                if let Err(error) = result {
                    this.report_failure("The stage was not changed", error, cx);
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the venue after a write.
    ///
    /// The whole rig rather than the report's venue: the picture is built from
    /// fixture definitions and meshes as well as poses, and a builder that
    /// updated the graph without the scene would draw the room it had before
    /// the edit.
    pub(crate) fn reload_stage(&mut self, cx: &mut Context<Self>) {
        let Some(venue) = self.visualizer.as_ref().map(|v| v.venue_id.clone()) else {
            return;
        };
        let pending = self.library.venue_rig(&venue);
        cx.spawn(async move |this, cx| {
            let loaded = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this
                    .visualizer
                    .as_mut()
                    .filter(|state| state.venue_id == venue)
                {
                    state.rig_reloaded(loaded);
                    this.reload_patch(venue.clone(), cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Give the room walls and a ceiling at `size`, or take them away.
    pub(crate) fn stage_set_room(&mut self, size: Option<[f64; 3]>, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        if build.committing {
            return;
        }
        build.committing = true;
        let snapshot = build.snapshot();
        let venue = build.venue_id.clone();
        let pending = self.library.set_room(&venue, size);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    if result.is_ok() {
                        build.remember(snapshot);
                    }
                }
                if let Err(error) = result {
                    this.report_failure("The room was not changed", error, cx);
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-lay the selected fixture's row at a new count or layout.
    ///
    /// The verb derives the row from the member server-side and answers with
    /// the row as it now is; the selection follows it. A count that does not
    /// fit is an `Err` and changes nothing — the report line says why.
    pub(crate) fn stage_redistribute(
        &mut self,
        member: String,
        count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        if build.committing {
            return;
        }
        build.committing = true;
        let snapshot = build.snapshot();
        let venue = build.venue_id.clone();
        let layout =
            luma_lib::models::distribute::DistributeLayout::from(build.distribution_layout);
        let pending = self.library.redistribute(&venue, &member, count, layout);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                match &result {
                    Ok(report) => {
                        let ids: Vec<String> =
                            report.fixtures.iter().map(|f| f.id.clone()).collect();
                        if let Some(state) = this.visualizer_mut() {
                            state.replace_selection(
                                ids.iter()
                                    .cloned()
                                    .map(luma_render::frame::EditorObject::Fixture),
                            );
                        }
                        if let Some(build) = this.build_mut() {
                            build.committing = false;
                            build.select(ids.last().cloned());
                            build.distribution = ids;
                            build.remember(snapshot);
                        }
                    }
                    Err(error) => {
                        if let Some(build) = this.build_mut() {
                            build.committing = false;
                        }
                        this.report_failure("The lights were not spread out", error, cx);
                    }
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Preview the same vertical translation as trim, including attached children.
    /// Leave graph rows untouched until release so history records one gesture.
    pub(crate) fn stage_preview_trim(&mut self, metres: f64, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        if build.committing || !metres.is_finite() {
            return;
        }
        let Some(selected) = build.selected_view() else {
            return;
        };
        let delta = metres - selected.trim;
        let poses: Vec<_> = build
            .graph
            .subtree(&selected.node)
            .iter()
            .filter_map(|id| {
                let pose = build.solved.nodes.iter().find(|pose| &pose.id == id)?;
                let mut position = pose.position;
                position[2] += delta;
                Some((id.clone(), position))
            })
            .collect();
        if let Some(view) = self.visualizer_mut() {
            view.preview_stage_positions(&poses);
        }
        cx.notify();
    }

    /// Trim: how high a free placement flies. Children follow, because the
    /// resolver moves them — a subtree is a relation, not a set of poses.
    pub(crate) fn stage_set_trim(&mut self, metres: f64, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let (Some(node), venue) = (build.selected.clone(), build.venue_id.clone()) else {
            return;
        };
        if build.committing || !metres.is_finite() {
            return;
        }
        let minimum = build
            .selected_view()
            .and_then(|view| {
                let edge = build.graph.edge(&node)?;
                let height = build.room.socket_world(&node, &edge.my_socket)?.y;
                let (_, surface) = build.surface_below(&node)?;
                Some((surface - (height - view.trim)).max(0.0))
            })
            .unwrap_or(0.0);
        let metres = metres.max(minimum);
        let pending = self.library.set_params(
            &venue,
            &node,
            BTreeMap::from([("trim".to_string(), metres)]),
            None,
        );
        self.stage_verb(pending, cx);
    }

    /// Write one parameter of one node — the whole of what a joint's own
    /// freedom can change. `yaw` lands on the edge, which is where a mate's
    /// turn about the shared normal lives.
    pub(crate) fn stage_set_param(
        &mut self,
        node: &str,
        key: &str,
        value: f64,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        match build.param_scrub.as_mut() {
            Some(scrub) if scrub.node == node && scrub.key == key => {
                scrub.value = value;
                scrub.queued = true;
                cx.notify();
                return;
            }
            // Another control's writes are still running; this value waits
            // for a moment when nothing is in flight.
            Some(_) => return,
            None => {
                build.param_scrub = Some(ParamScrub {
                    node: node.to_string(),
                    key: key.to_string(),
                    value,
                    queued: true,
                    snapshot: build.snapshot(),
                    wrote: false,
                });
            }
        }
        self.send_param_scrub(cx);
    }

    /// Write the newest scrubbed value, re-read the rig, then go again if a
    /// newer value arrived meanwhile. One pass in flight at a time.
    fn send_param_scrub(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let venue = build.venue_id.clone();
        let Some(scrub) = build.param_scrub.as_mut() else {
            return;
        };
        if !scrub.queued {
            // Caught up: the graph now holds the last value, so the control
            // reads it again, and the whole scrub is one undo step.
            let scrub = build.param_scrub.take().expect("checked above");
            if scrub.wrote {
                build.remember(scrub.snapshot);
            }
            cx.notify();
            return;
        }
        scrub.queued = false;
        let (node, key, value) = (scrub.node.clone(), scrub.key.clone(), scrub.value);
        let write = self
            .library
            .set_params(&venue, &node, BTreeMap::from([(key, value)]), None);
        cx.spawn(async move |this, cx| {
            let result = write.await;
            let Ok(reread) = this.update(cx, |this, cx| {
                match &result {
                    Ok(_) => {
                        if let Some(scrub) = this
                            .build_mut()
                            .and_then(|build| build.param_scrub.as_mut())
                        {
                            scrub.wrote = true;
                        }
                    }
                    Err(error) => this.report_failure("The change was not saved", error, cx),
                }
                cx.notify();
                this.library.venue_rig(&venue)
            }) else {
                return;
            };
            let loaded = reread.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this
                    .visualizer
                    .as_mut()
                    .filter(|state| state.venue_id == venue)
                {
                    state.rig_reloaded(loaded);
                    this.reload_patch(venue.clone(), cx);
                }
                this.send_param_scrub(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Write dragged pieces' previewed poses into the graph.
    ///
    /// The gizmo previewed by writing the scene; a piece's position *lives* in
    /// its joint, so the release inverts the mate — the same
    /// [`luma_scene::venue::invert_placement`] the drop path uses — and writes
    /// `(u, v, yaw, trim)` through the ordinary verb. The re-solve then hands
    /// back the pose that was previewed, read out of the graph.
    pub(crate) fn stage_commit_pose(&mut self, pieces: Vec<String>, cx: &mut Context<Self>) {
        let Some(venue) = self.build_state().map(|build| build.venue_id.clone()) else {
            return;
        };
        let jobs: Vec<_> = pieces
            .iter()
            .filter_map(|id| self.gizmo_seat(id).map(|params| (id.clone(), params)))
            .collect();
        for (node, params) in jobs {
            let pending = self.library.set_params(&venue, &node, params, None);
            self.stage_verb(pending, cx);
        }
    }

    /// A dragged piece's previewed pose as the seat its joint would write.
    fn gizmo_seat(&self, node: &str) -> Option<BTreeMap<String, f64>> {
        let build = self.build_state()?;
        let world = self.visualizer.as_ref()?.piece_pose_three(node)?;
        let edge = build.graph.edge(node)?;
        let host = build.room.socket(&edge.parent, &edge.their_socket)?;
        let parent_world = build.room.pose(&edge.parent)?;
        let data = build.graph.node(node)?;
        let held_sockets = build.sockets.sockets(data);
        let held = held_sockets.iter().find(|s| s.name == edge.my_socket)?;
        let seat = luma_scene::venue::invert_placement(world, parent_world, host, held, data.kind);
        Some(BTreeMap::from([
            ("u".to_string(), seat.u),
            ("v".to_string(), seat.v),
            ("trim".to_string(), seat.trim.max(0.0)),
            ("yaw".to_string(), seat.yaw),
        ]))
    }

    /// Undo the last structural verb: restore the graph it replaced.
    pub(crate) fn stage_undo(&mut self, cx: &mut Context<Self>) {
        self.stage_time_travel(false, cx);
    }

    /// Step forward over the last undo.
    pub(crate) fn stage_redo(&mut self, cx: &mut Context<Self>) {
        self.stage_time_travel(true, cx);
    }

    /// Both directions are one move: pop a snapshot off one stack, park the
    /// present on the other, and ask the library to make the snapshot the
    /// room. A restore the server refuses burns both stacks — what they hold
    /// no longer matches the venue, and a history that might half-apply is
    /// worse than none.
    fn stage_time_travel(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        if build.committing {
            return;
        }
        let target = if forward {
            build.redo.pop()
        } else {
            build.undo.pop()
        };
        let Some(target) = target else {
            return;
        };
        let present = build.snapshot();
        if forward {
            build.undo.push(present);
        } else {
            build.redo.push(present);
        }
        build.committing = true;
        let venue = build.venue_id.clone();
        let pending = self
            .library
            .restore_graph(&venue, &target.rows, &target.patch);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    if result.is_err() {
                        build.forget_history();
                    }
                }
                if let Err(error) = result {
                    this.report_failure(
                        if forward {
                            "Could not redo that"
                        } else {
                            "Could not undo that"
                        },
                        error,
                        cx,
                    );
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Scene removal is undoable: a deleted light's patch row comes back with
    /// it.
    ///
    /// Acts on the whole selection — a row, a marquee, shift-clicks — not only
    /// the primary. A node whose ancestor is also selected is left to the
    /// ancestor's delete.
    pub(crate) fn stage_delete(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        if build.committing {
            return;
        }
        let mut nodes: Vec<String> = self
            .visualizer
            .as_ref()
            .map(|state| {
                state
                    .selected_objects()
                    .iter()
                    .map(|object| match object {
                        luma_render::frame::EditorObject::Fixture(id)
                        | luma_render::frame::EditorObject::StagePiece(id) => id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if nodes.is_empty() {
            nodes.extend(build.selected.clone());
        }
        let has_selected_ancestor = |node: &str| {
            let mut at = node;
            while let Some(edge) = build.graph.edge(at) {
                if nodes.iter().any(|n| *n == edge.parent) {
                    return true;
                }
                at = &edge.parent;
            }
            false
        };
        let jobs: Vec<String> = nodes
            .iter()
            .filter(|node| !has_selected_ancestor(node) && build.graph.node(node).is_some())
            .cloned()
            .collect();
        if jobs.is_empty() {
            return;
        }
        let venue = build.venue_id.clone();
        let snapshot = build.snapshot();
        if let Some(build) = self.build_mut() {
            build.committing = true;
            build.select(None);
        }
        if let Some(state) = self.visualizer.as_mut() {
            state.clear_selection();
        }
        self.run_stage_deletes(
            venue,
            jobs.into_iter().rev().collect(),
            snapshot,
            Vec::new(),
            cx,
        );
    }

    /// Run queued deletes one at a time, so each sees the graph the last one
    /// left. `jobs` is popped from the tail. The whole batch is one undo step.
    fn run_stage_deletes(
        &mut self,
        venue: String,
        mut jobs: Vec<String>,
        snapshot: Snapshot,
        mut errors: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = jobs.pop() else {
            if let Some(build) = self.build_mut() {
                build.committing = false;
                build.select(None);
                // A partial batch still changed the graph; one snapshot
                // restores all of it.
                build.remember(snapshot);
            }
            if !errors.is_empty() {
                self.report_failure("Some pieces were not removed", errors.join("; "), cx);
            }
            self.reload_stage(cx);
            cx.notify();
            return;
        };
        let pending = self.library.delete_subtree(&venue, &node);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    errors.push(error.to_string());
                }
                this.run_stage_deletes(venue, jobs, snapshot, errors, cx);
            })
            .ok();
        })
        .detach();
    }
}
