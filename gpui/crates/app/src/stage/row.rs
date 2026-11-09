//! Fitting a row of fixtures to one face: the configure verbs and the
//! popover that previews the count and layout before it is placed.

use std::collections::BTreeMap;

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, Pixels, Point};
use luma_ui::float::{self, Dismiss};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::Luma;

use super::hand::{self, Hand, Holding};

impl Luma {
    /// Start fitting a row of `what` to one face.
    ///
    /// The preview is solved immediately rather than on the first edit, so the
    /// popover opens over a picture that already answers "what would one look
    /// like here" — which is the question the count is an adjustment to.
    pub(crate) fn stage_configure(
        &mut self,
        what: Holding,
        node: String,
        socket: String,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let host_label = build.label_of(&node);
        build.hand = Hand::Configuring(Box::new(hand::Configuring {
            what,
            host_node: node,
            host_label,
            host_socket: socket,
            at,
            count: 1,
            layout: luma_scene::distribute::Layout::Even,
            preview: Ok(Vec::new()),
        }));
        build.repreview();
        cx.notify();
    }

    /// Change how many bodies the row has, and re-solve it.
    pub(crate) fn stage_set_count(&mut self, count: usize, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            if let Hand::Configuring(row) = &mut build.hand {
                row.count = count.max(1);
            }
            build.repreview();
        }
        cx.notify();
    }

    /// Change how the row is spread, and re-solve it.
    pub(crate) fn stage_set_layout(
        &mut self,
        layout: luma_scene::distribute::Layout,
        cx: &mut Context<Self>,
    ) {
        if let Some(build) = self.build_mut() {
            if let Hand::Configuring(row) = &mut build.hand {
                row.layout = layout;
            }
            build.repreview();
        }
        cx.notify();
    }

    /// Grow the host to the length the fit failure asked for, then re-solve.
    ///
    /// The one press that answers a refusal, and it is a *preview* press: the
    /// host really does get longer, because that is an edit to the room and
    /// the room is where a truss's span lives — but the row itself is still
    /// only previewed, so the operator sees the fit before committing to it.
    pub(crate) fn stage_extend_host(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let venue = build.venue_id.clone();
        let Some(row) = build.hand.configuring() else {
            return;
        };
        let Err(luma_scene::distribute::Fit::TooLong { needed_m, .. }) = row.preview else {
            return;
        };
        let host = row.host_node.clone();
        let pending = self.library.set_params(
            &venue,
            &host,
            BTreeMap::from([("span".to_string(), needed_m)]),
            None,
        );
        cx.spawn(async move |this, cx| {
            let _ = pending.await;
            this.update(cx, |this, cx| {
                this.reload_stage(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Commit the row the popover has been previewing.
    pub(crate) fn stage_apply_row(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let venue = build.venue_id.clone();
        let snapshot = build.snapshot();
        let Some(row) = build.hand.configuring() else {
            return;
        };
        // A refused row has no poses, and a partial distribution is not one.
        if row.preview.is_err() {
            return;
        }
        let Holding::Fixture { path, mode, .. } = &row.what else {
            return;
        };
        let Some(mode) = mode.clone() else {
            return;
        };
        let pending = self.library.distribute(
            &venue,
            Some((row.host_node.as_str(), row.host_socket.as_str())),
            path,
            &mode,
            row.count,
            row.layout.into(),
            None,
        );
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Idle;
            build.committing = true;
        }
        cx.spawn(async move |this, cx| {
            let report = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    if report.as_ref().is_ok_and(|report| report.refusal.is_none()) {
                        build.remember(snapshot);
                    }
                }
                if let Err(error) = &report {
                    this.report_failure("The lights were not placed", error, cx);
                }
                // The row's own report carries no venue, so the room is re-read
                // rather than adopted — one more round trip, and the only verb
                // that needs it.
                this.reload_stage(cx);
            })
            .ok();
        })
        .detach();
    }
}

/// The row being fitted, as the popover draws it.
pub(crate) struct ConfigureView {
    pub(crate) host: String,
    pub(crate) what: String,
    pub(crate) at: Point<Pixels>,
    pub(crate) count: usize,
    pub(crate) even: bool,
    /// How many bodies the preview placed, or why it placed none.
    pub(crate) fits: Result<usize, String>,
    /// The label of the one press that would make a refused row fit.
    pub(crate) offer: Option<String>,
    /// Whether the row can be committed — it fits and its mode has landed.
    pub(crate) ready: bool,
}

/// The configure popover, hung at the point on the face that was clicked.
///
/// In the viewport and not a modal, because every control on it changes the
/// picture behind it: the count and the layout are questions about what the
/// room would look like, and a card that covered the room while asking them
/// would be asking the operator to imagine the answer it is already drawing.
pub(super) fn configure(view: &ConfigureView, app: &Entity<Luma>) -> AnyElement {
    let mut card = float::popover_card().w(px(260.0)).gap(px(8.0)).p(px(8.0));
    card = card.child(float::label(view.what.clone())).child(
        div()
            .text_size(px(11.0))
            .text_color(ladder::foreground_alpha(0.45))
            .child(view.host.clone())
            .agent_node(Role::Text, format!("On {}", view.host)),
    );

    let set_count = app.clone();
    card = card.child(float::field_row(
        "Count",
        float::scrub(
            "stage-count",
            view.count as f64,
            1.0,
            MAX_COUNT,
            1.0,
            CONTROL_WIDTH,
            move |value, _, cx| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let count = value.round().max(1.0) as usize;
                set_count.update(cx, |this, cx| this.stage_set_count(count, cx));
            },
        ),
    ));

    // Two of the three layouts: even always fits, a pitch is the one that can
    // be refused, and the third is a pair of fractions that wants a handle on
    // the picture rather than a cell in a card.
    let mut track = float::segmented().w(px(CONTROL_WIDTH));
    for (name, even) in [("Even", true), ("Spacing", false)] {
        let app = app.clone();
        track = track.child(
            float::segment(name, view.even == even, name)
                .id(name)
                .on_click(move |_, _, cx| {
                    let layout = if even {
                        luma_scene::distribute::Layout::Even
                    } else {
                        luma_scene::distribute::Layout::Spacing(DEFAULT_PITCH_M)
                    };
                    app.update(cx, |this, cx| this.stage_set_layout(layout, cx));
                })
                .agent_node(Role::Toggle, name),
        );
    }
    card = card.child(float::field_row("Layout", track));

    // The fit, always — a row that fits says so as quietly as one that does
    // not says why. Both sit directly above the button they qualify.
    match &view.fits {
        Ok(placed) => {
            card = card.child(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::foreground_alpha(0.45))
                    .child(format!("{placed} will fit"))
                    .agent_node(Role::Text, format!("{placed} will fit")),
            );
        }
        Err(why) => {
            card = card.child(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::status_warn())
                    .child(why.clone())
                    .agent_node(Role::Text, why.clone()),
            );
            if let Some(offer) = &view.offer {
                let extend = app.clone();
                card = card.child(
                    float::btn(offer.clone(), "stage-extend")
                        .id("stage-extend")
                        .w_full()
                        .on_click(move |_, _, cx| {
                            extend.update(cx, |this, cx| this.stage_extend_host(cx));
                        })
                        .agent_node(Role::Button, offer.clone()),
                );
            }
        }
    }

    let apply = app.clone();
    let ready = view.ready;
    card = card.child(
        float::btn_primary("Place")
            .id("stage-place-row")
            .w_full()
            .when(!ready, |b| b.opacity(float::INERT_OPACITY))
            .on_click(move |_, _, cx| {
                apply.update(cx, |this, cx| this.stage_apply_row(cx));
            })
            .agent_node(Role::Button, "Place")
            .agent_disabled(!ready),
    );

    let close = app.clone();
    float::anchored_at(
        "stage-configure",
        view.at,
        Dismiss::on_press_out(move |_, cx| {
            close.update(cx, |this, cx| this.stage_escape(cx));
        }),
        // The card reports its own bounds, because *where it is* is a claim:
        // the spec asks it to sit on the face that was clicked, and only the
        // rectangle can say whether it did.
        card.agent_node(Role::Card, "Row popover")
            .into_any_element(),
    )
}

/// The most bodies one gesture will seat on one face.
pub(super) const MAX_COUNT: f64 = 24.0;

/// The pitch a spacing layout starts at, in metres.
pub(super) const DEFAULT_PITCH_M: f64 = 1.0;

/// The width of a control inside the configure popover — the card, less both
/// gutters.
const CONTROL_WIDTH: f32 = 244.0;
