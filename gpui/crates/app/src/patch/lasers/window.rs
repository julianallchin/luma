//! The window editor: the venue's laser window on a graph of directions,
//! moved, resized and drawn by hand.
//!
//! The graph is the room as a projector looks out at it: across is the turn
//! from straight ahead into the house, up is the tilt from level. The window
//! is a rectangle on it. Drag it to move it, pull an edge or a corner to
//! resize it, or drag on the empty graph to draw a new one. It lands on
//! whole degrees, and is drawn to the middle, to level and to an even split
//! either side of ahead when it comes near them; Alt resizes about its
//! middle and Ctrl or Cmd lets go of the snapping.
//!
//! Each projector's scan is drawn behind it, where that projector can
//! reach: a window outside one is light that projector cannot draw.

use gpui::prelude::*;
use gpui::{
    canvas, div, point, px, relative, AnyElement, Bounds, Context, CursorStyle, Entity, Modifiers,
    MouseButton, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, SharedString,
};
use luma_lib::laser::Window;
use luma_ui::ladder;
use luma_ui::node::{Instrument as _, Role};

use crate::library::Lasers;
use crate::patch::Patch;
use crate::Luma;

/// The graph, in pixels: twice as wide as tall, as its angles are.
pub(super) const WIDTH: f32 = 640.0;
const HEIGHT: f32 = WIDTH / 2.0;
/// The turn and tilt the graph shows, degrees: a quarter turn either side
/// of ahead, and from a little under level to well up.
const TURN: [f64; 2] = [-90., 90.];
const TILT: [f64; 2] = [-30., 60.];
/// A grid line every this many degrees.
const GRID: f64 = 15.;
/// How near, in degrees, the window is drawn to a place worth landing on.
const SNAP: f64 = 2.5;
/// A handle's square, and how far round it a press still takes it.
const HANDLE: f32 = 9.0;
const REACH: f32 = 15.0;
/// The narrowest and shortest a window is dragged to, degrees.
const LEAST: f64 = 2.;

/// What of the window a drag has hold of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Grab {
    /// The whole window.
    Move,
    /// Its edges: −1 the lower edge of turn or tilt, 1 the upper, 0 neither.
    Edges { turn: i8, tilt: i8 },
    /// A new window, from where the press was to where the pointer is.
    Draw,
}

/// A drag of the window in hand.
#[derive(Clone, Copy, Debug)]
pub(super) struct Drag {
    grab: Grab,
    /// The window as it was when it was taken.
    from: Window,
    /// Where on the graph the press was, degrees.
    press: [f64; 2],
}

/// A line the window has been drawn to, shown while it holds: a turn
/// (upright) or a tilt (level), degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Guide {
    upright: bool,
    at: f64,
}

/// Where on the graph `at` is, degrees of turn and tilt.
fn angles(bounds: Bounds<Pixels>, at: Point<Pixels>) -> [f64; 2] {
    let along = |at: Pixels, origin: Pixels, size: Pixels| {
        f64::from(f32::from(at - origin) / f32::from(size).max(1.0))
    };
    let (x, y) = (
        along(at.x, bounds.origin.x, bounds.size.width),
        along(at.y, bounds.origin.y, bounds.size.height),
    );
    [
        TURN[0] + x * (TURN[1] - TURN[0]),
        TILT[1] - y * (TILT[1] - TILT[0]),
    ]
}

/// Where turn and tilt sit on the graph, each 0 to 1 from its left and top.
#[allow(clippy::cast_possible_truncation)]
fn place([turn, tilt]: [f64; 2]) -> [f32; 2] {
    [
        ((turn - TURN[0]) / (TURN[1] - TURN[0])) as f32,
        ((TILT[1] - tilt) / (TILT[1] - TILT[0])) as f32,
    ]
}

/// `value` on a whole degree, or on the nearest of `targets` when it is
/// within [`SNAP`] of one and snapping is on; the target, when it took one.
fn land(value: f64, targets: &[f64], snapping: bool) -> (f64, Option<f64>) {
    let nearest = targets
        .iter()
        .copied()
        .filter(|target| snapping && (target - value).abs() <= SNAP)
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()));
    (nearest.unwrap_or_else(|| value.round()), nearest)
}

