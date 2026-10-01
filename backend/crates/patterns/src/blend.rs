use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    Replace,
    Add,
    Multiply,
    Screen,
    Max,
    Min,
    Subtract,
    /// An aim clip's fan and motion turn the aim under it
    /// ([`crate::aim::offset_aim`]). Only aim takes it
    /// ([`crate::blend_modes`]).
    Offset,
}

impl BlendMode {
    /// Every mode. The one canonical list — the score DSL, the track-edit
    /// hasher, and both hosts' blend selects all read it (or a form's share
    /// of it, [`crate::blend_modes`]) from here rather than keeping a
    /// spelling of their own.
    pub const ALL: [Self; 8] = [
        Self::Replace,
        Self::Add,
        Self::Multiply,
        Self::Screen,
        Self::Max,
        Self::Min,
        Self::Subtract,
        Self::Offset,
    ];

    /// The modes that blend light, in the order a picker lists them.
    pub const LIGHT: [Self; 7] = [
        Self::Replace,
        Self::Add,
        Self::Multiply,
        Self::Screen,
        Self::Max,
        Self::Min,
        Self::Subtract,
    ];

    /// The mode's wire spelling. Identical to the serde `camelCase` rename on
    /// the enum — the DSL and the JSON schema deliberately agree — so a new
    /// variant added to one is a compile error here rather than a drift.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Add => "add",
            Self::Multiply => "multiply",
            Self::Screen => "screen",
            Self::Max => "max",
            Self::Min => "min",
            Self::Subtract => "subtract",
            Self::Offset => "offset",
        }
    }

    /// The mode's name in a picker, in sentence case.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Replace => "Replace",
            Self::Add => "Add",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Max => "Max",
            Self::Min => "Min",
            Self::Subtract => "Subtract",
            Self::Offset => "Offset",
        }
    }

    /// [`Self::name`]'s inverse; `None` for a word that names no mode.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.name() == name)
    }
}

/// Blend a single scalar channel (dimmer / strobe / a color component).
/// Ported from `compositor::blend_values`.
#[inline]
pub fn blend_value(base: f32, top: f32, mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Replace => top,
        BlendMode::Offset => {
            unreachable!("an Offset clip turns aim through `offset_aim` and blends no value")
        }
        BlendMode::Add => (base + top).min(1.0),
        BlendMode::Multiply => base * top,
        BlendMode::Screen => 1.0 - (1.0 - base) * (1.0 - top),
        BlendMode::Max => base.max(top),
        BlendMode::Min => base.min(top),
        BlendMode::Subtract => (base - top).max(0.0),
    }
}

/// Blend one head's light onto the light under it. Light is a normalized
/// color and a dimmer; the light itself is their product. No light under a
/// head is a zero dimmer, and the color of zero light never shows, so a black
/// head under a layer is the same as no head.
///
/// `Replace` is the top light, as in every compositor: black on replace
/// paints black. Opacity is alpha's job ([`blend_light_alpha`]). Every other
/// mode is [`blend_value`] on each channel of the light: add and screen over
/// black equal add and screen over nothing, and multiply or subtract over
/// nothing stay nothing.
#[inline]
pub fn blend_light(
    base_color: [f32; 3],
    base_dimmer: f32,
    top_color: [f32; 3],
    top_dimmer: f32,
    mode: BlendMode,
) -> ([f32; 3], f32) {
    let base_dimmer = base_dimmer.clamp(0.0, 1.0);
    let top_dimmer = top_dimmer.clamp(0.0, 1.0);
    if mode == BlendMode::Replace {
        return (top_color, top_dimmer);
    }
    let light: [f32; 3] = std::array::from_fn(|c| {
        blend_value(base_color[c] * base_dimmer, top_color[c] * top_dimmer, mode).clamp(0.0, 1.0)
    });
    let dimmer = light.into_iter().fold(0.0_f32, f32::max);
    if dimmer <= 1e-6 {
        return (top_color, 0.0);
    }
    (light.map(|v| v / dimmer), dimmer)
}

/// A clip's light over the light under it at the clip's opacity `alpha`:
/// the clip's light blends with the light below by `mode`
/// ([`blend_light`]), then the result mixes with the light below by alpha.
/// Alpha 0 leaves the light below as it was, in every mode; alpha 1 is
/// [`blend_light`].
#[inline]
pub fn blend_light_alpha(
    base_color: [f32; 3],
    base_dimmer: f32,
    top_color: [f32; 3],
    top_dimmer: f32,
    mode: BlendMode,
    alpha: f32,
) -> ([f32; 3], f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return (base_color, base_dimmer);
    }
    let (color, dimmer) = blend_light(base_color, base_dimmer, top_color, top_dimmer, mode);
    if alpha >= 1.0 {
        return (color, dimmer);
    }
    let base_dimmer = base_dimmer.clamp(0.0, 1.0);
    let light: [f32; 3] = std::array::from_fn(|c| {
        let below = base_color[c] * base_dimmer;
        below + (color[c] * dimmer - below) * alpha
    });
    let dimmer = light.into_iter().fold(0.0_f32, f32::max);
    if dimmer <= 1e-6 {
        return (color, 0.0);
    }
    (light.map(|v| v / dimmer), dimmer)
}

