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
//! [`Entry::sync`] is what keeps them in step: it hands each part's current text
//! to that part's [`IncrementalParser`], which is O(delta) when the text only
//! grew. The parser's source is therefore always exactly the transcript's, and
//! there is no append path that could drift from the fold.
//!
//! # A row is a block, not a message
//!
//! The virtualized list is sized in **blocks**: a user turn is one bubble row,
//! an assistant text part becomes one row per top-level markdown block, and a
//! run of consecutive tool calls folds into one group row. [`RowKey`] is the
//! whole of a row's identity — where it came from, plus a content hash.
//!
//! This is what makes streaming flat-cost. A message-per-row list must
//! remeasure the entire reply on every commit, so the cost of one token grows
//! with the length of the answer. With block rows only the *tail* rows' hashes
//! move, so [`diff_rows`] touches O(changed rows) and every settled row keeps
//! both its measured height and its render cache untouched.
//!
//! The one subtlety worth stating: when the diff finds the same row *count*, it
//! must remeasure rather than splice. `splice` resets items to hint-less
//! `Unmeasured` and clobbers the scroll anchor when the viewport's top item is
//! inside the range — which is exactly the end-of-turn jump, because the
//! live→settled flip changes every row's version (the streaming bit) while
//! every id stays put.
//!
//! # Streaming
//!
//! A live turn renders its parser's *display* tree — hanging `**` and
//! `[link](` auto-closed — so the real closing marker never reflows painted
//! text, and passes its [`luma_md::TurnVeil`] so newly arrived characters fade
//! in by paint alone. Settled entries render the canonical tree with neither.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{div, prelude::*, px, AnyElement, Entity, SharedString, Window};
use luma_lib::agent::{AgentChatMessage, AgentChatPart, Role, ToolPart, Transcript};
use luma_md::render::{render_block, MD_LINE_HEIGHT, MD_TEXT_SIZE};
use luma_md::{Block, BlockTree, IncrementalParser, RenderCache, RenderOptions, Syntax};
use luma_ui::node::{Instrument, Role as NodeRole};

use crate::chip;
use crate::theme::{self, Theme};
use crate::AgentChat;

// -- the row model (pure) ----------------------------------------------------

/// What one row of the list shows.
///
/// Deliberately positional rather than string-keyed: a row's identity is where
/// it sits in the transcript, and the transcript only ever appends, so a pair
/// of indices is a stable name. Strings would buy nothing and allocate per row
/// per frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A user turn, whole — one bubble, however long the prompt is.
    Prompt,
    /// The session turn a non-conversational client opened. One line, in the
    /// recessive tone: it is a marker in the record, not something anyone
    /// said, so it must not read as a prompt.
    Session,
    /// One top-level markdown block of one part.
    Block {
        part: usize,
        block: usize,
        /// Reasoning renders in the recessive tone. Part of the *kind* because
        /// it changes what the row paints, so it belongs to the row's identity.
        reasoning: bool,
    },
    /// A run of consecutive tool calls, as one group.
    Tools { part: usize, count: usize },
}

/// A row's whole identity: which turn, which block of it, and a hash of what
/// that block currently says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowKey {
    /// Index into the transcript's messages.
    pub turn: usize,
    pub kind: RowKind,
    /// FNV-1a over what the row paints, shifted up one with the low bit set
    /// while its turn is still streaming.
    ///
    /// The streaming bit is what makes the live→settled flip visible to
    /// [`diff_rows`]: at that moment no text changes, but every row of the turn
    /// stops fading, gains a timestamp lane and switches from the display parse
    /// to the canonical one. Without the bit the list would keep heights
    /// measured under the veil.
    pub version: u64,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x1_0000_01b3;

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Fold the streaming flag into a content hash without losing any of it.
///
/// A shift rather than masking the low bit: masking would silently collide two
/// blocks whose hashes differ only there, and a version collision is a row that
/// never remeasures.
fn version(hash: u64, streaming: bool) -> u64 {
    (hash << 1) | u64::from(streaming)
}

/// The rows a whole transcript renders as.
///
/// `live` is the message a turn is currently writing into, if any — the only
/// one whose rows carry the streaming bit.
#[must_use]
pub fn rows_for(transcript: &Transcript, entries: &[Entry], live: Option<usize>) -> Vec<RowKey> {
    let mut rows = Vec::new();
    for (ix, message) in transcript.messages.iter().enumerate() {
        let streaming = live == Some(ix);
        let Some(turn) = entries.get(ix) else {
            continue;
        };
        match message.role {
            Role::User => rows.push(RowKey {
                turn: ix,
                kind: RowKind::Prompt,
                version: version(
                    fnv1a(FNV_OFFSET, prompt_text(message).as_bytes()),
                    streaming,
                ),
            }),
            Role::Assistant => push_assistant_rows(&mut rows, ix, message, turn, streaming),
            Role::Session => rows.push(RowKey {
                turn: ix,
                kind: RowKind::Session,
                version: version(
                    fnv1a(FNV_OFFSET, prompt_text(message).as_bytes()),
                    streaming,
                ),
            }),
        }
    }
    rows
}

