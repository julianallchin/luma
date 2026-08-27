//! The `python` tool's own detail view.
//!
//! [`crate::chip`] shows a call as the JSON it travelled in, which is the
//! honest rendering for a tool this crate has no reading of. Python is the one
//! tool it *does* have a reading of: what is stored is a typed cell result
//! ([`PythonToolOutput`], the very struct the agent persisted), and a reader
//! wants the code highlighted, the streams told apart and the figures as
//! pictures — not a `figures  [{"width":1100,"base64Png":"iVBOR…` row.
//!
//! # A cell is read once, not once a frame
//!
//! A visible row re-renders every frame, and a call's stored output carries its
//! figures as base64 — megabytes of it. Deserializing that, and decoding the
//! PNGs (gpui names an image by hashing its bytes, so even the *identity* is
//! O(size)), is not something a scrolling list can do per frame. [`Cells`] is
//! therefore the entry point: it reads a call once and hands back an `Rc`,
//! re-reading only when the call itself moves.
//!
//! # One reading feeds both the height and the element
//!
//! A chip's open height is *declared, never measured* (see [`crate::chip`]), so
//! a card and its height are a pair that can drift. Here they cannot: both walk
//! the same [`Cell`], section for section. The figure box is a declared height
//! for the same reason — a box sized to its own picture would be a measurement,
//! and one that is not even available until the decode lands.

use std::collections::HashMap;
use std::mem::Discriminant;
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use gpui::{
    div, img, prelude::*, px, AnyElement, Hsla, Image, ImageFormat, ObjectFit, SharedString, Window,
};
use luma_lib::agent::{ToolPart, ToolState};
use luma_lib::models::agent_execution::{PythonStoredFigure, PythonToolOutput};
use luma_md::render::{code_block_height, render_block, RenderOptions};
use luma_md::{Block, Highlighter as _, Syntax};

use crate::chip::section;
use crate::theme::{self, Theme};

/// A figure's box, at its tallest, and at its shortest.
///
/// The box is *declared*, never measured — a card whose height waited on an
/// async decode could not be tweened open. What it is declared from is the
/// figure's own aspect against [`FIGURE_WIDTH`]: a wide plot gets a short box
/// and a stage render gets a tall one, which is the difference between a
/// picture and a picture in a letterbox.
const FIGURE_HEIGHT_MAX: f32 = 240.0;
const FIGURE_HEIGHT_MIN: f32 = 48.0;

/// The width a figure's box is sized *as if* it had: the reading column
/// ([`theme::MAX_CONTENT_WIDTH`]) less the tool rail, the chip's indent under
/// its tile, and the card's own padding. Measuring the real width would make
/// the height a measurement; `ObjectFit::Contain` absorbs whatever the panel's
/// actual width turns out to be.
const FIGURE_WIDTH: f32 = theme::MAX_CONTENT_WIDTH
    - theme::RAIL_INSET
    - theme::RAIL_WIDTH
    - theme::RAIL_GUTTER
    - CARD_INDENT
    - 2.0 * theme::SPACE_MD;

/// How far a chip's card is inset under its narration, past the icon tile.
/// Spelled here because the figure box is sized against it; the element that
/// applies it lives in [`crate::chip`].
pub const CARD_INDENT: f32 = 32.0;

/// How many lines of one stream a card shows.
///
/// Longer than [`theme::CHIP_DETAIL_MAX_LINES`], which bounds a JSON dump
/// nobody reads past the first rows. This is what the cell actually printed —
/// the thing the chip was opened for.
const STREAM_MAX_LINES: usize = 24;

/// Every python call this panel has drawn, read.
///
/// See the module docs: the read is the expensive part and the transcript is
/// append-only, so a call is read when it moves and not otherwise. Entries are
/// only ever made for chips a reader actually opened.
#[derive(Default)]
pub struct Cells(HashMap<SharedString, Rc<Cell>>);

