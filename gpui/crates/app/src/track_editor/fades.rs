//! A clip's alpha on the timeline, the way a DAW shows clip fades.
//!
//! Alpha is how much a clip counts. The timeline draws it as a line across
//! the clip body and dims the body above the line. Handles on the line edit
//! it: a fade-in and a fade-out handle at the top corners, a bend handle in
//! the middle of each fade, and the flat part of the line for the level.
//!
//! The handles edit the output node's `alpha` input, the clip's opacity,
//! which the clip sheet shows too: a value, or a curve over the clip
//! (`curve(time(), shape)`, the time with no events, delay or phase). A
//! simple fade shape is a [`Fades`]. Any other such curve is drawn but has
//! no handles; the sheet's curve editor edits it. An alpha wired any other
//! way has no one line to draw.

use super::*;
use luma_patterns as p;
use p::clip_graph::{ClipGraph, Input, Kind, Node};

/// The input every output node has.
const ALPHA: &str = "alpha";

/// An alpha the timeline writes: a level, or a curve over the clip whose
/// values are the alpha itself.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Level {
    Flat(f64),
    Curve(p::Envelope),
}

/// The gap between the body's edges and the line at alpha 1 and alpha 0.
const INSET: f32 = 3.;
/// How far from a handle's centre a press still takes it.
const GRAB: f32 = 6.;
/// How far from the flat line a press still takes the level.
const LINE_GRAB: f32 = 4.;
/// The drawn size of a handle.
const MARK: f32 = 6.;
/// A fade narrower than this on screen has no bend handle.
const BEND_MIN: f32 = 16.;
/// A body shorter than this has no handles; the line still shows.
const BODY_MIN: f32 = 12.;
/// A clip narrower than this has no handles.
const WIDTH_MIN: f32 = 24.;
/// Fade lengths and levels closer than this count as equal.
const EPSILON: f64 = 1e-9;

/// A fade's ease `raised` by `by`, a share of the fade's change: both
/// handles move up, kept within the fade. A hold stays a hold, and handles
/// back on the line are a straight fade again.
fn raised(ease: p::Ease, by: f64) -> p::Ease {
    let Some([x1, y1, x2, y2]) = ease.handles() else {
        return ease;
    };
    let lift = |y: f64| (y + by).clamp(0., 1.);
    let raised = [x1, lift(y1), x2, lift(y2)];
    let line = p::Ease::Linear.handles().unwrap();
    if raised
        .iter()
        .zip(line)
        .all(|(a, b)| (a - b).abs() <= EPSILON)
    {
        p::Ease::Linear
    } else {
        p::Ease::Bezier(raised)
    }
}

/// A simple fade shape: rise from 0 to `level`, hold, fall back to 0.
/// Fade lengths are shares of the clip, 0–1, and never sum past 1. Each fade
/// has an ease, local to the fade, so its shape keeps when the fade or the
/// level changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fades {
    pub level: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub bend_in: p::Ease,
    pub bend_out: p::Ease,
}

impl Fades {
    pub fn flat(level: f64) -> Self {
        Self {
            level: level.clamp(0., 1.),
            fade_in: 0.,
            fade_out: 0.,
            bend_in: p::Ease::Linear,
            bend_out: p::Ease::Linear,
        }
    }

    /// The fade shape `curve` draws, or `None` for any other curve.
    pub fn of_curve(curve: &p::Envelope) -> Option<Self> {
        if curve.points.is_empty() {
            return None;
        }
        let points: Vec<(f64, f64)> = curve
            .points
            .iter()
            .map(|point| (point.x, point.value))
            .collect();
        let zero = |v: f64| v.abs() <= EPSILON;
        let last = points.len() - 1;
        let (mut first, mut end) = (0, last);
        let mut fades = Self::flat(points[0].1);
        if last >= 1 && zero(points[0].1) && points[1].1 > EPSILON && curve.ease(0) != p::Ease::Hold
        {
            fades.fade_in = points[1].0;
            fades.bend_in = curve.ease(0);
            first = 1;
        }
        if end > first
            && zero(points[end].1)
            && points[end - 1].1 > EPSILON
            && curve.ease(end - 1) != p::Ease::Hold
        {
            fades.fade_out = 1. - points[end - 1].0;
            fades.bend_out = curve.ease(end - 1);
            end -= 1;
        }
        let level = points[first].1;
        if !(0. ..=1.).contains(&level)
            || points[first..=end]
                .iter()
                .any(|(_, v)| (v - level).abs() > EPSILON)
        {
            return None;
        }
        fades.level = level;
        Some(fades)
    }