fn push_assistant_rows(
    rows: &mut Vec<RowKey>,
    ix: usize,
    message: &AgentChatMessage,
    turn: &Entry,
    streaming: bool,
) {
    let mut parts = message.parts.iter().enumerate().peekable();
    while let Some((part, item)) = parts.next() {
        if let AgentChatPart::Tool(_) = item {
            // Consecutive calls are one group. The run is measured here, where
            // the sequence is visible, so the rail only ever draws what it is
            // handed.
            let first = part;
            let mut count = 1;
            let mut hash = FNV_OFFSET;
            hash = hash_tool(hash, tool_at(message, part));
            while matches!(parts.peek(), Some((_, AgentChatPart::Tool(_)))) {
                let (next, _) = parts.next().expect("peeked");
                hash = hash_tool(hash, tool_at(message, next));
                count += 1;
            }
            rows.push(RowKey {
                turn: ix,
                kind: RowKind::Tools { part: first, count },
                version: version(hash, streaming),
            });
            continue;
        }
        let reasoning = matches!(item, AgentChatPart::Reasoning { .. });
        if markdown_of(item).is_none() {
            continue;
        }
        let Some(state) = turn.parts.get(part).and_then(Option::as_ref) else {
            continue;
        };
        for block in 0..state.tree.blocks.len() {
            rows.push(RowKey {
                turn: ix,
                kind: RowKind::Block {
                    part,
                    block,
                    reasoning,
                },
                version: version(state.block_hash(block), streaming),
            });
        }
    }
}

fn tool_at(message: &AgentChatMessage, part: usize) -> Option<&ToolPart> {
    match message.parts.get(part) {
        Some(AgentChatPart::Tool(tool)) => Some(tool),
        _ => None,
    }
}

/// A tool's contribution to its group's version: enough to catch a state
/// change and a growing output, and nothing that would rehash a whole payload
/// every frame.
fn hash_tool(seed: u64, tool: Option<&ToolPart>) -> u64 {
    let Some(tool) = tool else {
        return seed;
    };
    let mut hash = fnv1a(seed, tool.call_id.as_bytes());
    hash = fnv1a(hash, tool.state.as_str().as_bytes());
    for len in [
        tool.input
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
            .len(),
        tool.output
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
            .len(),
        tool.error_text.as_ref().map_or(0, String::len),
    ] {
        hash = fnv1a(hash, &(len as u64).to_le_bytes());
    }
    hash
}

/// The gap opening `row`, given the row above it.
///
/// Three gaps in priority order and nothing else. A block's spacing is decided
/// by the pair it sits between, never by what the block itself is — which is
/// why this is a free function over two rows rather than a property of one.
#[must_use]
pub fn top_gap_for(previous: Option<&RowKey>, row: &RowKey) -> f32 {
    let Some(previous) = previous else {
        return 0.0;
    };
    if previous.turn != row.turn {
        return theme::GAP_TURN;
    }
    let same_part_markdown = match (previous.kind, row.kind) {
        (RowKind::Block { part: a, .. }, RowKind::Block { part: b, .. }) => a == b,
        _ => false,
    };
    if same_part_markdown {
        // Exactly the markdown renderer's own inter-block gap: these two rows
        // were one element a moment ago and will be again on the next reparse,
        // so the split cannot be allowed to move anything.
        theme::GAP_TOOL
    } else if matches!(previous.kind, RowKind::Tools { .. })
        || matches!(row.kind, RowKind::Tools { .. })
    {
        theme::GAP_TOOL
    } else {
        theme::GAP_BLOCK
    }
}

/// The minimal edit turning `old` into `new`: `Some((old_range, new_count))`,
/// or `None` when the two are identical.
///
/// Trims the common prefix *and* suffix, so a commit that only touches the
/// tail reports a range covering the tail and nothing else. That bound is the
/// whole point: it is what keeps a settled prefix off the remeasure path no
/// matter how long the reply gets.
#[must_use]
pub fn diff_rows(old: &[RowKey], new: &[RowKey]) -> Option<(Range<usize>, usize)> {
    let mut prefix = 0;
    let max_prefix = old.len().min(new.len());
    while prefix < max_prefix && old[prefix] == new[prefix] {
        prefix += 1;
    }
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let mut suffix = 0;
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    while suffix < max_suffix && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    Some((prefix..old.len() - suffix, new.len() - suffix - prefix))
}