impl Drag {
    /// The window with the pointer at `at`, and the lines it is held to.
    ///
    /// `centred` resizes about the window's middle; `snapping` draws it to
    /// ahead, to level and to an even split either side of ahead.
    fn window(&self, at: [f64; 2], centred: bool, snapping: bool) -> (Window, Vec<Guide>) {
        let delta = [at[0] - self.press[0], at[1] - self.press[1]];
        let from = [self.from.turn, self.from.tilt];
        // The widest each range may be: a window's own bounds.
        let bound = [180., 90.];
        let mut guides = Vec::new();
        let mut ranges = from;
        for axis in 0..2 {
            let [low, high] = from[axis];
            let moved = match self.grab {
                Grab::Move => {
                    // Its middle to ahead, across; its foot to level, up.
                    let (anchor, targets) = if axis == 0 {
                        ((low + high) / 2., [0.])
                    } else {
                        (low, [0.])
                    };
                    let (to, held) = land(anchor + delta[axis], &targets, snapping);
                    let by = (to - anchor).clamp(-bound[axis] - low, bound[axis] - high);
                    if held.is_some() && (by - (to - anchor)).abs() < 1e-9 {
                        guides.push(Guide {
                            upright: axis == 0,
                            at: 0.,
                        });
                    }
                    [low + by, high + by]
                }
                Grab::Edges { turn, tilt } => {
                    let edge = [turn, tilt][axis];
                    if edge == 0 {
                        [low, high]
                    } else {
                        let (held_edge, other) = if edge < 0 { (low, high) } else { (high, low) };
                        // Ahead or level, and across, as far the other side
                        // of ahead as the edge that stays.
                        let targets = if axis == 0 && !centred {
                            vec![0., -other]
                        } else {
                            vec![0.]
                        };
                        let (to, held) = land(held_edge + delta[axis], &targets, snapping);
                        let other = if centred {
                            other - (to - held_edge)
                        } else {
                            other
                        };
                        let to = to.clamp(-bound[axis], bound[axis]);
                        let other = other.clamp(-bound[axis], bound[axis]);
                        if held.is_some() {
                            guides.push(Guide {
                                upright: axis == 0,
                                // Ahead, or level: an even split is about ahead.
                                at: 0.,
                            });
                        }
                        // An edge stops short of the one across from it.
                        if edge < 0 {
                            [to.min(other - LEAST), other]
                        } else {
                            [other, to.max(other + LEAST)]
                        }
                    }
                }
                Grab::Draw => {
                    let (to, held) = land(at[axis], &[0., -self.press[axis].round()], snapping);
                    let anchor = self.press[axis].round();
                    if held.is_some() {
                        guides.push(Guide {
                            upright: axis == 0,
                            at: 0.,
                        });
                    }
                    let (low, high) = (anchor.min(to), anchor.max(to));
                    [low, high.max(low + LEAST)]
                }
            };
            ranges[axis] = moved;
        }
        let window = Window {
            turn: ranges[0],
            tilt: ranges[1],
        };
        (window, guides)
    }
}

/// What the editor holds between frames.
#[derive(Default)]
pub(super) struct State {
    /// Where the graph was last drawn.
    bounds: std::rc::Rc<std::cell::RefCell<Option<Bounds<Pixels>>>>,
    drag: Option<Drag>,
    guides: Vec<Guide>,
}

impl Luma {
    /// Take hold of the window where the pointer went down.
    fn grab_laser_window(
        &mut self,
        venue_id: &str,
        grab: Grab,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(venue_id) else {
            return;
        };
        let (Some(bounds), Some(from)) = (
            *state.lasers.window.bounds.borrow(),
            state.data.as_ref().map(|data| data.lasers.rig.window()),
        ) else {
            return;
        };
        state.lasers.window.drag = Some(Drag {
            grab,
            from,
            press: angles(bounds, at),
        });
        cx.notify();
    }

    /// Follow the pointer with the window in hand: shown, and not yet
    /// written.
    fn drag_laser_window(
        &mut self,
        venue_id: &str,
        at: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(venue_id) else {
            return;
        };
        let (Some(drag), Some(bounds)) = (
            state.lasers.window.drag,
            *state.lasers.window.bounds.borrow(),
        ) else {
            return;
        };
        let (window, guides) =
            drag.window(angles(bounds, at), modifiers.alt, !modifiers.secondary());
        if window.check().is_err() {
            return;
        }
        state.lasers.window.guides = guides;
        self.set_laser_window(venue_id.to_owned(), Some(window), false, cx);
    }