    /// The stored alpha: a plain level when there is no fade, a curve over
    /// the clip otherwise.
    pub fn value(self) -> Level {
        let level = self.level.clamp(0., 1.);
        let fade_in = self.fade_in.clamp(0., 1.);
        let fade_out = self.fade_out.clamp(0., 1. - fade_in);
        if fade_in <= EPSILON && fade_out <= EPSILON {
            return Level::Flat(level);
        }
        let mut points = Vec::with_capacity(4);
        let mut eases = Vec::with_capacity(3);
        if fade_in > EPSILON {
            points.push([0., 0.]);
            eases.push(self.bend_in);
            points.push([fade_in, level]);
        } else {
            points.push([0., level]);
        }
        let hold = 1. - fade_out;
        if fade_out > EPSILON {
            if hold > points.last().unwrap()[0] + EPSILON {
                eases.push(p::Ease::Linear);
                points.push([hold, level]);
            }
            eases.push(self.bend_out);
            points.push([1., 0.]);
        } else if points.last().unwrap()[0] < 1. - EPSILON {
            eases.push(p::Ease::Linear);
            points.push([1., level]);
        }
        Level::Curve(p::Envelope::eased(points, &eases))
    }

    /// Set the fade-in, leaving the fade-out room.
    pub fn with_fade_in(self, share: f64) -> Self {
        Self {
            fade_in: share.clamp(0., 1. - self.fade_out),
            ..self
        }
    }

    /// Set the fade-out, leaving the fade-in room.
    pub fn with_fade_out(self, share: f64) -> Self {
        Self {
            fade_out: share.clamp(0., 1. - self.fade_in),
            ..self
        }
    }
}

/// A clip's alpha, as far as the timeline can show it.
pub(super) enum Alpha {
    /// A plain value or a simple fade shape: drawn, with handles.
    Fades(Fades),
    /// Any other number curve over the clip: drawn, no handles.
    Custom(p::Envelope),
}

impl Alpha {
    fn sample(&self, progress: f64) -> f64 {
        self.curve().sample(progress)
    }
}

/// The curve an alpha of `graph`'s output `out` is wired to, when the
/// timeline may edit it: a number curve over the clip — `x` a time with no
/// `every` or `duration` and no delay or phase — that nothing else shares.
fn over_clip<'a>(graph: &'a ClipGraph, out: &str) -> Option<(&'a str, &'a Node)> {
    let id = graph.nodes.get(out)?.inputs.get(ALPHA)?.source()?;
    let curve = graph.nodes.get(id)?;
    let shared = graph
        .nodes
        .iter()
        .flat_map(|(at, node)| node.wires().map(move |(name, from)| (at, name, from)))
        .any(|(at, name, from)| from == id && (at != out || name != ALPHA));
    if curve.kind != Kind::Curve || curve.setting("kind") != Some("number") || shared {
        return None;
    }
    let time = graph.nodes.get(curve.inputs.get("x")?.source()?)?;
    let zero = |name: &str| match time.inputs.get(name) {
        None => true,
        Some(Input::Number(v)) => *v == 0.,
        Some(_) => false,
    };
    let once = !time.inputs.contains_key("every") && !time.inputs.contains_key("duration");
    (time.kind == Kind::Time && once && zero("delay") && zero("phase")).then_some((id, curve))
}

/// The alpha of a clip. `None` for an alpha that is noise, audio, space or
/// a per-hit curve: those change on their own and have no one line to draw.
pub(super) fn alpha(clip: &Clip) -> Option<Alpha> {
    alpha_of(&clip.core.as_ref()?.graph)
}

fn alpha_of(graph: &ClipGraph) -> Option<Alpha> {
    let (out, node) = graph.output()?;
    match node.inputs.get(ALPHA) {
        None => return Some(Alpha::Fades(Fades::flat(1.))),
        Some(Input::Number(v)) => return Some(Alpha::Fades(Fades::flat(*v))),
        _ => {}
    }
    let (_, curve) = over_clip(graph, out)?;
    let bound = |name: &str, empty: f64| match curve.inputs.get(name) {
        None => Some(empty),
        Some(Input::Number(v)) => Some(*v),
        Some(_) => None,
    };
    let (low, high) = (bound("low", 0.)?, bound("high", 1.)?);
    let shape = match curve.inputs.get("shape") {
        None => p::Envelope::linear(vec![[0., 0.], [1., 1.]]),
        Some(Input::Points(points)) => points.clone(),
        Some(_) => return None,
    };
    let line = shape.map(|v| low + v * (high - low));
    Some(match Fades::of_curve(&line) {
        Some(fades) => Alpha::Fades(fades),
        None => Alpha::Custom(line),
    })
}

/// Write `value` as `clip`'s alpha in the working copy. `false` when it is
/// already stored, so an idle drag records no edit.
pub(super) fn store(clips: &mut [Clip], id: &str, value: &Level) -> bool {
    clips
        .iter_mut()
        .find(|clip| clip.id.as_ref() == id)
        .is_some_and(|clip| set(clip, value))
}