/// One scalar channel (a strobe's shutter) over the one under it at the
/// clip's opacity `alpha`, as [`blend_light_alpha`].
#[inline]
pub fn blend_value_alpha(base: f32, top: f32, mode: BlendMode, alpha: f32) -> f32 {
    let alpha = alpha.clamp(0.0, 1.0);
    base + (blend_value(base, top, mode) - base) * alpha
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 3] = [1.0, 0.0, 0.0];
    const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
    const NOTHING: ([f32; 3], f32) = ([1.0; 3], 0.0);

    fn light((color, dimmer): ([f32; 3], f32)) -> [f32; 3] {
        color.map(|c| c * dimmer)
    }
    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6)
    }
    fn over(base: ([f32; 3], f32), top: ([f32; 3], f32), mode: BlendMode) -> [f32; 3] {
        light(blend_light(base.0, base.1, top.0, top.1, mode))
    }

    #[test]
    fn add_and_screen_over_black_equal_over_nothing() {
        for mode in [BlendMode::Add, BlendMode::Screen] {
            let top = (BLUE, 0.5);
            let on_nothing = over(NOTHING, top, mode);
            assert!(close(on_nothing, [0.0, 0.0, 0.5]), "{mode:?}");
            // A black head from a lower clip, whatever color it holds.
            assert!(close(over((RED, 0.0), top, mode), on_nothing), "{mode:?}");
            assert!(
                close(over(([0.0; 3], 0.0), top, mode), on_nothing),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn add_adds_the_light_underneath() {
        assert!(close(
            over((RED, 0.5), (BLUE, 0.5), BlendMode::Add),
            [0.5, 0.0, 0.5]
        ));
        assert!(close(
            over((RED, 0.25), (RED, 0.25), BlendMode::Add),
            [0.5, 0.0, 0.0]
        ));
    }

    #[test]
    fn multiply_and_subtract_over_nothing_are_nothing() {
        for mode in [BlendMode::Multiply, BlendMode::Subtract, BlendMode::Min] {
            assert!(
                close(over(NOTHING, ([1.0; 3], 1.0), mode), [0.0; 3]),
                "{mode:?}"
            );
        }
        assert!(close(
            over((RED, 0.8), ([1.0; 3], 0.5), BlendMode::Multiply),
            [0.4, 0.0, 0.0]
        ));
    }

    #[test]
    fn replace_is_the_top_light() {
        // A dim blue over a bright red is that dim blue: no red left in it.
        assert!(close(
            over((RED, 1.0), (BLUE, 0.25), BlendMode::Replace),
            [0.0, 0.0, 0.25]
        ));
    }

    #[test]
    fn replace_black_paints_black() {
        assert!(close(
            over((RED, 1.0), (BLUE, 0.0), BlendMode::Replace),
            [0.0; 3]
        ));
        assert!(close(
            over(NOTHING, (BLUE, 0.5), BlendMode::Replace),
            [0.0, 0.0, 0.5]
        ));
        assert!(close(
            over((RED, 0.0), (BLUE, 0.5), BlendMode::Replace),
            [0.0, 0.0, 0.5]
        ));
    }

    #[test]
    fn alpha_mixes_the_blend_with_the_light_below() {
        let modes = BlendMode::LIGHT;
        for mode in modes {
            // Alpha 0 shows the light below exactly; alpha 1 is the blend.
            let below = (RED, 0.6);
            let top = (BLUE, 0.8);
            let none = blend_light_alpha(below.0, below.1, top.0, top.1, mode, 0.0);
            assert_eq!(none, below, "{mode:?}");
            let full = blend_light_alpha(below.0, below.1, top.0, top.1, mode, 1.0);
            assert_eq!(full, blend_light(below.0, below.1, top.0, top.1, mode));
            // Half way the light is half way between the two.
            let half = light(blend_light_alpha(below.0, below.1, top.0, top.1, mode, 0.5));
            let (a, b) = (light(below), light(full));
            assert!(
                close(half, std::array::from_fn(|c| (a[c] + b[c]) / 2.0)),
                "{mode:?}"
            );
        }
        assert_eq!(blend_value_alpha(0.2, 0.9, BlendMode::Replace, 0.0), 0.2);
        assert!((blend_value_alpha(0.2, 0.9, BlendMode::Replace, 0.5) - 0.55).abs() < 1e-6);
    }

    #[test]
    fn a_dark_top_leaves_the_light_under_it() {
        for mode in [
            BlendMode::Add,
            BlendMode::Screen,
            BlendMode::Max,
            BlendMode::Subtract,
        ] {
            assert!(
                close(over((RED, 0.6), (BLUE, 0.0), mode), [0.6, 0.0, 0.0]),
                "{mode:?}"
            );
        }
    }
}