// -- the bottom pin (pure) ---------------------------------------------------

/// The stick-to-bottom spring.
///
/// A spring rather than a snap because the transcript's bottom edge is *moving*
/// while a reply streams. Snapping to a moving target every frame is what makes
/// a chat feel like it is yanking the page; a critically-damped chase with a
/// feed-forward term for the target's own velocity tracks it smoothly and lands
/// exactly once.
///
/// The feed-forward is the part that is easy to leave out and impossible to
/// fake: without it the spring is always behind a growing target by however far
/// it grew this frame, so it never converges and the text visibly lags the
/// bottom edge. `target_vel` is a smoothed estimate of that growth, added to
/// the position directly.
#[derive(Debug, Clone, Copy, Default)]
pub struct StickSpring {
    /// Velocity, px per 60fps frame.
    velocity: f32,
    /// Feed-forward: smoothed target growth, px per 60fps frame.
    target_vel: f32,
    /// The target at the previous tick. `None` while parked.
    last_target: Option<f32>,
}

impl StickSpring {
    /// Park it: the next tick starts cold.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether any residual motion is left worth scheduling a frame for.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.velocity < 0.05 && self.target_vel < 0.05
    }

    /// Advance one tick and return the new position.
    ///
    /// `pos` and `target` are scroll offsets in px, larger meaning closer to the
    /// bottom; `frames` is elapsed time expressed in 60fps frames. Never
    /// overshoots `target`, is monotone while approaching it, and snaps exactly
    /// once inside half a pixel — so "arrived" is a state it reaches rather
    /// than one it oscillates around.
    pub fn step(&mut self, mut pos: f32, target: f32, mut frames: f32) -> f32 {
        let grew = self.last_target.map_or(0.0, |last| target - last);
        self.last_target = Some(target);
        if grew < -1.0 {
            // The target shrank — a row collapsed or was removed, and the
            // growth estimate describes a world that no longer exists.
            self.target_vel = 0.0;
        } else {
            let observed = grew.max(0.0) / frames.max(0.25);
            self.target_vel += theme::SPRING_GROWTH_EMA * (observed - self.target_vel);
        }
        // Chase a point slightly *above* the true bottom, proportional to how
        // fast it is growing: hugging a moving edge leaves the newest line
        // half-painted at the boundary every frame.
        let chase = target - (self.target_vel * 9.0).min(theme::SPRING_CHASE_MAX_LEAD);
        let mut velocity = self.velocity;
        // Fixed-timestep sub-stepping: the integration is defined per 60fps
        // frame, so a 30fps display runs it twice rather than doubling a delta
        // and changing the curve.
        while frames > 0.0 {
            let step = frames.min(1.0);
            frames -= step;
            let diff = (chase - pos).max(0.0);
            velocity += step
                * ((theme::SPRING_DAMPING * velocity + theme::SPRING_STIFFNESS * diff)
                    / theme::SPRING_MASS
                    - velocity);
            pos = (pos + (velocity + self.target_vel) * step).min(target);
        }
        self.velocity = velocity;
        if target - pos <= 0.5 {
            target
        } else {
            pos
        }
    }
}

/// Whether a user scroll should re-engage the bottom pin.
///
/// Direction is half the rule and the half that is easy to miss: inside the
/// band alone would make the pin unbreakable, because a small wheel-up notch
/// near the bottom stays inside it and would snap the view straight back. The
/// reader has to be *moving toward* the bottom to be asking for the pin.
#[must_use]
pub fn should_restick(distance: f32, previous: f32) -> bool {
    distance <= theme::STICK_THRESHOLD_PX && distance < previous
}

/// Whether a user scroll should break the pin: any real movement away from the
/// bottom. The tolerance is what keeps a growing target — which momentarily
/// increases the distance every frame — from unpinning the view it is chasing.
#[must_use]
pub fn should_unpin(distance: f32, previous: f32) -> bool {
    distance > previous + theme::AT_BOTTOM_PX
}

// -- per-turn render state ---------------------------------------------------

/// One part's parse, and the tree that part currently renders as.
struct PartState {
    parser: IncrementalParser,
    /// What actually gets rendered: the *display* parse while the turn streams
    /// (hanging markers closed for display only), the canonical parse once it
    /// settles. Held rather than derived per frame because the row list and the
    /// row's own render must agree on the block count, and asking the parser
    /// twice would parse twice.
    tree: Arc<BlockTree>,
    /// Which of the two `tree` is, so a liveness flip rebuilds it.
    live: bool,
}

