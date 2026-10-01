//! The one curve format: a list of points `[x, value]` or `[x, value, ease]`.
//! x runs from 0 to 1. The ease says how the value moves from its point to
//! the next; an omitted ease is linear. An envelope and a `time` or `hit`
//! source are the same curve with different values.
use crate::{Error, Result};
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeTuple, Serializer};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::marker::PhantomData;

/// The shape of a curve, for errors that a model reads.
const EXAMPLE: &str = r#"{"points": [[0, 0, "ease-in"], [0.5, 1, "hold"], [0.8, 1], [1, 0]]}"#;
const EASES: &str = r#""linear", "ease-in", "ease-out", "ease-in-out", "hold" or [x1, y1, x2, y2] such as [0.42, 0, 0.58, 1]"#;

/// How a value moves from its point to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Ease {
    #[default]
    Linear,
    SineIn,
    SineOut,
    SineInOut,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// Stay at this point's value and jump to the next value at the next point.
    Hold,
    /// CSS `cubic-bezier(x1, y1, x2, y2)`, local to the segment: x is a share
    /// of the segment's length, y a share of the change to the next value.
    /// Every number is in 0..1, so the curve stays between its two values.
    Bezier([f64; 4]),
}

impl Ease {
    const NAMED: [(&'static str, Ease); 8] = [
        ("linear", Ease::Linear),
        ("sine-in", Ease::SineIn),
        ("sine-out", Ease::SineOut),
        ("sine-in-out", Ease::SineInOut),
        ("ease-in", Ease::EaseIn),
        ("ease-out", Ease::EaseOut),
        ("ease-in-out", Ease::EaseInOut),
        ("hold", Ease::Hold),
    ];

    /// The Bézier handles `[x1, y1, x2, y2]`; `None` for a hold. A linear
    /// ease has its handles on the line, at the thirds.
    pub fn handles(self) -> Option<[f64; 4]> {
        Some(match self {
            Self::Linear => [1. / 3., 1. / 3., 2. / 3., 2. / 3.],
            Self::SineIn => [0.47, 0., 0.745, 0.715],
            Self::SineOut => [0.39, 0.575, 0.565, 1.],
            Self::SineInOut => [0.445, 0.05, 0.55, 0.95],
            Self::EaseIn => [0.42, 0., 1., 1.],
            Self::EaseOut => [0., 0., 0.58, 1.],
            Self::EaseInOut => [0.42, 0., 0.58, 1.],
            Self::Bezier(handles) => handles,
            Self::Hold => return None,
        })
    }

    /// The share of the change done at share `t` of the segment.
    pub fn apply(self, t: f64) -> f64 {
        match self {
            Self::Linear => t,
            Self::SineIn => 1. - (std::f64::consts::FRAC_PI_2 * t).cos(),
            Self::SineOut => (std::f64::consts::FRAC_PI_2 * t).sin(),
            Self::SineInOut => (1. - (std::f64::consts::PI * t).cos()) / 2.,
            Self::Hold => 0.,
            _ => {
                let [x1, y1, x2, y2] = self.handles().expect("a curved ease");
                let controls = [[0., 0.], [x1, y1], [x2, y2], [1., 1.]];
                at(controls, parameter(controls, t))[1]
            }
        }
    }

    fn is_valid(self) -> bool {
        match self {
            Self::Bezier(handles) => handles
                .iter()
                .all(|v| v.is_finite() && (0. ..=1.).contains(v)),
            _ => true,
        }
    }
}

impl Serialize for Ease {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Bezier(handles) => handles.serialize(serializer),
            named => {
                let (name, _) = Self::NAMED
                    .iter()
                    .find(|(_, ease)| ease == named)
                    .expect("every ease but a Bézier has a name");
                serializer.serialize_str(name)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Ease {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct EaseVisitor;
        impl<'de> Visitor<'de> for EaseVisitor {
            type Value = Ease;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "an ease: {EASES}")
            }
            fn visit_str<E: de::Error>(self, name: &str) -> std::result::Result<Ease, E> {
                Ease::NAMED
                    .iter()
                    .find(|(known, _)| *known == name)
                    .map(|(_, ease)| *ease)
                    .ok_or_else(|| E::custom(format!("unknown ease {name:?}; use {EASES}")))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Ease, A::Error> {
                let mut handles = [0.; 4];
                for (i, handle) in handles.iter_mut().enumerate() {
                    *handle = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;
                }
                if seq.next_element::<IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom(format!(
                        "a Bézier ease has four numbers; use {EASES}"
                    )));
                }
                Ok(Ease::Bezier(handles))
            }
        }
        deserializer.deserialize_any(EaseVisitor)
    }
}