/// Write `value` as the alpha of `graph`'s output: a level as a value, a
/// curve into the curve over the clip already there, or a new time and
/// curve.
fn write(graph: &mut ClipGraph, value: &Level) {
    let Some(out) = graph.output().map(|(id, _)| id.to_owned()) else {
        return;
    };
    let points = match value {
        Level::Flat(level) => {
            if let Some(node) = graph.nodes.get_mut(&out) {
                node.inputs.insert(ALPHA.into(), Input::Number(*level));
            }
            super::sheet::graph::edit::prune(graph);
            return;
        }
        Level::Curve(points) => Input::Points(points.clone()),
    };
    if let Some(id) = over_clip(graph, &out).map(|(id, _)| id.to_owned()) {
        let curve = graph.nodes.get_mut(&id).expect("the alpha curve");
        curve.inputs.insert("shape".into(), points);
        curve.inputs.remove("low");
        curve.inputs.remove("high");
        return;
    }
    let time = graph.next_id(Kind::Time);
    graph.nodes.insert(time.clone(), Node::new(Kind::Time));
    let curve = graph.next_id(Kind::Curve);
    graph.nodes.insert(
        curve.clone(),
        Node::new(Kind::Curve)
            .with_setting("kind", "number")
            .with_input("x", Input::wire(time))
            .with_input("shape", points),
    );
    if let Some(node) = graph.nodes.get_mut(&out) {
        node.inputs.insert(ALPHA.into(), Input::wire(curve));
    }
    super::sheet::graph::edit::prune(graph);
}

fn set(clip: &mut Clip, value: &Level) -> bool {
    let Some(core) = clip.core.as_mut() else {
        return false;
    };
    let mut graph = core.graph.clone();
    write(&mut graph, value);
    if graph == core.graph {
        return false;
    }
    core.graph = graph;
    clip.refresh();
    true
}

/// `clip`'s fade shape when it has a fade. A custom curve has none.
pub(super) fn faded(clip: &Clip) -> Option<Fades> {
    match alpha(clip)? {
        Alpha::Fades(fades) if fades.fade_in > EPSILON || fades.fade_out > EPSILON => Some(fades),
        _ => None,
    }
}

/// After a resize, give `clip` the fades it had over `length` seconds, at
/// the same lengths in time: a DAW's fades do not stretch with the clip.
/// They shrink to fit when the clip gets too short, the fade-in first.
pub(super) fn refit(clip: &mut Clip, fades: Fades, length: f64) {
    let now = (clip.end - clip.start).max(EPSILON);
    let (fade_in, fade_out) = (fades.fade_in * length, fades.fade_out * length);
    let refit = Fades {
        fade_in: 0.,
        fade_out: 0.,
        ..fades
    }
    .with_fade_in(fade_in / now)
    .with_fade_out(fade_out / now);
    set(clip, &refit.value());
}

/// Which part of the alpha line a press took hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    FadeIn,
    FadeOut,
    BendIn,
    BendOut,
    /// One segment of the line, between two of its points.
    Segment(usize),
}

impl Part {
    fn label(self) -> String {
        match self {
            Self::FadeIn => "fade in".into(),
            Self::FadeOut => "fade out".into(),
            Self::BendIn => "fade in bend".into(),
            Self::BendOut => "fade out bend".into(),
            Self::Segment(index) => format!("fade {}", index + 1),
        }
    }

    /// The pointer over this part, or dragging it.
    pub fn cursor(self, dragging: bool) -> CursorStyle {
        match self {
            Self::FadeIn | Self::FadeOut => CursorStyle::ResizeLeftRight,
            Self::BendIn | Self::BendOut if dragging => CursorStyle::ClosedHand,
            Self::BendIn | Self::BendOut => CursorStyle::OpenHand,
            Self::Segment(_) => CursorStyle::ResizeUpDown,
        }
    }
}

/// What a press took hold of, and the alpha as it was then.
#[derive(Clone, Debug)]
pub(super) struct Grab {
    pub part: Part,
    /// The line as a curve: a plain value is one flat segment.
    curve: p::Envelope,
    /// The fade shape, when the line is one.
    fades: Option<Fades>,
}

impl Alpha {
    /// The line as a curve over the clip.
    fn curve(&self) -> p::Envelope {
        match self {
            Self::Fades(fades) => match fades.value() {
                Level::Curve(curve) => curve,
                Level::Flat(level) => p::Envelope::linear(vec![[0., level], [1., level]]),
            },
            Self::Custom(curve) => curve.clone(),
        }
    }
}

/// Where the line sits inside a clip's box, in window pixels.
#[derive(Clone, Copy)]
struct Frame {
    left: f32,
    width: f32,
    top: f32,
    height: f32,
    body: Bounds<Pixels>,
}

impl Frame {
    fn of(box_: Bounds<Pixels>) -> Self {
        let body = clip_body(box_);
        Self {
            left: f32::from(body.origin.x),
            width: f32::from(body.size.width),
            top: f32::from(body.origin.y) + INSET,
            height: (f32::from(body.size.height) - 2. * INSET).max(1.),
            body,
        }
    }
    fn x(self, progress: f64) -> f32 {
        self.left + progress as f32 * self.width
    }
    fn y(self, value: f64) -> f32 {
        self.top + (1. - value.clamp(0., 1.) as f32) * self.height
    }
    fn editable(self) -> bool {
        self.width >= WIDTH_MIN && f32::from(self.body.size.height) >= BODY_MIN
    }
}

