//! A form clip's alpha on the timeline, the way a DAW shows clip fades.
//!
//! Alpha is how much a clip counts. The timeline draws it as a line across
//! the clip body and dims the body above the line. Handles on the line edit
//! it: a fade-in and a fade-out handle at the top corners, a bend handle in
//! the middle of each fade, and the flat part of the line for the level.
//!
//! The handles edit the same stored `alpha` input that the clip sheet shows.
//! A simple fade shape is a [`Fades`]. Any other curve is drawn but has no
//! handles; the sheet's curve editor edits it.

use super::*;
use luma_lib::node_graph::lighting::decode;
use luma_patterns as p;

/// The input every form clip has.
pub(super) const ALPHA: &str = "alpha";

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
/// How far a bend handle moves before the fade changes shape.
const BEND_STEP: f32 = 6.;
/// Fade lengths and levels closer than this count as equal.
const EPSILON: f64 = 1e-9;

/// A simple fade shape: rise from 0 to `level`, hold, fall back to 0.
/// Fade lengths are shares of the clip, 0–1, and never sum past 1. A fade is
/// straight, or eased (the engine's `ease` segment).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fades {
    pub level: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub ease_in: bool,
    pub ease_out: bool,
}

impl Fades {
    pub fn flat(level: f64) -> Self {
        Self {
            level: level.clamp(0., 1.),
            fade_in: 0.,
            fade_out: 0.,
            ease_in: false,
            ease_out: false,
        }
    }