impl PartState {
    /// A block's content hash: the source bytes it was parsed from.
    ///
    /// The range is clamped because the display parse indexes the *mended*
    /// source, which is the real source plus any closers `mend` appended — so
    /// the final block's end can sit past the text the parser was given.
    ///
    /// Sliced as **bytes**, not as a `str`: a clamped end is not guaranteed to
    /// land on a character boundary, and `&source[start..end]` panics when it
    /// does not. A hash has no opinion about characters, so taking bytes is
    /// both correct and incapable of the panic.
    fn block_hash(&self, block: usize) -> u64 {
        let Some(top) = self.tree.blocks.get(block) else {
            return FNV_OFFSET;
        };
        let source = self.parser.source().as_bytes();
        let start = top.range.start.min(source.len());
        let end = top.range.end.clamp(start, source.len());
        fnv1a(FNV_OFFSET, &source[start..end])
    }
}

/// One message's render state.
pub struct Entry {
    /// Stable across frames and unique in the transcript. Never a key on its
    /// own: element ids, the render cache and the veil are all keyed by
    /// [`part_key`], because one message is several parts and each part's
    /// blocks are numbered from zero.
    key: SharedString,
    /// A parser and its tree per part, by part index. `None` for a part that
    /// carries no markdown (a tool call, a step marker).
    parts: Vec<Option<PartState>>,
    /// One veil for the whole turn, spanning every row it renders as — which is
    /// why the row key is half of what it is keyed by. A veil per part would be
    /// the same state split into pieces that [`Self::is_fading`] would have to
    /// re-join every frame.
    veil: Rc<RefCell<luma_md::veil::TurnVeil>>,
    cache: Rc<RefCell<RenderCache>>,
    /// When the panel saw this turn arrive. `None` for restored history — the
    /// durable transcript does not carry times yet, and a stamped guess would
    /// be a timestamp that lies. Rendered under a settled assistant turn.
    time: Option<chrono::DateTime<chrono::Local>>,
}

impl Entry {
    /// A turn that arrived over the wire — its text fades in, and its arrival
    /// is the turn's timestamp.
    pub fn streaming(id: &str) -> Self {
        let mut turn = Self::with_veil(id, luma_md::veil::TurnVeil::default());
        turn.time = Some(chrono::Local::now());
        turn
    }

    /// A turn read back from storage. Seeded, so history does not dissolve onto
    /// the screen the first time the panel opens.
    pub fn restored(id: &str) -> Self {
        Self::with_veil(id, luma_md::veil::TurnVeil::seeded())
    }

    fn with_veil(id: &str, veil: luma_md::veil::TurnVeil) -> Self {
        Self {
            key: SharedString::from(id.to_string()),
            parts: Vec::new(),
            veil: Rc::new(RefCell::new(veil)),
            cache: Rc::new(RefCell::new(RenderCache::default())),
            time: None,
        }
    }

    /// Bring the render state up to the message's current content.
    ///
    /// Cheap to call on every fold, and cheapest of all when nothing moved: a
    /// part whose text is unchanged and whose liveness is unchanged does no
    /// work at all. When it *has* changed, [`IncrementalParser::set_text`]
    /// reparses only from the last stable top-level block.
    pub fn sync(&mut self, message: &AgentChatMessage, live: bool) {
        if self.parts.len() < message.parts.len() {
            self.parts.resize_with(message.parts.len(), || None);
        }
        for (ix, part) in message.parts.iter().enumerate() {
            let Some(text) = markdown_of(part) else {
                continue;
            };
            let slot = &mut self.parts[ix];
            let Some(state) = slot else {
                let mut parser = IncrementalParser::new();
                parser.set_text(text);
                let tree = Arc::new(if live {
                    parser.display_tree()
                } else {
                    parser.tree().clone()
                });
                *slot = Some(PartState { parser, tree, live });
                continue;
            };
            // A whole-string compare, deliberately: text only ever appends
            // here, but a length check would call an in-place rewrite
            // unchanged, and this memcmp is noise beside the reparse it guards.
            let changed = state.parser.source() != text;
            let flipped = state.live != live;
            if changed {
                state.parser.set_text(text);
            }
            if changed || flipped {
                // The render cache is keyed by *position*, so anything the
                // parser just reparsed has to be dropped from it by hand — see
                // `RenderCache::invalidate_from`. Doing it here, next to the
                // reparse, is what keeps the two from disagreeing.
                //
                // A liveness flip counts even though no text moved: the tail
                // switches from the mended display parse to the canonical one,
                // and the cache is still holding the mended flatten under the
                // same key. Without this a reply that ended mid-marker keeps
                // its auto-closed rendering forever after the turn settles.
                let stable = state.parser.stable_prefix_blocks();
                self.cache
                    .borrow_mut()
                    .invalidate_from(&part_key(&self.key, ix), stable);
                state.tree = Arc::new(if live {
                    state.parser.display_tree()
                } else {
                    state.parser.tree().clone()
                });
                state.live = live;
            }
        }
    }