/// The handles a fade shape shows, with their centres.
fn handles(frame: Frame, fades: Fades) -> Vec<(Part, Point<f32>)> {
    if !frame.editable() {
        return Vec::new();
    }
    let top = frame.y(fades.level);
    // A bend handle sits on the line, halfway along its fade.
    let curve = match fades.value() {
        Level::Curve(curve) => Some(curve),
        Level::Flat(_) => None,
    };
    let on_line = |x: f64| {
        frame.y(curve
            .as_ref()
            .map_or(fades.level / 2., |curve| curve.sample(x)))
    };
    let mut handles = vec![
        (Part::FadeIn, point(frame.x(fades.fade_in), top)),
        (Part::FadeOut, point(frame.x(1. - fades.fade_out), top)),
    ];
    if fades.fade_in as f32 * frame.width >= BEND_MIN {
        let x = fades.fade_in / 2.;
        handles.push((Part::BendIn, point(frame.x(x), on_line(x))));
    }
    if fades.fade_out as f32 * frame.width >= BEND_MIN {
        let x = 1. - fades.fade_out / 2.;
        handles.push((Part::BendOut, point(frame.x(x), on_line(x))));
    }
    handles
}

/// What part of `clip`'s alpha line, drawn in `box_`, is under `at`. Fade
/// handles first, then any segment of the line.
pub(super) fn hit(box_: Bounds<Pixels>, clip: &Clip, at: Point<Pixels>) -> Option<Grab> {
    let frame = Frame::of(box_);
    let (x, y) = (f32::from(at.x), f32::from(at.y));
    if !frame.editable()
        || x < frame.left - GRAB
        || x > frame.left + frame.width + GRAB
        || y < f32::from(frame.body.origin.y)
        || y > f32::from(frame.body.bottom())
    {
        return None;
    }
    let alpha = alpha(clip)?;
    let fades = match &alpha {
        Alpha::Fades(fades) => Some(*fades),
        Alpha::Custom(_) => None,
    };
    let curve = alpha.curve();
    let grab = |part| {
        Some(Grab {
            part,
            curve: curve.clone(),
            fades,
        })
    };
    let near = |c: Point<f32>| (c.x - x).abs() <= GRAB && (c.y - y).abs() <= GRAB;
    if let Some((part, _)) = fades
        .map(|fades| handles(frame, fades))
        .unwrap_or_default()
        .into_iter()
        .find(|(_, c)| near(*c))
    {
        return grab(part);
    }
    let progress = f64::from((x - frame.left) / frame.width);
    if (y - frame.y(curve.sample(progress.clamp(0., 1.)))).abs() > LINE_GRAB {
        return None;
    }
    let index = curve
        .points
        .windows(2)
        .position(|pair| (pair[0].x..=pair[1].x).contains(&progress))?;
    grab(Part::Segment(index))
}

/// The alpha a drag leaves, from `grab` at the press. `span` is the clip's
/// seconds, `time` the pointer's time, snapped; `rise` is how far the pointer
/// went up and `height` the line's travel from 0 to 1, in pixels.
pub(super) fn moved(grab: &Grab, span: (f64, f64), time: f64, rise: f32, height: f32) -> Level {
    match (grab.part, grab.fades) {
        (Part::Segment(index), _) => lift(&grab.curve, index, f64::from(rise / height.max(1.))),
        (part, Some(fades)) => {
            dragged(part, fades, span, time, f64::from(rise / height.max(1.))).value()
        }
        (_, None) => Level::Curve(grab.curve.clone()),
    }
}

/// `curve` with segment `index` moved up by `by`: both of its points, each
/// kept in 0–1. Eases are local to their segments, so every segment keeps
/// its shape. A line that is a fade shape again is stored as one.
pub(super) fn lift(curve: &p::Envelope, index: usize, by: f64) -> Level {
    let mut curve = curve.clone();
    for point in curve.points.iter_mut().skip(index).take(2) {
        point.value = (point.value + by).clamp(0., 1.);
    }
    match Fades::of_curve(&curve) {
        Some(fades) => fades.value(),
        None => Level::Curve(curve),
    }
}

/// A drag of a fade handle, from `fades` as they were at the press. `time`
/// is the pointer's time, snapped; `rise` is how far the pointer went up, in
/// alpha. A bend follows the pointer: the middle of a cubic moves 3/4 of
/// what its two handles move. A fade-out's ease runs downward, so raising
/// it lowers its handles.
pub(super) fn dragged(part: Part, fades: Fades, span: (f64, f64), time: f64, rise: f64) -> Fades {
    let length = (span.1 - span.0).max(EPSILON);
    match part {
        Part::FadeIn => fades.with_fade_in((time - span.0) / length),
        Part::FadeOut => fades.with_fade_out((span.1 - time) / length),
        Part::BendIn | Part::BendOut if fades.level > EPSILON => {
            let by = rise / fades.level / 0.75;
            if part == Part::BendIn {
                Fades {
                    bend_in: raised(fades.bend_in, by),
                    ..fades
                }
            } else {
                Fades {
                    bend_out: raised(fades.bend_out, -by),
                    ..fades
                }
            }
        }
        Part::BendIn | Part::BendOut => fades,
        Part::Segment(_) => fades,
    }
}

/// The line's travel from alpha 0 to 1 for a clip drawn in `box_`.
pub(super) fn travel(box_: Bounds<Pixels>) -> f32 {
    Frame::of(box_).height
}