impl Cells {
    /// This call as a cell, or `None` for anything the typed view cannot read —
    /// which is what routes a row back to the generic card.
    pub fn read(&mut self, tool: &ToolPart) -> Option<Rc<Cell>> {
        let key = SharedString::from(tool.call_id.clone());
        let stamp = Stamp::of(tool);
        if let Some(hit) = self.0.get(&key).filter(|hit| hit.stamp == stamp) {
            return Some(Rc::clone(hit));
        }
        let cell = Rc::new(Cell::read(tool, stamp)?);
        self.0.insert(key, Rc::clone(&cell));
        Some(cell)
    }
}

/// What has to change before a call is worth reading again.
///
/// A tool part only ever grows its input and then settles with an output, so
/// the state it is in plus how much code has arrived names its content exactly.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    state: Discriminant<ToolState>,
    code_len: usize,
}

impl Stamp {
    fn of(tool: &ToolPart) -> Self {
        Self {
            state: std::mem::discriminant(&tool.state),
            code_len: tool
                .input
                .as_ref()
                .and_then(|input| input.get("code")?.as_str())
                .map_or(0, str::len),
        }
    }
}

/// A python call, read.
///
/// `code` is the model's argument and is there from the moment the call
/// streams; everything else arrives with the result, so a running cell is its
/// code alone.
pub struct Cell {
    stamp: Stamp,
    key: SharedString,
    code: SharedString,
    streams: Vec<Stream>,
    figures: Vec<Figure>,
    status: Status,
    duration_ms: Option<u64>,
}

/// How a call ended, as the chip's dot reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Ok,
    /// The cell raised. The kernel is fine; the traceback is in the card.
    Raised,
    /// The cell never finished — interrupted, or the worker failed under it.
    Stopped,
}

impl Status {
    /// The dot's colour. Hue is meaning here, which is the one thing it is for.
    #[must_use]
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Status::Running => theme.warning,
            Status::Ok => theme.success,
            Status::Raised | Status::Stopped => theme.danger,
        }
    }
}

impl Cell {
    fn read(tool: &ToolPart, stamp: Stamp) -> Option<Self> {
        if tool.tool_name() != "python" {
            return None;
        }
        let code = tool.input.as_ref()?.get("code")?.as_str()?;
        // A stored shape this build cannot read is *not* half-rendered: `None`
        // sends the row back to the JSON it was actually stored as, which is
        // the only honest thing to show for it.
        let output: Option<PythonToolOutput> = match tool.output.as_ref() {
            Some(stored) => Some(serde_json::from_value(stored.clone()).ok()?),
            None => None,
        };

        let mut streams = Vec::new();
        if let Some(error) = &tool.error_text {
            streams.push(Stream::grave("Error", error));
        }
        if let Some(output) = &output {
            if !output.stdout.trim().is_empty() {
                streams.push(Stream::plain("Stdout", &output.stdout));
            }
            if !output.stderr.trim().is_empty() {
                streams.push(Stream::grave("Stderr", &output.stderr));
            }
            if let Some(traceback) = &output.traceback {
                streams.push(Stream::tail("Traceback", traceback));
            }
            if let Some(repr) = &output.repr {
                streams.push(Stream::plain("Value", repr));
            }
            for notice in &output.notices {
                streams.push(Stream::note("Note", notice));
            }
        }

        let status = if tool.error_text.is_some() {
            Status::Stopped
        } else {
            match output.as_ref().map(|output| output.status.as_str()) {
                None => Status::Running,
                Some("ok") => Status::Ok,
                Some("error") => Status::Raised,
                Some(_) => Status::Stopped,
            }
        };
        let figures = output
            .as_ref()
            .map(|output| output.figures.iter().map(Figure::decode).collect())
            .unwrap_or_default();

        Some(Self {
            stamp,
            key: SharedString::from(format!("chip-{}", tool.call_id)),
            code: SharedString::from(code.trim_end().to_string()),
            streams,
            figures,
            status,
            duration_ms: output.map(|output| output.duration_ms),
        })
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.status
    }

    /// How long the cell ran, once it has.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    /// The card's exact open height — the element below, term for term.
    #[must_use]
    pub fn height(&self) -> f32 {
        let sections = 1 + self.streams.len() + self.figures.len();
        let streams: f32 = self
            .streams
            .iter()
            .map(|stream| (1 + stream.lines.len()) as f32 * theme::CHIP_DETAIL_LINE)
            .sum();
        code_block_height(&self.code)
            + streams
            + self.figures.iter().map(Figure::height).sum::<f32>()
            // Per card: the top rule and its padding, the bottom padding, and
            // one gap between each pair of sections.
            + 1.0
            + theme::SPACE_SM
            + theme::SPACE_MD
            + (sections - 1) as f32 * theme::SPACE_SM
    }

