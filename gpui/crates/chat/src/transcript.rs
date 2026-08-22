//! One conversation, as rows.
//!
//! # Two states, one source
//!
//! The content lives in `luma_lib`'s [`Transcript`] and is *not* mirrored here.
//! What this module adds beside it is **render** state — a parser and a veil
//! per text part — because a mirrored transcript is the classic
//! two-sources-of-truth bug and the fold that maintains the real one already
//! lives a crate down.
//!
//! [`Row::sync`] is what keeps them in step: it hands each part's current text
//! to that part's [`IncrementalParser`], which is O(delta) when the text only
//! grew. The parser's source is therefore always exactly the transcript's, and
//! there is no append path that could drift from the fold.
//!
//! # Streaming
//!
//! The live row renders its parser's *display* tree — hanging `**` and
//! `[link](` auto-closed — so the real closing marker never reflows painted
//! text, and passes its [`luma_md::RowVeil`] so newly arrived characters fade in by
//! paint alone. Settled rows render the canonical tree with neither.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use gpui::{div, prelude::*, px, AnyElement, Entity, SharedString, Window};
use luma_lib::agent::{AgentChatMessage, AgentChatPart, Role, Transcript};
use luma_md::render::{MD_LINE_HEIGHT, MD_TEXT_SIZE};
use luma_md::{Block, BlockTree, IncrementalParser, RenderCache, RenderOptions, Syntax};
use luma_ui::node::{Instrument, Role as NodeRole};

use crate::chip;
use crate::theme::{self, Theme};
use crate::AgentChat;

/// What a row needs to know beyond its own content: where it sits, what the
/// panel is doing, and how to talk back to it.
///
/// Bundled rather than passed as five arguments because every one of them is
/// the *panel's* state, not the row's — a row that took them individually
/// would grow a parameter every time the panel learned something new.
pub struct RowCtx<'a> {
    /// The panel, for the one thing a row does to it: open or close a chip.
    pub chat: &'a Entity<AgentChat>,
    /// This row's index, which is what a toggle has to remeasure.
    pub ix: usize,
    /// Whether the turn is writing into this row.
    pub live: bool,
    /// The working indicator, on the one row that carries it — the last. See
    /// [`crate::working`] for why it trails a row rather than pinning to the
    /// panel.
    pub trailer: Option<crate::working::Trailer>,
    /// Tool calls the reader has closed, by call id — see
    /// [`crate::AgentChat::toggle_tool`] for why the set is the negative one.
    pub collapsed: &'a HashSet<SharedString>,
    pub theme: &'a Theme,
}

impl RowCtx<'_> {
    /// Whether this call's detail is showing.
    #[must_use]
    pub fn is_expanded(&self, call_id: &str) -> bool {
        !self.collapsed.contains(call_id)
    }
}

/// One message's render state.
pub struct Row {
    /// Stable across frames and unique in the transcript: element ids, the
    /// render cache and the veil are all keyed by it.
    key: SharedString,
    /// A parser per part, by part index. `None` for a part that carries no
    /// markdown (a tool call, a step marker).
    parsers: Vec<Option<IncrementalParser>>,
    veil: Rc<RefCell<luma_md::veil::RowVeil>>,
    cache: Rc<RefCell<RenderCache>>,
    /// When the panel saw this row arrive. `None` for restored history — the
    /// durable transcript does not carry times yet, and a stamped guess would
    /// be a timestamp that lies. Rendered under a settled assistant turn.
    time: Option<chrono::DateTime<chrono::Local>>,
}

impl Row {
    /// A row for a message that arrived over the wire — its text fades in,
    /// and its arrival is the turn's timestamp.
    pub fn streaming(id: &str) -> Self {
        let mut row = Self::with_veil(id, luma_md::veil::RowVeil::default());
        row.time = Some(chrono::Local::now());
        row
    }

    /// A row read back from storage. Seeded, so history does not dissolve onto
    /// the screen the first time the panel opens.
    pub fn restored(id: &str) -> Self {
        Self::with_veil(id, luma_md::veil::RowVeil::seeded())
    }

    fn with_veil(id: &str, veil: luma_md::veil::RowVeil) -> Self {
        Self {
            key: SharedString::from(id.to_string()),
            parsers: Vec::new(),
            veil: Rc::new(RefCell::new(veil)),
            cache: Rc::new(RefCell::new(RenderCache::default())),
            time: None,
        }
    }