/// Set the fades two overlapping clips need to cross. `left` starts first and
/// `right` starts inside it and ends after it. Each gets a fade over the
/// overlap: a fade-out on `left`, a fade-in on `right`. A clip whose alpha is
/// not a fade shape is left alone.
pub(super) fn crossfade(clips: &mut [Clip], left: &str, right: &str) -> bool {
    let span = |id: &str| {
        clips
            .iter()
            .find(|clip| clip.id.as_ref() == id)
            .map(|clip| (clip.start, clip.end))
    };
    let (Some(a), Some(b)) = (span(left), span(right)) else {
        return false;
    };
    let overlap = a.1 - b.0;
    if !(a.0 < b.0 && b.0 < a.1 && a.1 < b.1) || overlap <= 0. {
        return false;
    }
    let fades = |id: &str| {
        clips
            .iter()
            .find(|clip| clip.id.as_ref() == id)
            .and_then(alpha)
            .and_then(|alpha| match alpha {
                Alpha::Fades(fades) => Some(fades),
                Alpha::Custom(_) => None,
            })
    };
    let (Some(out), Some(into)) = (fades(left), fades(right)) else {
        return false;
    };
    let out = out.with_fade_out(overlap / (a.1 - a.0)).value();
    let into = into.with_fade_in(overlap / (b.1 - b.0)).value();
    let changed = store(clips, left, &out);
    store(clips, right, &into) || changed
}

/// Draw `clip`'s alpha line in `box_`: dim the body above it, stroke it, and
/// mark its handles.
pub(super) fn paint(box_: Bounds<Pixels>, clip: &Clip, selected: bool, window: &mut Window) {
    let Some(alpha) = alpha(clip) else {
        return;
    };
    let frame = Frame::of(box_);
    if frame.width < 2. || f32::from(frame.body.size.height) < 4. {
        return;
    }
    let line = outline(frame, &alpha);
    let at = |p: Point<f32>| point(px(p.x), px(p.y));
    let body_top = f32::from(frame.body.origin.y);
    let mut shade = PathBuilder::fill();
    shade.move_to(at(point(frame.left, body_top)));
    shade.line_to(at(point(frame.left + frame.width, body_top)));
    for p in line.iter().rev() {
        shade.line_to(at(*p));
    }
    shade.close();
    if let Ok(path) = shade.build() {
        window.paint_path(path, fade(ladder::background(), 0.4));
    }
    let mut stroke = PathBuilder::stroke(px(if selected { 1.5 } else { 1. }));
    stroke.move_to(at(line[0]));
    for p in &line[1..] {
        stroke.line_to(at(*p));
    }
    if let Ok(path) = stroke.build() {
        window.paint_path(
            path,
            fade(ladder::foreground(), if selected { 0.9 } else { 0.6 }),
        );
    }
    let Alpha::Fades(fades) = alpha else {
        return;
    };
    for (_, centre) in handles(frame, fades) {
        let mark = Bounds {
            origin: point(px(centre.x - MARK / 2.), px(centre.y - MARK / 2.)),
            size: size(px(MARK), px(MARK)),
        };
        window.paint_quad(quad(
            mark,
            Corners::all(px(MARK / 2.)),
            fade(ladder::foreground(), if selected { 0.95 } else { 0.7 }),
            Edges::all(px(1.)),
            fade(ladder::background(), 0.6),
            BorderStyle::Solid,
        ));
    }
}

/// The line's points across the clip, left to right.
fn outline(frame: Frame, alpha: &Alpha) -> Vec<Point<f32>> {
    // Every key, and each side of it, so holds keep their corners;
    // and enough even samples between them for bends to read as curves.
    let mut xs: Vec<f64> = match alpha {
        Alpha::Fades(fades) => match fades.value() {
            Level::Curve(curve) => curve.points.iter().map(|point| point.x).collect(),
            Level::Flat(_) => Vec::new(),
        },
        Alpha::Custom(curve) => curve.points.iter().map(|point| point.x).collect(),
    };
    let steps = (frame.width / 3.).clamp(8., 256.) as usize;
    xs.extend((0..=steps).map(|i| i as f64 / steps as f64));
    xs.retain(|x| (0. ..=1.).contains(x));
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() <= EPSILON);
    let mut line = Vec::with_capacity(xs.len() * 2);
    for x in xs {
        let before = alpha.sample((x - 1e-7).max(0.));
        let after = alpha.sample(x);
        line.push(point(frame.x(x), frame.y(before)));
        if (after - before).abs() > EPSILON {
            line.push(point(frame.x(x), frame.y(after)));
        }
    }
    line
}

