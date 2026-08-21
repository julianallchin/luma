//! Adapted from zeron (MIT, © 2026 Wing) — crates/ui/src/theme.rs
//!
//! The chat surface's *semantic* palette: the named roles ([`Theme`]) and the
//! syntax tones ([`SyntaxPalette`]) the markdown renderer paints with. The
//! primitives underneath them — the glass tier, the wash/ink/hairline inks,
//! the oklch math — live in [`luma_ui::glass`], because the shell's chrome
//! paints with the same tier and one palette cannot have two homes.
//!
//! Motto, kept from the source: **numbers drive layout, colors are paint.**
//! Every layout constant in this crate is a plain number and none of them
//! depend on which color is painted.

use gpui::{hsla, Hsla, SharedString};

pub use luma_ui::glass::{
    card_bg, generation as glass_generation, glass, glass_hover, grey, hairline, ink, neutral,
    oklch, scrim, wash, window_background_appearance, GLASS_ALPHA, SCRIM_ALPHA,
};

use crate::syntax::TokenKind;
/// Paint-only colors for fenced code.
///
/// A separate struct from [`Theme`] because these are the only tokens with a
/// *lookup* — [`Self::color`] is the whole seam between the lexer's closed
/// vocabulary and the palette, and it is the reason no other file in the crate
/// needs to know either half.
///
/// Hues are zeron's: indigo / emerald / amber / pink at 72% of their nominal
/// saturation, which is what keeps six tones on a near-black ground from
/// reading as six highlighter pens.
#[derive(Debug, Clone)]
pub struct SyntaxPalette {
    pub comment: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub constant: Hsla,
    pub type_name: Hsla,
    pub function: Hsla,
    pub property: Hsla,
    /// Identifiers and operators — code's body copy, and the majority of it.
    pub plain: Hsla,
}

impl SyntaxPalette {
    /// What one token paints in.
    #[must_use]
    pub fn color(&self, kind: TokenKind) -> Hsla {
        match kind {
            TokenKind::Comment => self.comment,
            TokenKind::Keyword => self.keyword,
            TokenKind::String => self.string,
            TokenKind::Number => self.number,
            TokenKind::Constant => self.constant,
            TokenKind::Type => self.type_name,
            TokenKind::Function => self.function,
            TokenKind::Property => self.property,
            TokenKind::Plain => self.plain,
        }
    }

    /// The one appearance, over [`Theme::dark`]'s code surface.
    #[must_use]
    pub fn dark() -> Self {
        let tone = |color: Hsla| Hsla {
            s: color.s * 0.72,
            ..color
        };
        let indigo = tone(oklch(0.673, 0.182, 276.935));
        let emerald = tone(oklch(0.765, 0.177, 163.223));
        let amber = tone(oklch(0.828, 0.189, 84.429));
        let pink = tone(oklch(0.718, 0.202, 349.761));
        Self {
            comment: neutral(0.60),
            keyword: indigo,
            string: emerald,
            number: amber,
            constant: emerald,
            type_name: amber,
            function: pink,
            property: amber,
            plain: neutral(0.86),
        }
    }
}

/// The chat surface's tokens.
///
/// A struct rather than free functions for the *palette* — a token is a design
/// decision with a name, and a call site that reaches for `oklch(…)` directly
/// has minted a second palette. The context-free helpers above are the
/// exception, and only because they are called from element builders that have
/// no theme in scope.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The transcript's plane: the deepest surface in the panel.
    pub bg: Hsla,
    /// Shell around the transcript — header, composer gutter.
    pub surface: Hsla,
    /// Raised pills and chips that sit proud of the panel.
    pub surface_raised: Hsla,
    /// A floating card: menu, popover.
    pub surface_overlay: Hsla,
    /// Hover wash for interactive rows and buttons.
    pub element_hover: Hsla,
    /// Active / pressed wash.
    pub element_active: Hsla,
    /// Hairline border.
    pub border: Hsla,
    /// Stronger border for focused or raised edges.
    pub border_strong: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Timestamps and secondary labels.
    pub text_muted: Hsla,
    /// Placeholders and disabled copy.
    pub text_faint: Hsla,
    /// The composer plate.
    pub input_bg: Hsla,
    /// Accent — indigo. Bullets, quote rails, selection.
    pub accent: Hsla,
    /// Stronger accent for fills that carry a label.
    pub accent_strong: Hsla,
    /// Errors, and the stop button.
    pub danger: Hsla,
    /// Amber: a tool call still running.
    pub warning: Hsla,
    /// Emerald: a tool call that succeeded.
    pub success: Hsla,
    /// Pink: the working indicator.
    pub busy: Hsla,
    /// Inline-code and code-block text.
    pub code_text: Hsla,
    /// The wash behind an inline-code pill.
    pub code_wash: Hsla,
    /// Fenced code's tokens. Paint only: a block with no highlighter, or in a
    /// language the lexer does not know, paints entirely in [`Self::code_text`]
    /// at exactly the same size and metrics.
    pub syntax: SyntaxPalette,
    /// Body face. Luma's own, not zeron's Geist — the fonts are separately
    /// licensed and this port does not carry them.
    pub font_sans: SharedString,
    pub font_mono: SharedString,
}

impl Theme {
    /// The one appearance. Surfaces are zeron's sampled greys.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            bg: grey(6),
            surface: grey(13),
            surface_raised: neutral(0.235),
            surface_overlay: grey(0x16),
            element_hover: hsla(0.0, 0.0, 0.92, 0.11),
            element_active: hsla(0.0, 0.0, 0.92, 0.16),
            border: hsla(0.0, 0.0, 1.0, 0.08),
            border_strong: hsla(0.0, 0.0, 1.0, 0.14),
            text: neutral(0.922),
            text_muted: neutral(0.708),
            text_faint: neutral(0.556),
            input_bg: hsla(0.0, 0.0, 1.0, 0.03),
            accent: oklch(0.673, 0.182, 276.935),
            accent_strong: oklch(0.585, 0.233, 277.117),
            danger: oklch(0.704, 0.191, 22.216),
            warning: oklch(0.828, 0.189, 84.429),
            success: oklch(0.765, 0.177, 163.223),
            busy: oklch(0.718, 0.202, 349.761),
            code_text: oklch(0.811, 0.111, 293.571),
            code_wash: oklch(0.702, 0.183, 293.541).opacity(0.12),
            syntax: SyntaxPalette::dark(),
            font_sans: luma_font_sans(),
            font_mono: system_mono().into(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Luma's UI face. Named here rather than taken from `luma-ui` because this
/// crate deliberately does not depend on the brutalist surface — see the
/// module docs.
fn luma_font_sans() -> SharedString {
    "Inter".into()
}

fn system_mono() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}