    /// Let the window go where it is: written, as one step.
    fn drop_laser_window(&mut self, venue_id: &str, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(venue_id) else {
            return;
        };
        let Some(drag) = state.lasers.window.drag.take() else {
            return;
        };
        state.lasers.window.guides.clear();
        let now = state.data.as_ref().map(|data| data.lasers.rig.window());
        // A press that never moved changes nothing: a venue following its
        // projectors keeps following them.
        match now.filter(|now| *now != drag.from) {
            Some(window) => self.set_laser_window(venue_id.to_owned(), Some(window), true, cx),
            None => cx.notify(),
        }
    }
}

/// A line of text on the graph, at a place on it.
fn mark(text: impl Into<SharedString>, [x, y]: [f32; 2]) -> gpui::Div {
    div()
        .absolute()
        .left(relative(x))
        .top(relative(y))
        .text_size(px(10.0))
        .text_color(ladder::foreground_alpha(0.38))
        .child(text.into())
}

/// The graph, the window on it, and every projector's scan behind.
pub(super) fn editor(state: &Patch, lasers: &Lasers, app: &Entity<Luma>) -> AnyElement {
    let window = lasers.rig.window();
    let venue: SharedString = state.venue_id.clone().into();
    let dragging = state.lasers.window.drag.is_some();
    let guides = state.lasers.window.guides.clone();

    // Each projector's scan, as the directions its edge reaches.
    let scans: Vec<Vec<[f32; 2]>> = lasers
        .rig
        .projectors
        .iter()
        .map(|projector| {
            let side = |from: [f64; 2], to: [f64; 2]| {
                (0..=16).map(move |k| {
                    let t = f64::from(k) / 16.;
                    [
                        from[0] + (to[0] - from[0]) * t,
                        from[1] + (to[1] - from[1]) * t,
                    ]
                })
            };
            [
                ([-1., -1.], [1., -1.]),
                ([1., -1.], [1., 1.]),
                ([1., 1.], [-1., 1.]),
                ([-1., 1.], [-1., -1.]),
            ]
            .into_iter()
            .flat_map(|(from, to)| side(from, to))
            .map(|galvo| place(Window::angles(projector.direction(galvo))))
            .collect()
        })
        .collect();

    let measured = state.lasers.window.bounds.clone();
    let (moved, dropped, venue_move, venue_drop) =
        (app.clone(), app.clone(), venue.clone(), venue.clone());
    let picture = canvas(
        move |bounds, _, _| {
            *measured.borrow_mut() = Some(bounds);
        },
        move |bounds, (), window, _| {
            let at = |[x, y]: [f32; 2]| {
                point(
                    bounds.origin.x + bounds.size.width * x,
                    bounds.origin.y + bounds.size.height * y,
                )
            };
            let mut stroke = |points: &[[f32; 2]], width: f32, color: gpui::Hsla| {
                let mut path = PathBuilder::stroke(px(width));
                let Some(first) = points.first() else {
                    return;
                };
                path.move_to(at(*first));
                for corner in &points[1..] {
                    path.line_to(at(*corner));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            // The grid, ahead and level a little stronger.
            let lines = |[from, to]: [f64; 2]| {
                let first = (from / GRID).ceil() as i32;
                let last = (to / GRID).floor() as i32;
                (first..=last).map(|k| f64::from(k) * GRID)
            };
            for turn in lines(TURN) {
                let x = place([turn, 0.])[0];
                let ink = if turn == 0. { 0.2 } else { 0.07 };
                stroke(&[[x, 0.], [x, 1.]], 1.0, ladder::foreground_alpha(ink));
            }
            for tilt in lines(TILT) {
                let y = place([0., tilt])[1];
                let ink = if tilt == 0. { 0.2 } else { 0.07 };
                stroke(&[[0., y], [1., y]], 1.0, ladder::foreground_alpha(ink));
            }
            // A scan that looks away from the house runs off one side of
            // the graph and back in at the other: drawn as the runs it is
            // in, not joined across.
            for scan in &scans {
                for run in scan.chunk_by(|a, b| (a[0] - b[0]).abs() < 0.5) {
                    stroke(run, 1.0, ladder::foreground_alpha(0.3));
                }
            }
            for guide in &guides {
                let [x, y] = place([guide.at, guide.at]);
                let line = if guide.upright {
                    [[x, 0.], [x, 1.]]
                } else {
                    [[0., y], [1., y]]
                };
                stroke(&line, 1.0, ladder::accent().into());
            }
            // A drag follows the pointer wherever it goes, off the graph
            // too, and ends wherever the button comes up.
            if dragging {
                let (moved, venue) = (moved.clone(), venue_move.clone());
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.bubble() {
                        moved.update(cx, |this, cx| {
                            this.drag_laser_window(&venue, event.position, event.modifiers, cx);
                        });
                    }
                });
                let (dropped, venue) = (dropped.clone(), venue_drop.clone());
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase.bubble() && event.button == MouseButton::Left {
                        dropped.update(cx, |this, cx| this.drop_laser_window(&venue, cx));
                    }
                });
            }
        },
    )
    .absolute()
    .size_full();

    let [left, top] = place([window.turn[0], window.tilt[1]]);
    let [right, bottom] = place([window.turn[1], window.tilt[0]]);
    let grab = |grab: Grab| {
        let (app, venue) = (app.clone(), venue.clone());
        move |event: &gpui::MouseDownEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            cx.stop_propagation();
            app.update(cx, |this, cx| {
                this.grab_laser_window(&venue, grab, event.position, cx);
            });
        }
    };
    let body = div()
        .id("laser-window-body")
        .absolute()
        .left(relative(left))
        .top(relative(top))
        .w(relative((right - left).max(0.)))
        .h(relative((bottom - top).max(0.)))
        .rounded(px(2.0))
        .bg(gpui::Hsla::from(ladder::accent()).opacity(0.16))
        .border_1()
        .border_color(ladder::accent())
        .cursor(if dragging {
            CursorStyle::ClosedHand
        } else {
            CursorStyle::OpenHand
        })
        .on_mouse_down(MouseButton::Left, grab(Grab::Move))
        .agent_node(
            Role::Card,
            format!(
                "Laser window: turn {:.0} to {:.0}, tilt {:.0} to {:.0}",
                window.turn[0], window.turn[1], window.tilt[0], window.tilt[1]
            ),
        );
    // Eight handles: the corners and the middle of each edge.
    let handles = [-1i8, 0, 1].into_iter().flat_map(|turn| {
        [-1i8, 0, 1]
            .into_iter()
            .filter(move |tilt| turn != 0 || *tilt != 0)
            .map(move |tilt| (turn, tilt))
    });
    let handles: Vec<AnyElement> = handles
        .map(|(turn, tilt)| {
            let x = [left, (left + right) / 2., right][(turn + 1) as usize];
            let y = [bottom, (top + bottom) / 2., top][(tilt + 1) as usize];
            let cursor = match (turn, tilt) {
                (0, _) => CursorStyle::ResizeUpDown,
                (_, 0) => CursorStyle::ResizeLeftRight,
                (a, b) if a == b => CursorStyle::ResizeUpRightDownLeft,
                _ => CursorStyle::ResizeUpLeftDownRight,
            };
            let name = match (turn, tilt) {
                (-1, -1) => "bottom left",
                (-1, 0) => "left",
                (-1, 1) => "top left",
                (0, -1) => "bottom",
                (0, 1) => "top",
                (1, -1) => "bottom right",
                (1, 0) => "right",
                _ => "top right",
            };
            div()
                .id(SharedString::from(format!("laser-window-{turn}-{tilt}")))
                .absolute()
                .left(relative(x))
                .top(relative(y))
                .ml(px(-REACH / 2.))
                .mt(px(-REACH / 2.))
                .size(px(REACH))
                .flex()
                .items_center()
                .justify_center()
                .cursor(cursor)
                .child(
                    div()
                        .size(px(HANDLE))
                        .rounded(px(2.0))
                        .bg(ladder::background())
                        .border_1()
                        .border_color(ladder::accent()),
                )
                .on_mouse_down(MouseButton::Left, grab(Grab::Edges { turn, tilt }))
                .agent_node(Role::Button, format!("Laser window {name} handle"))
                .into_any_element()
        })
        .collect();

    let size = format!(
        "{:.0}° wide · {:.0}° tall",
        window.turn[1] - window.turn[0],
        window.tilt[1] - window.tilt[0]
    );
    let ticks = [-90., -45., 0., 45., 90.].map(|turn: f64| {
        let x = place([turn, 0.])[0];
        mark(format!("{turn:.0}°"), [x, 1.0])
            .mt(px(-16.0))
            .ml(px(if turn < 0. {
                4.0
            } else if turn > 80. {
                -24.0
            } else {
                4.0
            }))
    });
    div()
        .id("laser-window-graph")
        .relative()
        .flex_none()
        .w(px(WIDTH))
        .max_w_full()
        .h(px(HEIGHT))
        .rounded(px(luma_ui::radius::ROW))
        .bg(luma_ui::glass::ink(0.06))
        .overflow_hidden()
        .cursor(CursorStyle::Crosshair)
        .on_mouse_down(MouseButton::Left, grab(Grab::Draw))
        .child(picture)
        .children(ticks)
        .child(mark("Level", place([TURN[0], 0.])).ml(px(6.0)).mt(px(3.0)))
        .child(mark("Ahead", place([0., TILT[1]])).ml(px(5.0)).mt(px(4.0)))
        .child(body)
        .child(
            div()
                .absolute()
                .left(relative(left))
                .top(relative(top))
                .mt(px(-18.0))
                .text_size(px(10.5))
                .text_color(ladder::accent())
                .child(size.clone())
                .agent_node(Role::Text, format!("Laser window is {size}")),
        )
        .children(handles)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: Window = Window {
        turn: [-20., 30.],
        tilt: [5., 25.],
    };

    fn drag(grab: Grab, press: [f64; 2]) -> Drag {
        Drag {
            grab,
            from: FROM,
            press,
        }
    }

    /// Moved, a window keeps its size, lands on whole degrees, and is drawn
    /// to the middle and to level when it comes near them.
    #[test]
    fn a_move_keeps_the_size_and_finds_the_middle() {
        let moving = drag(Grab::Move, [0., 10.]);
        let (window, guides) = moving.window([7.3, 14.4], false, true);
        assert_eq!(window.turn, [-13., 37.]);
        assert_eq!(window.tilt, [9., 29.]);
        assert!(guides.is_empty());

        // Its middle is at 5: four degrees left is one short of ahead.
        let (window, guides) = moving.window([-4., 10.], false, true);
        assert_eq!(window.turn, [-25., 25.], "drawn to the middle");
        assert_eq!(guides.len(), 1);
        let (window, _) = moving.window([-4., 10.], false, false);
        assert_eq!(
            window.turn,
            [-24., 26.],
            "let go of, it is where it was put"
        );

        // Its foot is at 5: four down is one short of level.
        let (window, _) = moving.window([0., 6.], false, true);
        assert_eq!(window.tilt, [0., 20.]);
    }

    /// An edge moves alone, toward an even split either side of ahead, and
    /// about the middle the edge across from it moves as far the other way.
    #[test]
    fn an_edge_resizes_and_evens_the_split() {
        let right = drag(Grab::Edges { turn: 1, tilt: 0 }, [30., 10.]);
        let (window, _) = right.window([41.2, 50.], false, true);
        assert_eq!(window.turn, [-20., 41.]);
        assert_eq!(window.tilt, FROM.tilt, "the other way is untouched");

        let (window, guides) = right.window([21., 10.], false, true);
        assert_eq!(window.turn, [-20., 20.], "as far right as it is left");
        assert_eq!(guides.len(), 1);

        let (window, _) = right.window([40., 10.], true, true);
        assert_eq!(window.turn, [-30., 40.], "about its middle");

        // It stops short of the edge across from it.
        let (window, _) = right.window([-60., 10.], false, false);
        assert!(window.turn[1] > window.turn[0]);
        assert!(window.check().is_ok());
    }

    /// A drag on the empty graph draws a window between its two ends,
    /// whichever way it was dragged.
    #[test]
    fn a_drag_on_the_graph_draws_a_window() {
        let drawing = drag(Grab::Draw, [10., 30.]);
        let (window, _) = drawing.window([-25.4, 12.2], false, false);
        assert_eq!(window.turn, [-25., 10.]);
        assert_eq!(window.tilt, [12., 30.]);
    }

    /// The graph's angles and its places are each other's inverse.
    #[test]
    fn a_place_on_the_graph_is_its_angles() {
        let bounds = Bounds {
            origin: point(px(100.), px(50.)),
            size: gpui::size(px(WIDTH), px(HEIGHT)),
        };
        for angle in [[0., 0.], [-90., 60.], [45., -30.], [12., 33.]] {
            let [x, y] = place(angle);
            let at = point(
                bounds.origin.x + bounds.size.width * x,
                bounds.origin.y + bounds.size.height * y,
            );
            let back = angles(bounds, at);
            assert!((back[0] - angle[0]).abs() < 1e-3 && (back[1] - angle[1]).abs() < 1e-3);
        }
    }
}