    /// The card: the code, then what the cell said, then what it drew.
    pub fn card(&self, theme: &Theme, window: &Window) -> AnyElement {
        let syntax = Syntax::new(theme);
        let highlight = syntax.highlight(Some("python"), &self.code);
        let opts = RenderOptions::settled(self.key.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_SM))
            .px(px(theme::SPACE_MD))
            .pb(px(theme::SPACE_MD))
            .border_t_1()
            .border_color(theme.border)
            .pt(px(theme::SPACE_SM))
            .child(render_block(
                &Block::CodeBlock {
                    language: Some("python".into()),
                    code: self.code.to_string(),
                },
                0,
                0,
                &opts,
                theme,
                window,
                highlight.as_deref(),
            ))
            .children(self.streams.iter().map(|stream| {
                section(
                    stream.heading,
                    stream.lines.clone(),
                    stream.tone.color(theme),
                    Some(SharedString::from(format!(
                        "{}-{}",
                        self.key, stream.heading
                    ))),
                    theme,
                )
            }))
            .children(self.figures.iter().map(|figure| figure.element(theme)))
            .into_any_element()
    }
}

/// One labelled block of cell text.
struct Stream {
    heading: &'static str,
    lines: Vec<SharedString>,
    tone: Tone,
}

/// What a block of cell text *is*, which is the only thing that decides its
/// colour.
#[derive(Clone, Copy)]
enum Tone {
    /// What the cell said.
    Plain,
    /// What went wrong.
    Grave,
    /// What the kernel wants the reader to know — a restart, a dropped figure.
    Note,
}

impl Tone {
    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Tone::Plain => theme.text_muted,
            Tone::Grave => theme.danger,
            Tone::Note => theme.warning,
        }
    }
}

impl Stream {
    fn plain(heading: &'static str, text: &str) -> Self {
        Self {
            heading,
            lines: clipped(split(text)),
            tone: Tone::Plain,
        }
    }

    fn grave(heading: &'static str, text: &str) -> Self {
        Self {
            tone: Tone::Grave,
            ..Self::plain(heading, text)
        }
    }

    fn note(heading: &'static str, text: &str) -> Self {
        Self {
            tone: Tone::Note,
            ..Self::plain(heading, text)
        }
    }

    /// Clipped from the *head*, keeping the tail: a traceback's raising frame
    /// and its error line live at the bottom, so head-first clipping drops
    /// exactly the two lines anybody opened it for.
    fn tail(heading: &'static str, text: &str) -> Self {
        let lines = split(text);
        let dropped = lines.len().saturating_sub(STREAM_MAX_LINES);
        let mut kept: Vec<String> = lines.into_iter().skip(dropped).collect();
        if dropped > 0 {
            kept.insert(0, "…".into());
        }
        Self {
            heading,
            lines: kept.into_iter().map(SharedString::from).collect(),
            tone: Tone::Grave,
        }
    }
}

fn split(text: &str) -> Vec<String> {
    text.trim_end()
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect()
}

/// At most [`STREAM_MAX_LINES`], with an ellipsis standing in for the rest —
/// the bound that keeps [`Cell::height`] a count.
fn clipped(lines: Vec<String>) -> Vec<SharedString> {
    let over = lines.len() > STREAM_MAX_LINES;
    let mut out: Vec<SharedString> = lines
        .into_iter()
        .take(STREAM_MAX_LINES)
        .map(SharedString::from)
        .collect();
    if over {
        out.push(SharedString::from("…"));
    }
    out
}

/// One matplotlib figure, decoded.
struct Figure {
    width: u32,
    height: u32,
    /// `None` when the transcript did not keep the bytes — a single figure over
    /// the persistence budget — or when they will not decode.
    image: Option<Arc<Image>>,
}