/// Name each handle for a script: `<clip> fade in`, `<clip> fade out`, the
/// two bends, and `<clip> alpha N` for the Nth segment of the line, placed a
/// fifth of the way along it, clear of the handles.
pub(super) fn register(box_: Bounds<Pixels>, clip: &Clip, window: &mut Window, cx: &mut App) {
    let Some(alpha) = alpha(clip) else {
        return;
    };
    let frame = Frame::of(box_);
    if !frame.editable() {
        return;
    }
    let square = |c: Point<f32>| Bounds {
        origin: point(px(c.x - GRAB / 2.), px(c.y - GRAB / 2.)),
        size: size(px(GRAB), px(GRAB)),
    };
    if let Alpha::Fades(fades) = alpha {
        for (part, centre) in handles(frame, fades) {
            agent_paint_node(
                Role::Slider,
                format!("{} {}", clip.label, part.label()),
                square(centre),
                window,
                cx,
            );
        }
    }
    let curve = alpha.curve();
    for (index, pair) in curve.points.windows(2).enumerate() {
        let at = pair[0].x + (pair[1].x - pair[0].x) * 0.2;
        agent_paint_node(
            Role::Slider,
            format!("{} {}", clip.label, Part::Segment(index).label()),
            square(point(frame.x(at), frame.y(curve.sample(at)))),
            window,
            cx,
        );
    }
}

impl Editor {
    /// The part of an alpha line under `at`, in window space.
    fn alpha_under(&self, at: Point<Pixels>) -> Option<(&Clip, Bounds<Pixels>, Grab)> {
        if !self.writable() {
            return None;
        }
        let (canvas, layout, view) = (self.canvas.get(), self.layout(), self.view);
        let row = layout.row_at(f32::from(at.y - canvas.origin.y))?;
        self.clips
            .iter()
            .filter(|clip| clip.row == row)
            .find_map(|clip| {
                let box_ = clip_bounds(view, layout, canvas, clip);
                hit(box_, clip, at).map(|grab| (clip, box_, grab))
            })
    }

    /// Follow the pointer over the alpha lines, for the cursor. `true` when
    /// what it is over changed.
    pub(super) fn hover_alpha(&mut self, at: Point<Pixels>) -> bool {
        let over = self.alpha_under(at).map(|(_, _, grab)| grab.part);
        let changed = over != self.alpha_hover;
        self.alpha_hover = over;
        changed
    }

    /// Take hold of a clip's alpha line if the press at `at` is on it.
    /// Selects the clip.
    pub(super) fn press_alpha(&mut self, at: Point<Pixels>) -> bool {
        let Some((clip, box_, grab)) = self.alpha_under(at) else {
            return false;
        };
        let (id, row, start, end) = (clip.id.clone(), clip.row, clip.start, clip.end);
        let travel = travel(box_);
        self.selected = vec![id.clone()];
        self.cursor = Some(Cursor {
            row,
            row_end: None,
            start,
            end: Some(end),
        });
        // The point an undo comes back to; dropped on release if nothing
        // changed.
        self.checkpoint();
        self.gesture = Some(Gesture::Alpha {
            clip: id,
            grab,
            origin: at,
            travel,
        });
        true
    }

    /// Follow an alpha drag to the pointer at `at`. Measured from the press,
    /// not from the canvas: selecting the clip opens the sheet, and that can
    /// move the canvas under the pointer.
    pub(super) fn drag_alpha(&mut self, gesture: &Gesture, at: Point<Pixels>) {
        let Gesture::Alpha {
            clip,
            grab,
            origin,
            travel,
        } = gesture
        else {
            return;
        };
        let Some(span) = self
            .clips
            .iter()
            .find(|held| &held.id == clip)
            .map(|held| (held.start, held.end))
        else {
            return;
        };
        let zoom = self.view.zoom;
        let fades = grab.fades.unwrap_or(Fades::flat(1.));
        let grabbed = match grab.part {
            Part::FadeOut => span.1 - fades.fade_out * (span.1 - span.0),
            _ => span.0 + fades.fade_in * (span.1 - span.0),
        };
        let time = grabbed + f64::from(f32::from(at.x - origin.x) / zoom);
        let mut time = snap(self.beats.as_deref(), time, zoom, SNAP_CAPTURE_DRAG);
        // A fade pulled back to its own edge is gone, whatever the grid says.
        for edge in [span.0, span.1] {
            if ((time - edge) * f64::from(zoom)).abs() < f64::from(SNAP_CAPTURE_DRAG) {
                time = edge;
            }
        }
        let rise = f32::from(origin.y - at.y);
        let value = moved(grab, span, time, rise, *travel);
        let mut clips = self.clips.to_vec();
        if store(&mut clips, clip, &value) {
            self.replace_clips(clips);
        }
    }

    /// After a move: where a moved clip now overlaps the end or the start of
    /// another clip in its layer, and did not before, cross the two with
    /// fades over the overlap. Returns the clips it changed.
    pub(super) fn crossfade(&mut self, initial: &[Initial]) -> Vec<SharedString> {
        let mut clips = self.clips.to_vec();
        let mut touched: Vec<SharedString> = Vec::new();
        for held in initial {
            let Some(moved) = self.clips.iter().find(|clip| clip.id == held.id) else {
                continue;
            };
            for other in self.clips.iter() {
                if other.z != moved.z || initial.iter().any(|was| was.id == other.id) {
                    continue;
                }
                if held.start < other.end && other.start < held.end {
                    continue;
                }
                let (left, right) = if other.start < moved.start {
                    (other, moved)
                } else {
                    (moved, other)
                };
                if crossfade(&mut clips, &left.id, &right.id) {
                    for id in [&left.id, &right.id] {
                        if !touched.contains(id) {
                            touched.push(id.clone());
                        }
                    }
                }
            }
        }
        if !touched.is_empty() {
            self.replace_clips(clips);
        }
        touched
    }
}