    /// The fade shape `curve` draws, or `None` for any other curve.
    pub fn of_curve(curve: &p::Keyframes) -> Option<Self> {
        if curve.is_color() || curve.points.is_empty() {
            return None;
        }
        let points: Vec<(f64, f64)> = curve
            .points
            .iter()
            .map(|(x, key)| match key {
                p::Key::Number(v) => (*x, *v),
                p::Key::Color(_) => (*x, 0.),
            })
            .collect();
        let segment = |i: usize| curve.segments.get(i).copied().unwrap_or_default();
        let ramp = |i: usize| matches!(segment(i), p::Segment::Linear | p::Segment::Ease);
        let zero = |v: f64| v.abs() <= EPSILON;
        let last = points.len() - 1;
        let (mut first, mut end) = (0, last);
        let mut fades = Self::flat(points[0].1);
        if last >= 1 && zero(points[0].0) && zero(points[0].1) && points[1].1 > EPSILON && ramp(0) {
            fades.fade_in = points[1].0;
            fades.ease_in = segment(0) == p::Segment::Ease;
            first = 1;
        }
        if end > first
            && (points[end].0 - 1.).abs() <= EPSILON
            && zero(points[end].1)
            && points[end - 1].1 > EPSILON
            && ramp(end - 1)
        {
            fades.fade_out = 1. - points[end - 1].0;
            fades.ease_out = segment(end - 1) == p::Segment::Ease;
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

    /// The stored alpha: a plain proportion when there is no fade, a curve
    /// over the clip otherwise.
    pub fn value(self) -> p::Value {
        let level = self.level.clamp(0., 1.);
        let fade_in = self.fade_in.clamp(0., 1.);
        let fade_out = self.fade_out.clamp(0., 1. - fade_in);
        if fade_in <= EPSILON && fade_out <= EPSILON {
            return p::Value::Proportion(level);
        }
        let shape = |ease: bool| {
            if ease {
                p::Segment::Ease
            } else {
                p::Segment::Linear
            }
        };
        let mut points = Vec::with_capacity(4);
        let mut segments = Vec::with_capacity(3);
        if fade_in > EPSILON {
            points.push([0., 0.]);
            points.push([fade_in, level]);
            segments.push(shape(self.ease_in));
        } else {
            points.push([0., level]);
        }
        let hold = 1. - fade_out;
        if fade_out > EPSILON {
            if hold > points.last().unwrap()[0] + EPSILON {
                points.push([hold, level]);
                segments.push(p::Segment::Linear);
            }
            points.push([1., 0.]);
            segments.push(shape(self.ease_out));
        } else if points.last().unwrap()[0] < 1. - EPSILON {
            points.push([1., level]);
            segments.push(p::Segment::Linear);
        }
        p::Value::Time(p::Keyframes::numbers(&points, &segments))
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

/// A form clip's alpha, as far as the timeline can show it.
pub(super) enum Alpha {
    /// A plain value or a simple fade shape: drawn, with handles.
    Fades(Fades),
    /// Any other number curve over the clip: drawn, no handles.
    Custom(p::Keyframes),
}

impl Alpha {
    fn sample(&self, progress: f64) -> f64 {
        match self {
            Self::Fades(fades) => match fades.value() {
                p::Value::Time(curve) => curve.sample(progress)[0],
                _ => fades.level,
            },
            Self::Custom(curve) => curve.sample(progress)[0],
        }
    }
}

/// The alpha of a form clip. `None` for a clip with its own graph, and for an
/// alpha that is noise, audio or a per-hit curve: those change on their own
/// and have no one line to draw.
pub(super) fn alpha(clip: &Clip) -> Option<Alpha> {
    let input = document::form_definition(&clip.pattern)?
        .inputs
        .get(ALPHA)?;
    let value = match clip.args.get(ALPHA) {
        Some(stored) => decode(input.value_type, stored).ok()?,
        None => input.default.clone()?,
    };
    match value {
        p::Value::Proportion(v) | p::Value::Number(v) => Some(Alpha::Fades(Fades::flat(v))),
        p::Value::Time(curve) if !curve.is_color() => Some(match Fades::of_curve(&curve) {
            Some(fades) => Alpha::Fades(fades),
            None => Alpha::Custom(curve),
        }),
        _ => None,
    }
}

/// Write `value` as `clip`'s alpha in the working copy. `false` when it is
/// already stored, so an idle drag records no edit.
pub(super) fn store(clips: &mut [Clip], id: &str, value: &p::Value) -> bool {
    clips
        .iter_mut()
        .find(|clip| clip.id.as_ref() == id)
        .is_some_and(|clip| set(clip, value))
}

fn set(clip: &mut Clip, value: &p::Value) -> bool {
    let wire = document::wire_value(value);
    if clip.args.get(ALPHA) == Some(&wire) {
        return false;
    }
    match &mut clip.args {
        serde_json::Value::Object(map) => {
            map.insert(ALPHA.to_string(), wire);
        }
        other => *other = serde_json::json!({ ALPHA: wire }),
    }
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
            Self::Segment(index) => format!("alpha {}", index + 1),
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
    curve: p::Keyframes,
    /// The fade shape, when the line is one.
    fades: Option<Fades>,
}

impl Alpha {
    /// The line as a curve over the clip.
    fn curve(&self) -> p::Keyframes {
        match self {
            Self::Fades(fades) => match fades.value() {
                p::Value::Time(curve) => curve,
                _ => p::Keyframes::numbers(&[[0., fades.level], [1., fades.level]], &[]),
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
    let middle = frame.y(fades.level / 2.);
    let mut handles = vec![
        (Part::FadeIn, point(frame.x(fades.fade_in), top)),
        (Part::FadeOut, point(frame.x(1. - fades.fade_out), top)),
    ];
    if fades.fade_in as f32 * frame.width >= BEND_MIN {
        handles.push((Part::BendIn, point(frame.x(fades.fade_in / 2.), middle)));
    }
    if fades.fade_out as f32 * frame.width >= BEND_MIN {
        handles.push((
            Part::BendOut,
            point(frame.x(1. - fades.fade_out / 2.), middle),
        ));
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
    if (y - frame.y(curve.sample(progress.clamp(0., 1.))[0])).abs() > LINE_GRAB {
        return None;
    }
    let index = curve
        .points
        .windows(2)
        .position(|pair| pair[0].0 < pair[1].0 && (pair[0].0..=pair[1].0).contains(&progress))?;
    grab(Part::Segment(index))
}

/// The alpha a drag leaves, from `grab` at the press. `span` is the clip's
/// seconds, `time` the pointer's time, snapped; `rise` is how far the pointer
/// went up and `height` the line's travel from 0 to 1, in pixels.
pub(super) fn moved(grab: &Grab, span: (f64, f64), time: f64, rise: f32, height: f32) -> p::Value {
    match (grab.part, grab.fades) {
        (Part::Segment(index), _) => lift(&grab.curve, index, f64::from(rise / height.max(1.))),
        (part, Some(fades)) => dragged(part, fades, span, time, rise).value(),
        (_, None) => p::Value::Time(grab.curve.clone()),
    }
}

/// `curve` with segment `index` moved up by `by`: both of its points, each
/// kept in 0–1. A line that is a fade shape again is stored as one.
pub(super) fn lift(curve: &p::Keyframes, index: usize, by: f64) -> p::Value {
    let mut curve = curve.clone();
    for (_, key) in curve.points.iter_mut().skip(index).take(2) {
        if let p::Key::Number(value) = key {
            *value = (*value + by).clamp(0., 1.);
        }
    }
    match Fades::of_curve(&curve) {
        Some(fades) => fades.value(),
        None => p::Value::Time(curve),
    }
}

/// A drag of a fade handle, from `fades` as they were at the press. `time`
/// is the pointer's time, snapped; `rise` is how far the pointer went up, in
/// pixels.
pub(super) fn dragged(part: Part, fades: Fades, span: (f64, f64), time: f64, rise: f32) -> Fades {
    let length = (span.1 - span.0).max(EPSILON);
    match part {
        Part::FadeIn => fades.with_fade_in((time - span.0) / length),
        Part::FadeOut => fades.with_fade_out((span.1 - time) / length),
        Part::BendIn | Part::BendOut => {
            let ease = if rise >= BEND_STEP {
                true
            } else if rise <= -BEND_STEP {
                false
            } else if part == Part::BendIn {
                fades.ease_in
            } else {
                fades.ease_out
            };
            if part == Part::BendIn {
                Fades {
                    ease_in: ease,
                    ..fades
                }
            } else {
                Fades {
                    ease_out: ease,
                    ..fades
                }
            }
        }
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
    // Every key, and each side of it, so holds and steps keep their corners;
    // and enough even samples between them for eases to read as curves.
    let mut xs: Vec<f64> = match alpha {
        Alpha::Fades(fades) => match fades.value() {
            p::Value::Time(curve) => curve.points.iter().map(|(x, _)| *x).collect(),
            _ => Vec::new(),
        },
        Alpha::Custom(curve) => curve.points.iter().map(|(x, _)| *x).collect(),
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
        if pair[0].0 >= pair[1].0 {
            continue;
        }
        let at = pair[0].0 + (pair[1].0 - pair[0].0) * 0.2;
        agent_paint_node(
            Role::Slider,
            format!("{} {}", clip.label, Part::Segment(index).label()),
            square(point(frame.x(at), frame.y(curve.sample(at)[0]))),
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

    /// Take hold of a form clip's alpha line if the press at `at` is on it.
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
    use super::{dragged, lift, Fades, Part};
    use luma_patterns as p;

    fn curve(value: p::Value) -> p::Keyframes {
        match value {
            p::Value::Time(curve) => curve,
            other => panic!("expected a curve, got {other:?}"),
        }
    }

    #[test]
    fn no_fade_is_a_plain_value() {
        assert_eq!(Fades::flat(0.7).value(), p::Value::Proportion(0.7));
    }

    #[test]
    fn a_fade_in_rises_holds_and_reads_back() {
        let fades = Fades::flat(0.8).with_fade_in(0.25);
        let curve = curve(fades.value());
        assert_eq!(
            curve,
            p::Keyframes::numbers(
                &[[0., 0.], [0.25, 0.8], [1., 0.8]],
                &[p::Segment::Linear, p::Segment::Linear]
            )
        );
        assert_eq!(Fades::of_curve(&curve), Some(fades));
    }

    #[test]
    fn both_fades_with_eases_round_trip() {
        let fades = Fades {
            ease_in: true,
            ease_out: false,
            ..Fades::flat(1.).with_fade_in(0.2).with_fade_out(0.3)
        };
        let curve = curve(fades.value());
        assert_eq!(curve.points.len(), 4);
        assert_eq!(
            curve.segments,
            vec![p::Segment::Ease, p::Segment::Linear, p::Segment::Linear]
        );
        assert!((curve.sample(0.1)[0] - 0.5).abs() < 1e-9);
        assert!((curve.sample(0.85)[0] - 0.5).abs() < 1e-9);
        let back = Fades::of_curve(&curve).unwrap();
        assert!((back.fade_out - 0.3).abs() < 1e-12);
        assert_eq!(
            Fades {
                fade_out: 0.3,
                ..back
            },
            fades
        );
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
        let ramp = p::Keyframes::numbers(&[[0., 0.], [1., 1.]], &[]);
        let fades = Fades::of_curve(&ramp).unwrap();
        assert_eq!((fades.fade_in, fades.fade_out, fades.level), (1., 0., 1.));
        let swell = p::Keyframes::numbers(
            &[[0., 0.], [0.5, 1.], [1., 0.]],
            &[p::Segment::Ease, p::Segment::Ease],
        );
        let fades = Fades::of_curve(&swell).unwrap();
        assert_eq!((fades.fade_in, fades.fade_out), (0.5, 0.5));
        assert!(fades.ease_in && fades.ease_out);
        assert_eq!(fades.value(), p::Value::Time(swell));
    }

    #[test]
    fn other_curves_are_custom() {
        for curve in [
            p::Keyframes::numbers(&[[0., 0.2], [0.5, 0.9], [1., 0.2]], &[]),
            p::Keyframes::numbers(&[[0., 0.], [0.5, 1.], [1., 0.]], &[p::Segment::Hold; 2]),
            p::Keyframes::numbers(&[[0., 1.], [0.4, 0.5], [1., 0.5]], &[]),
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
            p::Value::Proportion(1.)
        );
        assert!(dragged(Part::BendIn, fade_in, span, 0., 10.).ease_in);
        assert!(!dragged(Part::BendIn, fade_in, span, 0., 3.).ease_in);
    }

    #[test]
    fn lifting_a_segment_moves_both_its_points() {
        let flat = p::Keyframes::numbers(&[[0., 1.], [1., 1.]], &[]);
        assert_eq!(lift(&flat, 0, -0.25), p::Value::Proportion(0.75));
        assert_eq!(lift(&flat, 0, -2.), p::Value::Proportion(0.));
        // The hold of a fade moves the level and keeps the fade.
        let fade = curve(Fades::flat(1.).with_fade_in(0.25).value());
        let lowered = Fades::flat(0.5).with_fade_in(0.25).value();
        assert_eq!(lift(&fade, 1, -0.5), lowered);
        // The ramp lifts off zero: a custom curve, each point kept in 0–1.
        assert_eq!(
            lift(&curve(lowered), 0, 0.75),
            p::Value::Time(p::Keyframes::numbers(
                &[[0., 0.75], [0.25, 1.], [1., 0.5]],
                &[p::Segment::Linear, p::Segment::Linear]
            ))
        );
    }
}