    /// Stop seeding: from here on, appended text fades.
    pub fn finish_restoring(&self) {
        self.veil.borrow_mut().finish_seeding();
    }

    /// Whether any of this turn's text is still dissolving in, which is the one
    /// reason the panel asks for another frame.
    pub fn is_fading(&self, now: Instant) -> bool {
        self.veil.borrow().is_fading(now)
    }
}

/// The cache and element-id key for one part of one turn. Spelled once: the
/// key that invalidates and the key that renders have to be the same string.
fn part_key(turn: &SharedString, part: usize) -> String {
    format!("{turn}:{part}")
}

/// The markdown a part carries, if it carries any.
fn markdown_of(part: &AgentChatPart) -> Option<&str> {
    match part {
        AgentChatPart::Text { text } | AgentChatPart::Reasoning { text, .. } => Some(text),
        _ => None,
    }
}

/// A user turn's prompt, as one string.
fn prompt_text(message: &AgentChatMessage) -> String {
    message
        .parts
        .iter()
        .filter_map(markdown_of)
        .collect::<Vec<_>>()
        .join("\n")
}

/// What one block paints, as plain text — what the automation tree reports for
/// it. Derived from the parsed tree rather than from the source so the node
/// says what is on screen, not what the markdown said.
fn block_plain_text(block: &Block) -> String {
    let mut out = String::new();
    push_block_plain(&mut out, block);
    out
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

// -- rendering ---------------------------------------------------------------

/// What a row needs to know beyond its own content: where it sits, what the
/// panel is doing, and how to talk back to it.
///
/// Bundled rather than passed as eight arguments because every one of them is
/// the *panel's* state, not the row's — a row that took them individually
/// would grow a parameter every time the panel learned something new.
pub struct RowCtx<'a> {
    /// The panel, for the one thing a row does to it: open or close a chip.
    pub chat: &'a Entity<AgentChat>,
    /// This row's index in the list, which is what a toggle has to remeasure.
    pub ix: usize,
    /// Whether the turn is writing into this row.
    pub live: bool,
    /// The gap above this row — see [`top_gap_for`].
    pub top_gap: f32,
    /// Whether this is its turn's last row: the one that carries the working
    /// trailer and the timestamp lane.
    pub last_of_turn: bool,
    /// The working indicator, on the one row that carries it — the last. See
    /// [`crate::working`] for why it trails a row rather than pinning to the
    /// panel.
    pub trailer: Option<crate::working::Trailer>,
    /// Tool calls the reader has closed, by call id — see
    /// [`crate::AgentChat::toggle_tool`] for why the set is the negative one.
    pub collapsed: &'a HashSet<SharedString>,
    /// Python calls, read. Interior-mutable because a row reads its call
    /// *during* the panel's own render, when the panel entity is already
    /// borrowed — see [`crate::python_cell`] for why the reading is cached at
    /// all.
    pub cells: &'a RefCell<crate::python_cell::Cells>,
    /// The one fold in flight, if any: which call, and how far through its
    /// tween it is. At most one, because a fold is started by a click and a
    /// click lands on one chip.
    pub fold: Option<(&'a SharedString, f32)>,
    pub theme: &'a Theme,
}

impl RowCtx<'_> {
    /// Whether this call's detail is showing.
    #[must_use]
    pub fn is_expanded(&self, call_id: &str) -> bool {
        !self.collapsed.contains(call_id)
    }

    /// How far through its fold this call is, or `None` when it is not moving.
    #[must_use]
    pub fn fold_progress(&self, call_id: &str) -> Option<f32> {
        self.fold
            .filter(|(call, _)| call.as_ref() == call_id)
            .map(|(_, progress)| progress)
    }
}

