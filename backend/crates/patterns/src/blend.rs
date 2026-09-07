use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    Replace,
    Add,
    Multiply,
    Screen,
    Max,
    Min,
    Lighten,
    Value,
    Subtract,
}

impl BlendMode {
    /// Every mode, in the order every picker lists them. The one canonical
    /// list — the score DSL, the track-edit hasher, and both hosts' blend
    /// selects all read it from here rather than keeping a spelling of their
    /// own.
    pub const ALL: [Self; 9] = [
        Self::Replace,
        Self::Add,
        Self::Multiply,
        Self::Screen,
        Self::Max,
        Self::Min,
        Self::Lighten,
        Self::Value,
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
            Self::Lighten => "lighten",
            Self::Value => "value",
            Self::Subtract => "subtract",
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
        BlendMode::Add => (base + top).min(1.0),
        BlendMode::Multiply => base * top,
        BlendMode::Screen => 1.0 - (1.0 - base) * (1.0 - top),
        BlendMode::Max => base.max(top),
        BlendMode::Min => base.min(top),
        BlendMode::Lighten => base.max(top),
        // "Value": top's own brightness acts as its opacity over the base.
        BlendMode::Value => top * top + base * (1.0 - top),
        BlendMode::Subtract => (base - top).max(0.0),
    }
}

/// Blend an RGB triple with an explicit top opacity. Ported from
/// `compositor::blend_color`: in `Value` mode the top's luminance modulates its
/// opacity; otherwise each component blends by `mode`. Then a standard
/// `over` alpha composite using `top_a`.
#[inline]
pub fn blend_color(base: [f32; 3], top: [f32; 3], top_a: f32, mode: BlendMode) -> [f32; 3] {
    let (br, bg, bb) = (base[0], base[1], base[2]);
    let (tr, tg, tb) = (top[0], top[1], top[2]);

    let (blended_r, blended_g, blended_b) = if matches!(mode, BlendMode::Value) {
        let top_lum = 0.299 * tr + 0.587 * tg + 0.114 * tb;
        (
            tr * top_lum + br * (1.0 - top_lum),
            tg * top_lum + bg * (1.0 - top_lum),
            tb * top_lum + bb * (1.0 - top_lum),
        )
    } else {
        (
            blend_value(br, tr, mode),
            blend_value(bg, tg, mode),
            blend_value(bb, tb, mode),
        )
    };

    [
        blended_r * top_a + br * (1.0 - top_a),
        blended_g * top_a + bg * (1.0 - top_a),
        blended_b * top_a + bb * (1.0 - top_a),
    ]
}