#[cfg(test)]
mod tests {
    use super::{alpha_of, dragged, lift, write, Alpha, Fades, Level, Part};
    use luma_patterns as p;
    use p::clip_graph::{ClipGraph, Kind, Node};

    fn curve(value: Level) -> p::Envelope {
        match value {
            Level::Curve(curve) => curve,
            other => panic!("expected a curve, got {other:?}"),
        }
    }

    fn numbers(points: &[[f64; 2]], eases: &[p::Ease]) -> p::Envelope {
        p::Envelope::eased(points.to_vec(), eases)
    }

    #[test]
    fn no_fade_is_a_plain_value() {
        assert_eq!(Fades::flat(0.7).value(), Level::Flat(0.7));
    }

    #[test]
    fn a_fade_in_rises_holds_and_reads_back() {
        let fades = Fades::flat(0.8).with_fade_in(0.25);
        let curve = curve(fades.value());
        assert_eq!(curve, numbers(&[[0., 0.], [0.25, 0.8], [1., 0.8]], &[]));
        assert_eq!(Fades::of_curve(&curve), Some(fades));
    }

    fn same(a: Fades, b: Fades) -> bool {
        let near = |x: f64, y: f64| (x - y).abs() < 1e-9;
        let bend = |a: p::Ease, b: p::Ease| match (a.handles(), b.handles()) {
            (Some(a), Some(b)) => a.iter().zip(b).all(|(x, y)| near(*x, y)),
            _ => a == b,
        };
        near(a.level, b.level)
            && near(a.fade_in, b.fade_in)
            && near(a.fade_out, b.fade_out)
            && bend(a.bend_in, b.bend_in)
            && bend(a.bend_out, b.bend_out)
    }

    #[test]
    fn both_fades_with_bends_round_trip() {
        let fades = Fades {
            bend_in: p::Ease::EaseInOut,
            ..Fades::flat(1.).with_fade_in(0.2).with_fade_out(0.3)
        };
        let curve = curve(fades.value());
        assert_eq!(curve.points.len(), 4);
        assert_eq!(curve.ease(0), p::Ease::EaseInOut);
        assert_eq!(
            [curve.ease(1), curve.ease(2)],
            [p::Ease::Linear, p::Ease::Linear]
        );
        assert!((curve.sample(0.1) - 0.5).abs() < 1e-9);
        assert!((curve.sample(0.85) - 0.5).abs() < 1e-9);
        let back = Fades::of_curve(&curve).unwrap();
        assert!(same(back, fades), "{back:?}");
    }

    #[test]
    fn fades_never_overlap() {
        let fades = Fades::flat(1.).with_fade_in(0.7).with_fade_out(0.6);
        assert!((fades.fade_out - 0.3).abs() < 1e-12);
        // Meeting fades share their peak point.
        assert_eq!(curve(fades.value()).points.len(), 3);
    }

    #[test]
    fn shipped_ramps_and_swells_are_fades() {
        let ramp = numbers(&[[0., 0.], [1., 1.]], &[]);
        let fades = Fades::of_curve(&ramp).unwrap();
        assert_eq!((fades.fade_in, fades.fade_out, fades.level), (1., 0., 1.));
        let swell = p::presets().curve("Swell").unwrap();
        let fades = Fades::of_curve(swell).unwrap();
        assert_eq!((fades.fade_in, fades.fade_out), (0.5, 0.5));
        assert!(same(fades, Fades::of_curve(&curve(fades.value())).unwrap()));
        let back = curve(fades.value());
        for i in 0..=20 {
            let x = f64::from(i) / 20.;
            assert!((back.sample(x) - swell.sample(x)).abs() < 1e-9, "{x}");
        }
    }

    #[test]
    fn other_curves_are_custom() {
        for curve in [
            numbers(&[[0., 0.2], [0.5, 0.9], [1., 0.2]], &[]),
            numbers(&[[0., 0.], [0.5, 1.], [1., 0.]], &[p::Ease::Hold; 2]),
            numbers(&[[0., 1.], [0.4, 0.5], [1., 0.5]], &[]),
        ] {
            assert_eq!(Fades::of_curve(&curve), None, "{curve:?}");
        }
    }