/// Render one row.
///
/// A live row renders the display parse and fades its new characters; a settled
/// one renders neither, which is also what makes it free — its flatten and
/// shaping are reused from the cache untouched.
pub fn row(
    key: &RowKey,
    turn: &Entry,
    message: &AgentChatMessage,
    ctx: &RowCtx,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let theme = ctx.theme;
    let (body, plain) = match key.kind {
        RowKind::Prompt => {
            let text = prompt_text(message);
            (user_bubble(&text, &turn.key, theme), text)
        }
        // Left-aligned and faint, deliberately unlike the user bubble beside
        // it: this line is a marker in the record, not something anyone said.
        RowKind::Session => {
            let text = prompt_text(message);
            (
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(px(MD_TEXT_SIZE))
                    .line_height(px(MD_LINE_HEIGHT))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(text.clone()))
                    .into_any_element(),
                text,
            )
        }
        RowKind::Tools { part, count } => {
            let tools: Vec<&ToolPart> = (part..part + count)
                .filter_map(|ix| tool_at(message, ix))
                .collect();
            let plain = tools
                .iter()
                .map(|tool| chip::label(tool).to_string())
                .collect::<Vec<_>>()
                .join("\n");
            (chip::rail(&tools, ctx, window), plain)
        }
        RowKind::Block {
            part,
            block,
            reasoning,
        } => {
            let color = if reasoning {
                theme.text_faint
            } else {
                theme.text
            };
            match markdown(turn, part, block, color, ctx, window) {
                Some(pair) => pair,
                None => (div().into_any_element(), String::new()),
            }
        }
    };
    let view = ctx.chat.entity_id();
    let trailer = ctx
        .trailer
        .as_ref()
        .map(|state| crate::working::trailer(state, theme, view, cx));
    // A settled assistant turn is signed with when it arrived — comet's faint
    // line under the reply, revealed on hover. A live turn is not signed at
    // all: a turn still being written is not at a time yet.
    //
    // The lane is reserved whether or not it holds anything, and the label
    // carries no inset of its own: its left edge has to land on the prose's
    // first character, and a padding here reads as a few pixels of drift.
    let group = SharedString::from(format!("turn-{}", turn.key));
    let stamp = (ctx.last_of_turn && !ctx.live && matches!(message.role, Role::Assistant))
        .then_some(turn.time)
        .flatten()
        .map(|time| SharedString::from(time.format("%b %-d, %-I:%M %p").to_string()));
    let lane = ctx.last_of_turn.then(|| {
        div()
            .h(px(theme::TIMESTAMP_LANE))
            .flex_none()
            .flex()
            .items_center()
            .children(stamp.map(|stamp| {
                div()
                    .opacity(0.0)
                    .group_hover(group.clone(), |style| style.opacity(1.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.55))
                    .child(stamp)
            }))
    });
    // The reading column: the `list` hands every item the pane's full width,
    // so the 736 cap and the centering live on the row itself.
    div()
        .group(group)
        .w_full()
        .pt(px(ctx.top_gap))
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(theme::MAX_CONTENT_WIDTH))
                .min_w_0()
                .flex()
                .flex_col()
                .child(body)
                .children(trailer)
                .children(lane),
        )
        .agent_node(NodeRole::Text, plain)
        .into_any_element()
}

/// The user's turn: a translucent plate, right-aligned, never full width.
///
/// Translucent rather than opaque so the panel reads through it — an opaque
/// plate on these near-black surfaces reads as a slab.
///
/// The prompt is selectable text like every assistant paragraph: the plate's
/// paint pass registers it into the frame's selection registry through the
/// same [`luma_md::render::paint_text_selection`] path the markdown rows take,
/// so a drag from a reply up through a prompt carries it and Cmd+C copies it.
fn user_bubble(text: &str, turn_key: &SharedString, theme: &Theme) -> AnyElement {
    let content: SharedString = text.to_string().into();
    let run = gpui::TextRun {
        len: content.len(),
        font: gpui::font(theme.font_sans.clone()),
        color: theme.text,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let styled = gpui::StyledText::new(content.clone()).with_runs(vec![run]);
    let layout = styled.layout().clone();
    let sel_key: Arc<str> = format!("{turn_key}:prompt").into();
    let paint_theme = theme.clone();
    let underlay = gpui::canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            luma_md::render::paint_text_selection(
                window,
                &sel_key,
                &content,
                &layout,
                &paint_theme,
            );
        },
    )
    .absolute()
    .size_full();
    div()
        .w_full()
        .flex()
        .flex_row()
        .justify_end()
        .child(
            div()
                .max_w(px(theme::MAX_CONTENT_WIDTH * theme::BUBBLE_WIDTH_SHARE))
                // Load-bearing, not tidiness: without it gpui's unwrapped
                // min-content width keeps the bubble from shrinking, and a long
                // prompt runs off the *left* edge of the column instead of
                // wrapping inside the plate.
                .min_w_0()
                .px(px(theme::SPACE_LG))
                .py(px(10.0))
                .rounded(px(luma_ui::radius::BUBBLE))
                .bg(theme::wash(0.08))
                .text_size(px(MD_TEXT_SIZE))
                .line_height(px(MD_LINE_HEIGHT))
                .text_color(theme.text)
                .child(div().relative().child(underlay).child(styled)),
        )
        .into_any_element()
}

