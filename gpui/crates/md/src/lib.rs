//! Streaming markdown, for the agent chat.
//!
//! # Why this is a port, not a small renderer
//!
//! Rendering settled markdown is easy. Rendering markdown *as it arrives*,
//! without the text you already painted moving, is not — and it needs three
//! non-obvious things at once:
//!
//! - a **block-incremental parse** ([`parser`]) that reparses only from the
//!   last stable top-level block, so cost is O(delta), not O(document);
//! - **hanging-marker mending** ([`mend`]), which auto-closes a dangling `**`
//!   or `[link](` in the *display* parse only, so the real closing marker
//!   arriving later never reflows painted text;
//! - a **veil** ([`veil`]) that fades newly arrived characters by multiplying
//!   alpha into their `TextRun` colors — layout-safe because cosmic-text's
//!   `Attrs::compatible` ignores color, so a color-only run split shapes
//!   byte-identically to the unsplit render.
//!
//! Their hard parts are the invariants, not the code. This crate is those
//! files, ported from zeron (MIT, © 2026 Wing) with the notice in
//! `THIRD_PARTY/zeron-MIT.txt` and a header on every lifted file.
//!
//! # What changed in the port
//!
//! - Syntax highlighting is behind [`Highlighter`]. zeron calls a Tree-sitter
//!   crate we do not have, so [`syntax`] is a lexer instead — enough to tell a
//!   comment from a string from a keyword, and honest about the languages it
//!   does not know. Highlighting is pure paint by design, so which of the two
//!   is behind the seam changes no layout.
//! - [`theme`] is dark-only, and is the chat surface's palette — not Luma's
//!   brutalist ladder. See its module docs.
//! - The code block's copy button is drawn from `div`s rather than from an
//!   icon set: it was the only thing in the source's renderer that reached for
//!   one, and two overlapping squares are not worth a dependency.

pub mod mend;
pub mod parser;
pub mod render;
pub mod selection;
pub mod syntax;
pub mod theme;
pub mod veil;

use std::ops::Range;
use std::sync::Arc;

pub use parser::{parse_full, Block, BlockTree, IncrementalParser, InlineRun, InlineStyle};
pub use render::{render_tree, RenderCache, RenderOptions};
pub use syntax::{Syntax, TokenKind};
pub use theme::Theme;
pub use veil::TurnVeil;

/// One painted token: a byte range within its line, and the color to paint it.
///
/// A color rather than a token *kind*: highlighting is pure paint, so a color
/// is the whole of what the renderer needs, and a kind would drag a palette
/// and an enum across the seam for no reader.
#[derive(Clone, Debug, PartialEq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub color: gpui::Hsla,
}

/// One fenced block's tokens, per line of its source.
#[derive(Clone, Debug, Default)]
pub struct HighlightedCode {
    pub lines: Vec<Vec<HighlightSpan>>,
}

/// Colors for fenced code.
///
/// Behind a trait because highlighting must stay optional: the renderer
/// composes runs on the identical mono font whether or not spans arrive, so a
/// highlighter can be added, replaced or removed without a relayout.
pub trait Highlighter {
    /// Tokens for one fenced block, or `None` to paint it plain.
    ///
    /// Returns an `Arc` because the renderer caches per-line runs across
    /// frames and keys them on the returned allocation's identity — a
    /// highlighter that re-derives hands back a fresh `Arc` and the cache
    /// rebuilds; one that has not moved hands back the same one and it does
    /// not.
    fn highlight(&self, language: Option<&str>, code: &str) -> Option<Arc<HighlightedCode>>;
}

/// The default: code paints in the body color on the mono face.
pub struct NoHighlight;

impl Highlighter for NoHighlight {
    fn highlight(&self, _language: Option<&str>, _code: &str) -> Option<Arc<HighlightedCode>> {
        None
    }
}