    /// Bring the render state up to the message's current content.
    ///
    /// Cheap to call on every fold: [`IncrementalParser::set_text`] reparses
    /// only from the last stable top-level block when the text grew, and does
    /// nothing at all when it did not.
    pub fn sync(&mut self, message: &AgentChatMessage) {
        if self.parsers.len() < message.parts.len() {
            self.parsers.resize_with(message.parts.len(), || None);
        }
        for (ix, part) in message.parts.iter().enumerate() {
            let Some(text) = markdown_of(part) else {
                continue;
            };
            let parser = self.parsers[ix].get_or_insert_with(IncrementalParser::new);
            parser.set_text(text);
            // The render cache is keyed by position, so the blocks the parser
            // just reparsed have to be dropped from it by hand — see
            // `RenderCache::invalidate_from`. Doing it here, next to the
            // reparse, is what keeps the two from disagreeing.
            let stable = parser.stable_prefix_blocks();
            self.cache
                .borrow_mut()
                .invalidate_from(&part_key(&self.key, ix), stable);
        }
    }

    /// Stop seeding: from here on, appended text fades.
    pub fn finish_restoring(&self) {
        self.veil.borrow_mut().finish_seeding();
    }

    /// Whether any of this row's text is still dissolving in, which is the one
    /// reason the panel asks for another frame.
    pub fn is_fading(&self) -> bool {
        self.veil.borrow().is_fading()
    }

    /// Everything this row paints, as plain text — what the automation tree
    /// reports for it. Derived from the parsed tree rather than from the
    /// source so the node says what is on screen, not what the markdown said.
    pub fn plain_text(&self, message: &AgentChatMessage) -> String {
        let mut out = String::new();
        for (ix, part) in message.parts.iter().enumerate() {
            match part {
                AgentChatPart::Text { .. } | AgentChatPart::Reasoning { .. } => {
                    let Some(parser) = self.parsers.get(ix).and_then(Option::as_ref) else {
                        continue;
                    };
                    push_plain(&mut out, parser.tree());
                }
                AgentChatPart::Tool(tool) => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&chip::label(tool));
                }
                _ => {}
            }
        }
        out
    }
}

/// The cache and element-id key for one part of one row. Spelled once: the
/// key that invalidates and the key that renders have to be the same string.
fn part_key(row: &SharedString, part: usize) -> String {
    format!("{row}:{part}")
}

/// The markdown a part carries, if it carries any.
fn markdown_of(part: &AgentChatPart) -> Option<&str> {
    match part {
        AgentChatPart::Text { text } | AgentChatPart::Reasoning { text, .. } => Some(text),
        _ => None,
    }
}

/// Flatten a parsed tree back to the prose it paints, one block per line.
fn push_plain(out: &mut String, tree: &BlockTree) {
    for top in &tree.blocks {
        push_block_plain(out, &top.block);
    }
}

fn push_block_plain(out: &mut String, block: &Block) {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => {
            if !out.is_empty() {
                out.push('\n');
            }
            for run in runs {
                out.push_str(&run.text);
            }
        }
        Block::CodeBlock { code, .. } => {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(code);
        }
        Block::BlockQuote { children } => {
            for child in children {
                push_block_plain(out, child);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for child in item {
                    push_block_plain(out, child);
                }
            }
        }
        Block::Table { header, rows, .. } => {
            for cells in std::iter::once(header).chain(rows.iter()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                for (ix, cell) in cells.iter().enumerate() {
                    if ix > 0 {
                        out.push('\t');
                    }
                    for run in cell {
                        out.push_str(&run.text);
                    }
                }
            }
        }
        Block::Rule => {}
    }
}

/// Render one message.
///
/// `live` is the row a turn is currently writing into: it renders the display
/// parse and fades its new characters. Every other row is settled and renders
/// neither, which is also what makes it free — a settled row's flatten and
/// shaping are reused from the cache untouched.
pub fn row(
    row: &Row,
    message: &AgentChatMessage,
    ctx: &RowCtx,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let body = match message.role {
        Role::User => user_bubble(message, ctx.theme),
        Role::Assistant => assistant(row, message, ctx, window),
    };
    let view = ctx.chat.entity_id();
    let trailer = ctx
        .trailer
        .as_ref()
        .map(|state| crate::working::trailer(state, ctx.theme, view, cx));
    // A settled assistant turn is signed with when it arrived — comet's faint
    // line under the reply. A live row is not: its time is "now".
    let stamp = (!ctx.live && matches!(message.role, Role::Assistant))
        .then_some(row.time)
        .flatten()
        .map(|time| {
            div()
                .pt(px(theme::SPACE_XS))
                .text_size(px(11.0))
                .text_color(ctx.theme.text_faint)
                .child(SharedString::from(
                    time.format("%b %-d, %-I:%M %p").to_string(),
                ))
        });
    // The reading column: the `list` hands every item the pane's full width,
    // so the 736 cap and the centering live on the row itself.
    div()
        .w_full()
        .pb(px(theme::GAP_TURN))
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(theme::MAX_CONTENT_WIDTH))
                .flex()
                .flex_col()
                .child(body)
                .children(trailer)
                .children(stamp),
        )
        .agent_node(NodeRole::Text, row.plain_text(message))
        .into_any_element()
}