/// One top-level markdown block, at the tone its kind is painted in.
///
/// Rendered through [`render_block`] with the *part's* cache key and the
/// block's own index, so the cache entries are exactly the ones a whole-tree
/// render would have produced — splitting the tree into rows changes what the
/// list measures, not what the renderer caches.
fn markdown(
    turn: &Entry,
    part: usize,
    block: usize,
    color: gpui::Hsla,
    ctx: &RowCtx,
    window: &Window,
) -> Option<(AnyElement, String)> {
    let state = turn.parts.get(part)?.as_ref()?;
    let top = state.tree.blocks.get(block)?;
    let opts = RenderOptions {
        row_key: SharedString::from(part_key(&turn.key, part)),
        veil: ctx.live.then(|| Rc::clone(&turn.veil)),
        cache: Some(Rc::clone(&turn.cache)),
        now: Instant::now(),
    };
    let syntax = Syntax::new(ctx.theme);
    let highlight = match &top.block {
        Block::CodeBlock { language, code } => {
            luma_md::Highlighter::highlight(&syntax, language.as_deref(), code)
        }
        _ => None,
    };
    let element = div()
        .w_full()
        .min_w_0()
        .text_color(color)
        .child(render_block(
            &top.block,
            block,
            block,
            &opts,
            ctx.theme,
            window,
            highlight.as_deref(),
        ))
        .into_any_element();
    Some((element, block_plain_text(&top.block)))
}

// Which turn is live is deliberately *not* derived here. "The last message, if
// it is an assistant one" is wrong for the window between a send and that
// turn's first event, where it names the PREVIOUS reply — which would re-veil
// settled text and remeasure rows nothing touched. `AgentChat` records it from
// the event that actually writes into a turn instead.

#[cfg(test)]
mod tests {
    use super::*;

    fn key(turn: usize, block: usize, version: u64) -> RowKey {
        RowKey {
            turn,
            kind: RowKind::Block {
                part: 0,
                block,
                reasoning: false,
            },
            version,
        }
    }

    /// Identical sets are no edit at all — the common case every frame a
    /// settled transcript is on screen.
    #[test]
    fn an_unchanged_row_set_is_not_an_edit() {
        let rows = vec![key(0, 0, 1), key(0, 1, 2)];
        assert_eq!(diff_rows(&rows, &rows), None);
    }

    /// The bound that makes streaming flat: a commit that only grows the tail
    /// reports the tail, however long the settled prefix in front of it is.
    #[test]
    fn a_growing_tail_never_touches_the_settled_prefix() {
        let old: Vec<RowKey> = (0..50).map(|b| key(0, b, b as u64)).collect();
        let mut new = old.clone();
        new[49].version = 999;
        assert_eq!(diff_rows(&old, &new), Some((49..50, 1)));
        // …and a wholly new block appends without disturbing anything.
        let mut appended = old.clone();
        appended.push(key(0, 50, 50));
        assert_eq!(diff_rows(&old, &appended), Some((50..50, 1)));
    }

    /// The end-of-turn case: every version moves (the streaming bit) while
    /// every identity stays. Equal counts is the signal to remeasure rather
    /// than splice, so the caller must be able to see it.
    #[test]
    fn the_live_to_settled_flip_is_an_equal_count_edit() {
        let live: Vec<RowKey> = (0..3).map(|b| key(0, b, version(b as u64, true))).collect();
        let settled: Vec<RowKey> = (0..3)
            .map(|b| key(0, b, version(b as u64, false)))
            .collect();
        let (range, count) = diff_rows(&live, &settled).expect("the flip is an edit");
        assert_eq!(range.len(), count, "an equal-count edit must remeasure");
        assert_eq!(range, 0..3);
    }

    /// The streaming bit must not be able to collide with a content hash —
    /// a version that matched by accident is a row that never remeasures.
    #[test]
    fn the_streaming_bit_costs_no_content_bits() {
        for hash in [0u64, 1, 2, 0xdead_beef] {
            assert_ne!(version(hash, true), version(hash, false));
            assert_ne!(version(hash, false), version(hash + 1, false));
        }
    }

