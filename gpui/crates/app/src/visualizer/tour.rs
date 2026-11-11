//! The camera walkthrough: the first time an account opens a score, a card
//! over the stage names each way of moving the eye, one at a time, and moves
//! on when the reader has made that move. It ends on the room's light dial.
//!
//! Whether an account has been through it is a session item of the shell's
//! ([`Luma::sync_camera_tour`]); the card and its place in the walk are the
//! stage's.
use super::*;

/// The session item that says the account has been through the walkthrough.
pub(crate) const KEY: &str = "camera-tour";

/// How far up the house lights are held while the walkthrough is up, in a
/// room saved darker than that: the reader is learning to look around a room
/// and has to be able to see it.
const HOUSE: f32 = 0.3;

/// How long a move stays ticked before the next one is asked for.
const TICKED: Duration = Duration::from_millis(700);

/// A way of moving the eye, and last the room's light dial. The walk asks
/// for them in this order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Move {
    Orbit,
    Zoom,
    Pan,
    Fly,
    Lights,
}

impl Move {
    const ALL: [Move; 5] = [Move::Orbit, Move::Zoom, Move::Pan, Move::Fly, Move::Lights];

    /// `indoor` is whether the room's dial is its house lights, or the sun.
    fn says(self, indoor: bool) -> &'static str {
        match self {
            Move::Orbit => "Drag to look around",
            Move::Zoom => "Scroll to zoom",
            Move::Pan => "Drag with the middle button to slide",
            Move::Fly => "Hold the right button and press W A S D to fly",
            Move::Lights if indoor => "Turn the house lights down on the dial at the right",
            Move::Lights => "Set the time of day on the dial at the right",
        }
    }

    fn icon(self) -> luma_ui::icons::IconName {
        use luma_ui::icons::IconName;
        match self {
            Move::Orbit | Move::Pan => IconName::Mouse,
            Move::Zoom => IconName::MouseScroll,
            Move::Fly => IconName::KeyboardMouse,
            Move::Lights => IconName::Camera,
        }
    }
}

/// Where the reader is in the walk.
#[derive(Default)]
pub(crate) struct Tour {
    /// The move being asked for, as an index into [`Move::ALL`]; past the end
    /// once the walk is over.
    step: usize,
    /// When the reader made it.
    made: Option<Instant>,
    /// The reader has changed the room's light, at any step: from then
    /// on the room is lit as they left it — see [`Visualizer::tour_lit`].
    dialled: bool,
}

impl Tour {
    fn asking(&self) -> Option<Move> {
        Move::ALL.get(self.step).copied()
    }

    /// Finished or skipped: the shell records it and takes the card down.
    pub(crate) fn over(&self) -> bool {
        self.asking().is_none()
    }

    fn skip(&mut self) {
        self.step = Move::ALL.len();
    }

    /// The reader moved the eye. Only the move being asked for counts.
    fn made(&mut self, made: Move, now: Instant) {
        if self.asking() == Some(made) && self.made.is_none() {
            self.made = Some(now);
        }
    }

    /// Ask for the next move once this one has stayed ticked and the hand
    /// that made it has let go.
    fn advance(&mut self, now: Instant, held: bool) {
        if !held
            && self
                .made
                .is_some_and(|made| now.saturating_duration_since(made) >= TICKED)
        {
            self.step += 1;
            self.made = None;
        }
    }
}

impl Visualizer {
    /// Tell the walkthrough the eye was moved, if one is up.
    pub(super) fn toured(&mut self, made: Move) {
        if let Some(tour) = self.tour.as_mut() {
            tour.made(made, Instant::now());
        }
    }

    /// The room as the walkthrough shows it: an indoor room never darker
    /// than [`HOUSE`], until the reader turns the dial themselves. Outdoors
    /// the sky is left where it was saved.
    pub(super) fn tour_lit(&self, room: VenueEnvironment) -> VenueEnvironment {
        let dark = matches!(room, VenueEnvironment::Indoor { .. }) && room.house_level() < HOUSE;
        if self.tour.as_ref().is_some_and(|tour| !tour.dialled) && dark {
            room.lit_at(HOUSE)
        } else {
            room
        }
    }

    /// The reader changed the room's light. Called before the change is
    /// applied, so the room is re-lit without the walkthrough's hold and what
    /// is saved is what is shown.
    pub(super) fn tour_dialled(&mut self) {
        if let Some(tour) = self.tour.as_mut() {
            tour.dialled = true;
        }
    }

    /// Put the walkthrough up or take it down, and light the room to match.
    fn set_tour(&mut self, tour: Option<Tour>) {
        self.tour = tour;
        self.render_controls.set_environment(self.environment());
    }

    /// One frame of the walkthrough.
    pub(super) fn tour_tick(&mut self, now: Instant) {
        let held = self.drag.is_some() || self.fly.is_some();
        if let Some(tour) = self.tour.as_mut() {
            tour.advance(now, held);
        }
    }
}