/// One point of a curve and the ease from it to the next point. The last
/// point's ease is always linear and is never written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint<V> {
    pub x: f64,
    pub value: V,
    pub ease: Ease,
}

impl<V> CurvePoint<V> {
    pub fn new(x: f64, value: V) -> Self {
        Self {
            x,
            value,
            ease: Ease::Linear,
        }
    }
    pub fn eased(x: f64, value: V, ease: Ease) -> Self {
        Self { x, value, ease }
    }
}

impl<V: Serialize> Serialize for CurvePoint<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let linear = self.ease == Ease::Linear;
        let mut tuple = serializer.serialize_tuple(if linear { 2 } else { 3 })?;
        tuple.serialize_element(&self.x)?;
        tuple.serialize_element(&self.value)?;
        if !linear {
            tuple.serialize_element(&self.ease)?;
        }
        tuple.end()
    }
}

/// A point as written, and whether it was written with an ease.
struct Written<V>(CurvePoint<V>, bool);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Written<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct PointVisitor<V>(PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for PointVisitor<V> {
            type Value = Written<V>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(
                    r#"a point [x, value] or [x, value, ease], such as [0.5, 1, "ease-in"]"#,
                )
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Written<V>, A::Error> {
                let x = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let value = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let ease: Option<Ease> = seq.next_element()?;
                if seq.next_element::<IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(4, &self));
                }
                Ok(Written(
                    CurvePoint::eased(x, value, ease.unwrap_or_default()),
                    ease.is_some(),
                ))
            }
        }
        deserializer.deserialize_seq(PointVisitor(PhantomData))
    }
}

/// Points over x 0–1, from x 0 to x 1, in order. Two points may share an x:
/// a jump, where x itself reads the larger of the two values, so a head
/// exactly on an edge is lit (a cut's front, a pill's ends). Before the
/// first point and after the last, the end values hold; a jump at x 0 or
/// x 1 sets the value outside, so `[[0, 0], [0, 1], [1, 1], [1, 0]]` is 1
/// on [0, 1] closed and `[[0, 1], [0, 0], [1, 0]]` is 1 up to 0 closed.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve<V> {
    pub points: Vec<CurvePoint<V>>,
}

impl<V: Serialize> Serialize for Curve<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("points", &self.points)?;
        map.end()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Curve<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CurveVisitor<V>(PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for CurveVisitor<V> {
            type Value = Curve<V>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a curve such as {EXAMPLE}")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Curve<V>, A::Error> {
                let mut points: Option<Vec<Written<V>>> = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key != "points" || points.is_some() {
                        return Err(de::Error::custom(format!(
                            "a curve has only \"points\", and each point carries its own ease; \
                             got {key:?}. Write {EXAMPLE}"
                        )));
                    }
                    points = Some(map.next_value()?);
                }
                let points = points.ok_or_else(|| {
                    de::Error::custom(format!("a curve needs \"points\": {EXAMPLE}"))
                })?;
                if points.last().is_some_and(|Written(_, eased)| *eased) {
                    return Err(de::Error::custom(
                        r#"the last point takes no ease: write [1, 0], not [1, 0, "hold"]"#,
                    ));
                }
                Ok(Curve {
                    points: points.into_iter().map(|Written(point, _)| point).collect(),
                })
            }
        }
        deserializer.deserialize_map(CurveVisitor(PhantomData))
    }
}

impl<V> Curve<V> {
    pub fn new(points: Vec<CurvePoint<V>>) -> Self {
        Self { points }
    }