    /// The spring lands, and stays landed. Overshoot would show as the last
    /// line bouncing at the bottom edge on every commit.
    #[test]
    fn the_spring_converges_and_never_overshoots() {
        let mut spring = StickSpring::default();
        let target = 500.0;
        let mut pos = 0.0;
        for _ in 0..600 {
            pos = spring.step(pos, target, 1.0);
            assert!(pos <= target, "overshot to {pos}");
        }
        assert_eq!(pos, target, "never arrived");
        assert!(spring.is_idle(), "arrived but still moving");
    }

    /// Approach is monotone: a chase that ever moved *away* from the bottom
    /// would read as the transcript flinching.
    #[test]
    fn the_spring_only_ever_moves_toward_the_bottom() {
        let mut spring = StickSpring::default();
        let mut pos = 0.0;
        for _ in 0..200 {
            let next = spring.step(pos, 400.0, 1.0);
            assert!(next >= pos, "moved backwards: {pos} -> {next}");
            pos = next;
        }
    }

    /// The feed-forward term is what lets it track a target that is still
    /// growing. Without it the view sits permanently behind a streaming reply,
    /// so this is the test that would fail if the EMA were dropped.
    #[test]
    fn the_spring_keeps_up_with_a_growing_target() {
        let mut spring = StickSpring::default();
        let mut target = 0.0;
        let mut pos = 0.0;
        // A steady 6px per frame of new text, for two seconds.
        for _ in 0..120 {
            target += 6.0;
            pos = spring.step(pos, target, 1.0);
        }
        assert!(
            target - pos < theme::SPRING_CHASE_MAX_LEAD + 1.0,
            "fell {}px behind a growing target",
            target - pos
        );
    }

    /// A shrinking target (a fold collapsing) must not leave the growth
    /// estimate running — it would push the view past a bottom that just moved
    /// up.
    #[test]
    fn a_collapsing_row_clears_the_growth_estimate() {
        let mut spring = StickSpring::default();
        let mut target = 0.0;
        let mut pos = 0.0;
        for _ in 0..60 {
            target += 8.0;
            pos = spring.step(pos, target, 1.0);
        }
        // The fold closes: the target jumps back up the document.
        target -= 200.0;
        pos = spring.step(pos.min(target), target, 1.0);
        assert!(pos <= target);
    }

    /// Both halves of the re-stick rule. Band alone would make the pin
    /// unbreakable; direction alone would re-stick from across the document.
    #[test]
    fn resticking_needs_both_the_band_and_the_direction() {
        let band = theme::STICK_THRESHOLD_PX;
        assert!(
            should_restick(band - 10.0, band - 5.0),
            "moving down inside"
        );
        assert!(!should_restick(band - 5.0, band - 10.0), "moving up inside");
        assert!(
            !should_restick(band + 10.0, band + 40.0),
            "outside the band"
        );
    }

    /// Unpinning tolerates the target growing under a pinned view — otherwise
    /// every streamed token would unpin the reader who is watching it.
    #[test]
    fn a_growing_target_does_not_unpin_the_reader_watching_it() {
        assert!(!should_unpin(theme::AT_BOTTOM_PX, 0.0));
        assert!(should_unpin(40.0, 0.0));
        assert!(!should_unpin(0.0, 40.0), "scrolling down never unpins");
    }

    /// A row's gap comes from the pair it sits between. The one that matters
    /// most: two blocks of the same part get exactly the markdown renderer's
    /// own gap, because a moment ago they were one element.
    #[test]
    fn the_gap_is_a_property_of_the_pair() {
        let first = key(0, 0, 1);
        let second = key(0, 1, 2);
        assert_eq!(top_gap_for(None, &first), 0.0);
        assert_eq!(top_gap_for(Some(&first), &second), theme::GAP_TOOL);
        assert_eq!(theme::GAP_TOOL, luma_md::render::MD_BLOCK_GAP);

        let next_turn = key(1, 0, 3);
        assert_eq!(top_gap_for(Some(&second), &next_turn), theme::GAP_TURN);

        let tools = RowKey {
            turn: 0,
            kind: RowKind::Tools { part: 1, count: 2 },
            version: 4,
        };
        assert_eq!(top_gap_for(Some(&first), &tools), theme::GAP_TOOL);
        assert_eq!(top_gap_for(Some(&tools), &second), theme::GAP_TOOL);

        // Two blocks of *different* parts are the ordinary within-turn gap.
        let other_part = RowKey {
            turn: 0,
            kind: RowKind::Block {
                part: 2,
                block: 0,
                reasoning: false,
            },
            version: 5,
        };
        assert_eq!(top_gap_for(Some(&first), &other_part), theme::GAP_BLOCK);
    }
}