impl Luma {
    /// Put the walkthrough up over a score for an account that has not been
    /// through it, and record it once it is over. Called at draw, after the
    /// stage is pointed at what is open — see [`Luma::sync_visualizer`].
    pub(crate) fn sync_camera_tour(&mut self, cx: &mut Context<Self>) {
        let seen = *self.camera_tour.value();
        let Some(state) = self.visualizer.as_mut() else {
            return;
        };
        let score = state.subject.is_some() && !state.rigging;
        match &state.tour {
            Some(tour) if tour.over() => {
                state.set_tour(None);
                self.store_session(|this| &mut this.camera_tour, true, cx);
                cx.notify();
            }
            Some(_) if seen || !score => state.set_tour(None),
            None if !seen && score => state.set_tour(Some(Tour::default())),
            _ => {}
        }
    }

    /// The store's value, for the read at sign-in.
    pub(crate) fn read_camera_tour(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<String>, LibraryError>> + use<> {
        self.library.get_session_item(&self.account_key(KEY))
    }
}

/// The walkthrough's card, centred over the stage's bottom edge.
pub(super) fn card(state: &Visualizer, app: &Entity<Luma>) -> Option<AnyElement> {
    let tour = state.tour.as_ref().filter(|_| !state.presentation)?;
    let asking = tour.asking()?;
    let made = tour.made.is_some();
    let says = asking.says(matches!(
        state.venue_environment,
        VenueEnvironment::Indoor { .. }
    ));
    let skip = app.clone();
    // Clear of the toolbar when it is up.
    let lift = if state.bottom_bar_visible {
        px(48.)
    } else {
        px(0.)
    };
    Some(
        div()
            .absolute()
            .bottom(TOOLBAR_OVERLAY_BOTTOM + lift)
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                luma_ui::float::popover_card()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(12.))
                    .pr(px(6.))
                    .py(px(6.))
                    // The card owns the pointer it covers: a press on it is
                    // not the start of an orbit.
                    .occlude()
                    .child(if made {
                        gpui_component::Icon::new(luma_ui::icons::IconName::Check)
                            .size(px(18.))
                            .text_color(ladder::primary())
                            .into_any_element()
                    } else {
                        gpui_component::Icon::new(asking.icon())
                            .size(px(18.))
                            .text_color(luma_ui::glass::ink(0.95))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(ladder::foreground())
                            .child(says)
                            .agent_node(Role::Text, says),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(luma_ui::glass::ink(luma_ui::glass::TEXT_TERTIARY))
                            .child(format!("{} of {}", tour.step + 1, Move::ALL.len())),
                    )
                    .child(
                        luma_ui::button("Skip", luma_ui::Enabled::Yes)
                            .id("camera-tour-skip")
                            .on_click(move |_, _, cx| {
                                skip.update(cx, |this, cx| {
                                    if let Some(tour) =
                                        this.visualizer_mut().and_then(|state| state.tour.as_mut())
                                    {
                                        tour.skip();
                                    }
                                    cx.notify();
                                });
                            })
                            .agent_node(Role::Button, "Skip camera walkthrough"),
                    )
                    .map(luma_ui::float::frosted_card),
            )
            .agent_node(Role::Card, "Camera walkthrough")
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walk_asks_for_each_move_in_turn_and_only_counts_the_one_asked_for() {
        let start = Instant::now();
        let later = start + TICKED;
        let mut tour = Tour::default();
        for (ix, asked) in Move::ALL.into_iter().enumerate() {
            assert_eq!(tour.asking(), Some(asked));
            // A move that is not the one asked for leaves the walk where it is.
            let other = Move::ALL[(ix + 1) % Move::ALL.len()];
            tour.made(other, start);
            tour.advance(later, false);
            assert_eq!(tour.asking(), Some(asked));

            tour.made(asked, start);
            // Ticked, but not yet moved on: too soon, and then still held.
            tour.advance(start, false);
            tour.advance(later, true);
            assert_eq!(tour.asking(), Some(asked));
            tour.advance(later, false);
        }
        assert!(tour.over());
    }

    #[test]
    fn a_dark_room_is_lit_for_the_walk_until_the_reader_turns_the_dial() {
        let camera = opening_camera(
            &Framing::default(),
            &Viewfinder::new(FOV_Y_DEG, DEFAULT_ASPECT),
        );
        let mut view = test_support::visualizer(None, test_support::empty_pick(camera));
        let dark = VenueEnvironment::indoor(0.0);
        let bright = VenueEnvironment::indoor(0.8);
        view.set_venue_environment(dark);
        assert_eq!(view.environment().house_level(), 0.0);

        view.set_tour(Some(Tour::default()));
        assert!(view.environment().house_level() > 0.0);
        // A room already brighter than the walk needs is left alone.
        view.set_venue_environment(bright);
        assert_eq!(view.environment().house_level(), bright.house_level());

        view.tour_dialled();
        view.set_venue_environment(dark);
        assert_eq!(view.environment().house_level(), 0.0);
    }

    #[test]
    fn skipping_ends_the_walk() {
        let mut tour = Tour::default();
        assert!(!tour.over());
        tour.skip();
        assert!(tour.over());
    }
}