impl Figure {
    /// This figure's declared box height — see [`FIGURE_HEIGHT_MAX`].
    fn height(&self) -> f32 {
        if self.width == 0 {
            return FIGURE_HEIGHT_MIN;
        }
        (FIGURE_WIDTH * self.height as f32 / self.width as f32)
            .clamp(FIGURE_HEIGHT_MIN, FIGURE_HEIGHT_MAX)
    }

    fn decode(stored: &PythonStoredFigure) -> Self {
        let image = stored
            .base64_png
            .as_ref()
            .and_then(|data| {
                base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .ok()
            })
            .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes)));
        Self {
            width: stored.width,
            height: stored.height,
            image,
        }
    }

    fn element(&self, theme: &Theme) -> AnyElement {
        let frame = div()
            .h(px(self.height()))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded(px(luma_ui::radius::ROW));
        let Some(image) = self.image.clone() else {
            // A figure the transcript could not keep still holds its slot and
            // says so, rather than leaving a gap the reader has to guess at.
            return frame
                .bg(theme::wash(0.04))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(format!(
                    "figure {}×{} was too large to keep",
                    self.width, self.height
                )))
                .into_any_element();
        };
        frame
            .child(
                img(image)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    // The decode is asynchronous. Both the placeholder and the
                    // picture fill the same declared box, so the card does not
                    // resize under a reader one frame after they opened it.
                    .with_loading(|| div().size_full().into_any_element()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn part(name: &str, input: serde_json::Value, output: Option<serde_json::Value>) -> ToolPart {
        ToolPart {
            name: Some(name.to_string()),
            dynamic: false,
            call_id: "call-1".into(),
            state: if output.is_some() {
                ToolState::OutputAvailable
            } else {
                ToolState::InputAvailable
            },
            input: Some(input),
            output,
            error_text: None,
        }
    }

    /// A stored output with everything at rest, so a test names only the field
    /// it is about.
    fn stored(extra: serde_json::Value) -> serde_json::Value {
        let mut base = json!({
            "status": "ok",
            "stdout": "",
            "stderr": "",
            "repr": null,
            "traceback": null,
            "notices": [],
            "figures": [],
            "durationMs": 12,
        });
        let (serde_json::Value::Object(fields), serde_json::Value::Object(extra)) =
            (&mut base, extra)
        else {
            panic!("both are objects");
        };
        fields.extend(extra);
        base
    }

    fn cell(output: serde_json::Value) -> Rc<Cell> {
        Cells::default()
            .read(&part("python", json!({ "code": "1" }), Some(output)))
            .expect("a readable python call")
    }

    /// Only python gets the typed view; everything else falls back to the
    /// generic card, which is the whole reason that card stays.
    #[test]
    fn only_a_python_call_reads_as_a_cell() {
        let mut cells = Cells::default();
        assert!(cells
            .read(&part("skill", json!({ "name": "beatgrid" }), None))
            .is_none());
        assert!(cells
            .read(&part("python", json!({ "code": "1" }), None))
            .is_some());
    }

    /// An output shape this build does not know is not half-rendered.
    #[test]
    fn an_unreadable_output_is_not_a_cell() {
        let tool = part("python", json!({ "code": "1" }), Some(json!({ "who": 1 })));
        assert!(Cells::default().read(&tool).is_none());
    }

    /// A call still streaming renders its code alone.
    #[test]
    fn a_running_cell_is_its_code() {
        let cell = Cells::default()
            .read(&part("python", json!({ "code": "a\nb" }), None))
            .unwrap();
        assert_eq!(cell.status(), Status::Running);
        assert!(cell.streams.is_empty());
        assert_eq!(cell.duration_ms(), None);
    }

    /// The four ways a call ends, and the one that is not the cell's fault.
    #[test]
    fn the_status_reads_every_ending() {
        assert_eq!(cell(stored(json!({}))).status(), Status::Ok);
        assert_eq!(
            cell(stored(json!({ "status": "error" }))).status(),
            Status::Raised
        );
        assert_eq!(
            cell(stored(json!({ "status": "interrupted" }))).status(),
            Status::Stopped
        );
        assert_eq!(
            cell(stored(json!({ "status": "failed" }))).status(),
            Status::Stopped
        );

        let mut tool = part("python", json!({ "code": "1" }), None);
        tool.state = ToolState::OutputError;
        tool.error_text = Some("workspace unavailable".into());
        let cell = Cells::default().read(&tool).unwrap();
        assert_eq!(cell.status(), Status::Stopped);
        assert_eq!(cell.streams[0].heading, "Error");
    }

    /// Empty sections are omitted — a card of `stderr` / `repr null` /
    /// `traceback null` rows is the complaint this view exists to answer.
    #[test]
    fn empty_sections_are_omitted() {
        let cell = cell(stored(json!({ "stdout": "hi\n" })));
        let headings: Vec<_> = cell.streams.iter().map(|s| s.heading).collect();
        assert_eq!(headings, vec!["Stdout"]);
        assert_eq!(cell.streams[0].lines, vec![SharedString::from("hi")]);
    }

    /// A traceback keeps its tail: the raising frame and the error line.
    #[test]
    fn a_long_traceback_keeps_its_last_line() {
        let body = (0..STREAM_MAX_LINES * 2)
            .map(|ix| format!("frame {ix}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cell = cell(stored(
            json!({ "traceback": format!("{body}\nValueError: no") }),
        ));
        let lines = &cell.streams[0].lines;
        assert_eq!(lines.len(), STREAM_MAX_LINES + 1);
        assert_eq!(lines[0].as_ref(), "…");
        assert_eq!(lines.last().unwrap().as_ref(), "ValueError: no");
    }

    /// Every section the card draws also adds height, and a cell that printed a
    /// novel still gets a card the size of a card.
    #[test]
    fn the_height_grows_with_the_card_and_stays_bounded() {
        let bare = cell(stored(json!({})));
        assert!(cell(stored(json!({ "stdout": "one\ntwo\n" }))).height() > bare.height());

        let huge = cell(stored(json!({ "stdout": "x\n".repeat(10_000) })));
        let ceiling = bare.height()
            + (STREAM_MAX_LINES + 2) as f32 * theme::CHIP_DETAIL_LINE
            + theme::SPACE_SM;
        assert!(huge.height() <= ceiling, "{} > {ceiling}", huge.height());

        // A figure's box comes from its aspect, and is capped either way: a
        // wide plot gets a short box, a stage render the tall one.
        let boxed = |width: u32, height: u32| {
            cell(stored(json!({
                "figures": [{ "width": width, "height": height }],
            })))
            .height()
                - bare.height()
                - theme::SPACE_SM
        };
        assert!((boxed(1200, 280) - FIGURE_WIDTH * 280.0 / 1200.0).abs() < 0.01);
        assert_eq!(boxed(960, 540), FIGURE_HEIGHT_MAX);
        assert_eq!(boxed(4000, 10), FIGURE_HEIGHT_MIN);
    }

    /// A figure decodes once per call, not once a frame — the property the
    /// cache exists for, and the one a scrolling list depends on.
    #[test]
    fn a_settled_call_is_read_once() {
        // 1×1 transparent PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk\
                   YPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let tool = part(
            "python",
            json!({ "code": "1" }),
            Some(stored(json!({
                "figures": [{ "width": 1, "height": 1, "base64Png": png }],
            }))),
        );
        let mut cells = Cells::default();
        let first = cells.read(&tool).unwrap();
        let second = cells.read(&tool).unwrap();
        assert!(first.figures[0].image.is_some());
        assert!(
            Rc::ptr_eq(&first, &second),
            "a second render must reuse the reading, not redo it"
        );

        // …and the same call with more code arrived *is* re-read: the stamp is
        // what makes a streaming chip keep up.
        let mut grown = tool.clone();
        grown.input = Some(json!({ "code": "1 + 1" }));
        assert!(!Rc::ptr_eq(&first, &cells.read(&grown).unwrap()));
    }

    /// A figure the transcript could not keep still occupies its slot, so a
    /// card's height does not depend on what was persisted.
    #[test]
    fn a_dropped_figure_keeps_its_box() {
        let cell = cell(stored(json!({
            "figures": [{ "width": 9, "height": 9 }],
        })));
        assert!(cell.figures[0].image.is_none());
    }
}