/// The user's turn: a translucent plate, right-aligned, never full width.
///
/// Translucent rather than opaque so the panel reads through it — an opaque
/// plate on these near-black surfaces reads as a slab.
fn user_bubble(message: &AgentChatMessage, theme: &Theme) -> AnyElement {
    let text: String = message
        .parts
        .iter()
        .filter_map(markdown_of)
        .collect::<Vec<_>>()
        .join("\n");
    div()
        .w_full()
        .flex()
        .flex_row()
        .justify_end()
        .child(
            div()
                .max_w(px(theme::MAX_CONTENT_WIDTH * 0.75))
                .px(px(theme::SPACE_MD))
                .py(px(theme::SPACE_SM))
                .rounded(px(theme::BUBBLE_RADIUS))
                .bg(theme::wash(0.08))
                .text_size(px(MD_TEXT_SIZE))
                .line_height(px(MD_LINE_HEIGHT))
                .text_color(theme.text)
                .child(SharedString::from(text)),
        )
        .into_any_element()
}

fn assistant(state: &Row, message: &AgentChatMessage, ctx: &RowCtx, window: &Window) -> AnyElement {
    let now = Instant::now();
    let theme = ctx.theme;
    let mut stack = div().w_full().flex().flex_col().gap(px(theme::GAP_BLOCK));
    // Consecutive tool calls render as one rail — comet's group — so the
    // grouping is decided here, where the sequence is visible, and the rail
    // only ever draws what it is handed.
    let mut tools: Vec<&luma_lib::agent::ToolPart> = Vec::new();
    let mut parts = message.parts.iter().enumerate().peekable();
    while let Some((ix, part)) = parts.next() {
        if let AgentChatPart::Tool(tool) = part {
            tools.push(tool);
            let group_continues = matches!(parts.peek(), Some((_, AgentChatPart::Tool(_))));
            if !group_continues {
                stack = stack.child(chip::rail(&tools, ctx));
                tools.clear();
            }
            continue;
        }
        let element = match part {
            AgentChatPart::Text { .. } => markdown(state, ix, now, ctx, window, theme.text),
            AgentChatPart::Reasoning { .. } => {
                markdown(state, ix, now, ctx, window, theme.text_faint)
            }
            _ => None,
        };
        if let Some(element) = element {
            stack = stack.child(element);
        }
    }
    stack.into_any_element()
}

/// One markdown part, at the tone its kind is painted in.
fn markdown(
    state: &Row,
    ix: usize,
    now: Instant,
    ctx: &RowCtx,
    window: &Window,
    color: gpui::Hsla,
) -> Option<AnyElement> {
    let (live, theme) = (ctx.live, ctx.theme);
    let parser = state.parsers.get(ix)?.as_ref()?;
    if parser.source().is_empty() {
        return None;
    }
    // The *display* parse while streaming: hanging markers are auto-closed for
    // display only, so the real closing marker never reflows painted text. The
    // canonical parse settles honestly when the row does.
    let tree = if live {
        parser.display_tree()
    } else {
        parser.tree().clone()
    };
    let opts = RenderOptions {
        row_key: SharedString::from(part_key(&state.key, ix)),
        veil: live.then(|| Rc::clone(&state.veil)),
        cache: Some(Rc::clone(&state.cache)),
        now,
    };
    Some(
        div()
            .w_full()
            .text_color(color)
            .child(luma_md::render_tree(
                &tree,
                &opts,
                theme,
                window,
                &Syntax::new(theme),
            ))
            .into_any_element(),
    )
}

/// The row a turn is writing into, if any: the last assistant row.
pub fn live_row(transcript: &Transcript, streaming: bool) -> Option<usize> {
    if !streaming {
        return None;
    }
    let last = transcript.messages.len().checked_sub(1)?;
    matches!(transcript.messages[last].role, Role::Assistant).then_some(last)
}