    /// A curve through `points` with `eases[i]` from point `i`. Missing
    /// eases are linear.
    pub fn with_eases(points: impl IntoIterator<Item = (f64, V)>, eases: &[Ease]) -> Self {
        Self::new(
            points
                .into_iter()
                .enumerate()
                .map(|(i, (x, value))| {
                    CurvePoint::eased(x, value, eases.get(i).copied().unwrap_or_default())
                })
                .collect(),
        )
    }

    /// The same curve with every value passed through `value`.
    pub fn map<W>(&self, value: impl Fn(&V) -> W) -> Curve<W> {
        Curve::new(
            self.points
                .iter()
                .map(|p| CurvePoint::eased(p.x, value(&p.value), p.ease))
                .collect(),
        )
    }

    /// The ease from point `segment` to the next.
    pub fn ease(&self, segment: usize) -> Ease {
        self.points[segment].ease
    }

    /// The checks every curve shares: the count, the x of each point, each
    /// ease. `value` says why a value is wrong, or `None` when it is right.
    pub(crate) fn check(&self, value: impl Fn(&V) -> Option<String>) -> Result<()> {
        let n = self.points.len();
        if !(2..=256).contains(&n) {
            return Err(Error(format!(
                "a curve has {n} points; it needs 2–256, such as {EXAMPLE}"
            )));
        }
        for (i, point) in self.points.iter().enumerate() {
            if !point.x.is_finite() || !(0. ..=1.).contains(&point.x) {
                return Err(Error(format!("points[{i}]: x must be in 0..1")));
            }
            if i > 0 && point.x < self.points[i - 1].x {
                return Err(Error(format!(
                    "points[{i}]: x {} must not be below the previous x {}",
                    point.x,
                    self.points[i - 1].x
                )));
            }
            if i > 1 && point.x == self.points[i - 2].x {
                return Err(Error(format!(
                    "points[{i}]: at most two points share an x (a jump); x {} has three",
                    point.x
                )));
            }
            if let Some(why) = value(&point.value) {
                return Err(Error(format!("points[{i}]: {why}")));
            }
            if !point.ease.is_valid() {
                return Err(Error(format!(
                    "points[{i}]: a Bézier ease [x1, y1, x2, y2] needs each number in 0..1, such as [0.42, 0, 0.58, 1]"
                )));
            }
        }
        if self.points[0].x != 0. || self.points[n - 1].x != 1. {
            return Err(Error(format!(
                "the first point needs x 0 and the last x 1, such as {EXAMPLE}"
            )));
        }
        if self.points[n - 1].ease != Ease::Linear {
            return Err(Error(
                r#"the last point takes no ease: write [1, 0], not [1, 0, "hold"]"#.into(),
            ));
        }
        Ok(())
    }

    /// The segment at `progress` and the share of its change done there; at
    /// a jump, the segment after it.
    pub(crate) fn locate(&self, progress: f64) -> (usize, f64) {
        let last = self.points.len() - 1;
        if progress < self.points[0].x {
            return (0, 0.);
        }
        if progress >= self.points[last].x {
            return (last - 1, 1.);
        }
        let i = self
            .points
            .partition_point(|p| p.x <= progress)
            .saturating_sub(1)
            .min(last - 1);
        let (a, b) = (self.points[i].x, self.points[i + 1].x);
        (i, self.points[i].ease.apply((progress - a) / (b - a)))
    }
}

pub(crate) fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
}
pub(crate) fn at([a, b, c, d]: [[f64; 2]; 4], t: f64) -> [f64; 2] {
    lerp(
        lerp(lerp(a, b, t), lerp(b, c, t), t),
        lerp(lerp(b, c, t), lerp(c, d, t), t),
        t,
    )
}
/// The Bézier parameter where the curve reaches `x`. Its x only grows.
pub(crate) fn parameter(controls: [[f64; 2]; 4], x: f64) -> f64 {
    if x <= controls[0][0] {
        return 0.;
    }
    if x >= controls[3][0] {
        return 1.;
    }
    let (mut low, mut high) = (0., 1.);
    for _ in 0..40 {
        let mid = (low + high) * 0.5;
        if at(controls, mid)[0] < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5
}