    #[test]
    fn dragging_the_handles() {
        let flat = Fades::flat(1.);
        let span = (10., 14.);
        let fade_in = dragged(Part::FadeIn, flat, span, 11., 0.);
        assert!((fade_in.fade_in - 0.25).abs() < 1e-12);
        let fade_out = dragged(Part::FadeOut, fade_in, span, 12., 0.);
        assert!((fade_out.fade_out - 0.5).abs() < 1e-12);
        // Past the other fade it stops there; back past the edge it is gone.
        assert!((dragged(Part::FadeIn, fade_out, span, 13.9, 0.).fade_in - 0.5).abs() < 1e-12);
        assert_eq!(dragged(Part::FadeIn, fade_in, span, 9., 0.).fade_in, 0.);
        assert_eq!(
            dragged(Part::FadeIn, fade_in, span, 10., 0.).value(),
            Level::Flat(1.)
        );
        // A partial bend: the middle of the fade follows the pointer up.
        let middle = |fades: Fades| curve(fades.value()).sample(fades.fade_in / 2.);
        let bent = dragged(Part::BendIn, fade_in, span, 0., 0.1);
        assert!(
            (middle(bent) - (middle(fade_in) + 0.1)).abs() < 1e-3,
            "{}",
            middle(bent)
        );
        assert!(matches!(curve(bent.value()).ease(0), p::Ease::Bezier(_)));
        assert!(same(Fades::of_curve(&curve(bent.value())).unwrap(), bent));
        // Down past straight it bends the other way, and back is straight.
        let sagging = dragged(Part::BendIn, bent, span, 0., -0.2);
        assert!(middle(sagging) < middle(fade_in));
        let straight = dragged(Part::BendIn, bent, span, 0., -0.1);
        assert_eq!(curve(straight.value()).ease(0), p::Ease::Linear);
        // Handles stay within the fade: a hard pull stops at the top.
        let hard = dragged(Part::BendIn, fade_in, span, 0., 5.);
        assert!(curve(hard.value()).validate().is_ok());
        assert_eq!(
            curve(hard.value()).ease(0).handles().map(|h| [h[1], h[3]]),
            Some([1., 1.])
        );
        // A fade-out bends up the same way.
        let fade_out = dragged(Part::FadeOut, flat, span, 13., 0.);
        let end = |fades: Fades| curve(fades.value()).sample(1. - fades.fade_out / 2.);
        let lifted = dragged(Part::BendOut, fade_out, span, 0., 0.1);
        assert!(
            (end(lifted) - (end(fade_out) + 0.1)).abs() < 1e-3,
            "{}",
            end(lifted)
        );
    }

    #[test]
    fn lifting_a_segment_moves_both_its_points() {
        let flat = numbers(&[[0., 1.], [1., 1.]], &[]);
        assert_eq!(lift(&flat, 0, -0.25), Level::Flat(0.75));
        assert_eq!(lift(&flat, 0, -2.), Level::Flat(0.));
        // The hold of a fade moves the level and keeps the fade.
        let fade = curve(Fades::flat(1.).with_fade_in(0.25).value());
        let lowered = Fades::flat(0.5).with_fade_in(0.25).value();
        assert_eq!(lift(&fade, 1, -0.5), lowered);
        // A bent fade keeps its bend when the level moves.
        let bent = Fades {
            bend_in: p::Ease::EaseInOut,
            ..Fades::flat(1.).with_fade_in(0.25)
        };
        let lowered_bent = lift(&curve(bent.value()), 1, -0.5);
        let back = Fades::of_curve(&curve(lowered_bent)).unwrap();
        assert!(same(back, Fades { level: 0.5, ..bent }), "{back:?}");
        // The ramp lifts off zero: a custom curve, each point kept in 0–1.
        assert_eq!(
            lift(&curve(lowered), 0, 0.75),
            Level::Curve(numbers(&[[0., 0.75], [0.25, 1.], [1., 0.5]], &[]))
        );
    }

    #[test]
    fn an_alpha_over_events_or_a_shifted_time_has_no_one_line() {
        let mut graph = ClipGraph::new([("color1".to_owned(), Node::new(Kind::Color))]);
        write(&mut graph, &Fades::flat(1.).with_fade_in(0.25).value());
        assert!(alpha_of(&graph).is_some());
        let time = graph
            .nodes
            .iter()
            .find(|(_, node)| node.kind == Kind::Time)
            .map(|(id, _)| id.clone())
            .unwrap();
        for (name, value) in [
            ("every", 1.),
            ("duration", 2.),
            ("delay", 0.5),
            ("phase", 90.),
        ] {
            let mut moved = graph.clone();
            let node = moved.nodes.get_mut(&time).unwrap();
            node.inputs
                .insert(name.into(), p::clip_graph::Input::Number(value));
            assert!(alpha_of(&moved).is_none(), "a time with {name} has a line");
        }
    }

    #[test]
    fn a_fade_is_stored_as_a_curve_over_the_clip_and_a_level_takes_it_back() {
        let mut graph = ClipGraph::new([("color1".to_owned(), Node::new(Kind::Color))]);
        let fades = Fades::flat(0.8).with_fade_in(0.25);
        write(&mut graph, &fades.value());
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        assert_eq!(graph.nodes.len(), 3);
        match alpha_of(&graph) {
            Some(Alpha::Fades(back)) => assert_eq!(back, fades),
            _ => panic!("the fade does not read back"),
        }
        // A second write edits the same curve.
        write(&mut graph, &fades.with_fade_out(0.25).value());
        assert_eq!(graph.nodes.len(), 3);
        write(&mut graph, &Level::Flat(0.5));
        assert_eq!(graph.nodes.len(), 1);
        assert!(matches!(alpha_of(&graph), Some(Alpha::Fades(f)) if f == Fades::flat(0.5)));
    }
}
