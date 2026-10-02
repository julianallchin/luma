//! Editor context is captured in the durable user turn, never in tool arguments.
//!
//! Each user message carries the editor as it stood when the message was
//! sent, as a hidden part. The model reads that part as a short
//! `<editor-context>` text block after the message; the chat does not show it.
//! The system prompt stays the same for the whole thread, so its cache holds.

use std::fmt::Write as _;

use crate::models::agent_threads::AgentThread;
use crate::models::node_graph::BeatGrid;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::transcript::{AgentChatMessage, AgentChatPart};
use super::{TurnContext, UserPrompt};

pub const PART_TYPE: &str = "data-luma-context";

/// At most this many clips go into one message's context.
pub const MAX_CLIPS: usize = 40;

/// The track editor when a message was sent. Times are seconds from the
/// start of the track.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditorState {
    pub playhead: TimePoint,
    pub cursor: Option<EditorCursor>,
    /// The clips the cursor touches in its lanes, and every selected clip.
    /// Sorted by start, at most [`MAX_CLIPS`].
    pub clips: Vec<EditorClip>,
    /// How many more clips matched than [`Self::clips`] holds.
    #[serde(default)]
    pub clips_omitted: usize,
}

/// A time on the track, and where it falls in the beat grid when the track
/// has one.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimePoint {
    pub seconds: f64,
    pub bar: Option<BarBeat>,
}

/// A position in bars. Bar 1 beat 1.00 is the first downbeat; a pickup before
/// it is bar 0 or lower. The model reads it as `bar.beat.16th`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BarBeat {
    pub bar: i64,
    /// From 1 up to, not including, `beats_per_bar + 1`.
    pub beat: f64,
}

/// The editor's cursor: a point when `end` is `None`, else a time range.
/// Lanes count down from lane 0, the empty lane for new clips.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditorCursor {
    pub start: TimePoint,
    pub end: Option<TimePoint>,
    pub first_lane: usize,
    pub last_lane: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditorClip {
    pub id: String,
    pub name: String,
    pub lane: usize,
    /// The clip's layer. Rows written before z was captured have none.
    #[serde(default)]
    pub z: Option<i64>,
    pub start: TimePoint,
    pub end: TimePoint,
    pub selected: bool,
}

/// The cursor as the editor holds it, in seconds, lowest first.
#[derive(Clone, Copy, Debug)]
pub struct CursorSpan {
    pub start: f64,
    pub end: Option<f64>,
    pub first_lane: usize,
    pub last_lane: usize,
}

/// One clip as the editor holds it, in seconds.
#[derive(Clone, Debug)]
pub struct ClipSpan {
    pub id: String,
    pub name: String,
    pub lane: usize,
    pub z: i64,
    pub start: f64,
    pub end: f64,
    pub selected: bool,
}

impl EditorState {
    /// Put the editor's raw state into the shape a message carries: keep the
    /// clips the cursor touches and the selected ones, and give every time
    /// its bar and beat when `grid` is set.
    #[must_use]
    pub fn capture(
        playhead: f64,
        cursor: Option<CursorSpan>,
        clips: impl IntoIterator<Item = ClipSpan>,
        grid: Option<&BeatGrid>,
    ) -> Self {
        let bars = grid.and_then(Bars::new);
        let at = |seconds: f64| TimePoint {
            seconds,
            bar: bars.as_ref().and_then(|bars| bars.at(seconds)),
        };
        let touched = |clip: &ClipSpan| {
            cursor.is_some_and(|cursor| {
                let in_lanes = (cursor.first_lane..=cursor.last_lane).contains(&clip.lane);
                let in_time = match cursor.end {
                    Some(end) => clip.start < end && clip.end > cursor.start,
                    None => clip.start <= cursor.start && cursor.start < clip.end,
                };
                in_lanes && in_time
            })
        };
        let mut kept: Vec<ClipSpan> = clips
            .into_iter()
            .filter(|clip| clip.selected || touched(clip))
            .collect();
        kept.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.lane.cmp(&b.lane)));
        let clips_omitted = kept.len().saturating_sub(MAX_CLIPS);
        kept.truncate(MAX_CLIPS);
        Self {
            playhead: at(playhead),
            cursor: cursor.map(|cursor| EditorCursor {
                start: at(cursor.start),
                end: cursor.end.map(at),
                first_lane: cursor.first_lane,
                last_lane: cursor.last_lane,
            }),
            clips: kept
                .into_iter()
                .map(|clip| EditorClip {
                    start: at(clip.start),
                    end: at(clip.end),
                    id: clip.id,
                    name: clip.name,
                    lane: clip.lane,
                    z: Some(clip.z),
                    selected: clip.selected,
                })
                .collect(),
            clips_omitted,
        }
    }
}

/// A beat grid read as bars, as the editor ruler draws them: bar n starts at
/// the n-th detected downbeat, and beats inside a bar are the detected beats.
/// Before the first downbeat and after the last, bars are `beats_per_bar`
/// beats long. `luma_exec/track.py` `BarGrid` is the same rule.
struct Bars {
    timeline: luma_patterns::BeatTimeline,
    /// Where each bar starts, in beats from the first downbeat.
    starts: Vec<f64>,
    beats_per_bar: f64,
}

impl Bars {
    fn new(grid: &BeatGrid) -> Option<Self> {
        if grid.beats_per_bar <= 0 {
            return None;
        }
        let timeline = grid.timeline().ok()?;
        let mut starts = grid
            .downbeats
            .iter()
            .map(|time| timeline.beat_at(f64::from(*time)).ok())
            .collect::<Option<Vec<f64>>>()?;
        if starts.is_empty() {
            starts.push(0.0);
        }
        Some(Self {
            timeline,
            starts,
            beats_per_bar: f64::from(grid.beats_per_bar),
        })
    }

    /// The beat where a bar (from 1) starts.
    #[allow(clippy::cast_precision_loss)]
    fn start(&self, bar: i64) -> f64 {
        let (first, last) = (self.starts[0], self.starts[self.starts.len() - 1]);
        let index = bar - 1;
        let count = self.starts.len() as i64;
        match usize::try_from(index) {
            Err(_) => first + index as f64 * self.beats_per_bar,
            Ok(i) if i < self.starts.len() => self.starts[i],
            Ok(_) => last + (index - count + 1) as f64 * self.beats_per_bar,
        }
    }

    /// The bar (from 1) that holds a beat.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    fn bar_at(&self, beat: f64) -> i64 {
        let (first, last) = (self.starts[0], self.starts[self.starts.len() - 1]);
        if beat < first {
            1 - ((first - beat) / self.beats_per_bar - 1e-9).ceil() as i64
        } else if beat >= last {
            self.starts.len() as i64 + ((beat - last) / self.beats_per_bar + 1e-9).floor() as i64
        } else {
            self.starts.partition_point(|start| *start <= beat) as i64
        }
    }

    fn at(&self, seconds: f64) -> Option<BarBeat> {
        let beat = self.timeline.beat_at(seconds).ok()?;
        let mut bar = self.bar_at(beat);
        // Rounded to what is shown (a hundredth of a 16th) first, so the end
        // of a bar never prints as 16th 5 of its last beat.
        let mut within = ((beat - self.start(bar)) * 400.0).round() / 400.0;
        if within >= ((self.start(bar + 1) - self.start(bar)) * 400.0).round() / 400.0 {
            bar += 1;
            within = 0.0;
        }
        Some(BarBeat {
            bar,
            beat: within + 1.0,
        })
    }
}

/// The context a user row carries, if it carries one that parses.
#[must_use]
pub fn of_message(message: &AgentChatMessage) -> Option<TurnContext> {
    message.parts.iter().find_map(|part| match part {
        AgentChatPart::Unknown(value) if value["type"] == PART_TYPE => {
            serde_json::from_value(value["data"].clone()).ok()
        }
        _ => None,
    })
}

/// The hidden part that stores `context` on a user row.
#[must_use]
pub fn part(context: &TurnContext) -> AgentChatPart {
    AgentChatPart::Unknown(serde_json::json!({ "type": PART_TYPE, "data": context }))
}

/// A user row as it is stored: the text, then the context part.
#[must_use]
pub fn user_message(id: impl Into<String>, prompt: &UserPrompt) -> AgentChatMessage {
    let mut message = AgentChatMessage::user(id, prompt.text.clone());
    message.parts.extend(prompt.context.as_ref().map(part));
    message
}

/// What the model reads for a user message: its text, then the context block.
#[must_use]
pub fn prompt_blocks(text: String, context: Option<&TurnContext>) -> Vec<String> {
    std::iter::once(text)
        .filter(|text| !text.is_empty())
        .chain(context.map(render))
        .collect()
}

/// [`prompt_blocks`] for a stored user row.
#[must_use]
pub fn message_blocks(message: &AgentChatMessage) -> Vec<String> {
    prompt_blocks(message.text(), of_message(message).as_ref())
}

/// The `<editor-context>` block the model reads after a user message.
#[must_use]
pub fn render(context: &TurnContext) -> String {
    let mut out = String::from("<editor-context>\n");
    let ids: Vec<String> = context
        .scope
        .iter()
        .flat_map(|scope| {
            let track = (scope.subject_kind == super::SubjectKind::Track)
                .then(|| ("Track", scope.subject_id.clone()));
            [
                track,
                scope.venue_id.clone().map(|id| ("Venue", id)),
                scope.score_id.clone().map(|id| ("Score", id)),
            ]
        })
        .flatten()
        .map(|(name, id)| format!("{name}: {id}"))
        .collect();
    if ids.is_empty() {
        out.push_str("No track or venue is open.\n");
    } else {
        let _ = writeln!(out, "{}", ids.join("  "));
    }
    if let Some(editor) = &context.editor {
        render_editor(&mut out, editor);
    }
    out.push_str("</editor-context>");
    out
}

fn render_editor(out: &mut String, editor: &EditorState) {
    let _ = writeln!(out, "Playhead: {}", point(editor.playhead));
    let heading = match &editor.cursor {
        None => {
            out.push_str("Cursor: none\n");
            "Selected clips"
        }
        Some(cursor) => {
            let lanes = lanes(cursor.first_lane, cursor.last_lane);
            match cursor.end {
                None => {
                    let _ = writeln!(out, "Cursor: {}, {lanes}", point(cursor.start));
                    "Clips at the cursor"
                }
                Some(end) => {
                    let _ = writeln!(out, "Selection: {}, {lanes}", range(cursor.start, end));
                    "Clips in the selection"
                }
            }
        }
    };
    let total = editor.clips.len() + editor.clips_omitted;
    if total == 0 {
        return;
    }
    let _ = writeln!(out, "{heading} ({total}):");
    for clip in &editor.clips {
        let z = clip.z.map(|z| format!(" z {z}")).unwrap_or_default();
        let _ = writeln!(
            out,
            "- {} {:?} lane {}{z}, {}{}",
            clip.id,
            clip.name,
            clip.lane,
            range(clip.start, clip.end),
            if clip.selected { ", selected" } else { "" }
        );
    }
    if editor.clips_omitted > 0 {
        let _ = writeln!(out, "…and {} more", editor.clips_omitted);
    }
}

fn seconds(at: TimePoint) -> String {
    format!("{:.2} s", at.seconds)
}

/// `bar.beat.16th`, all three from 1, as the Python tools print it: `17.2.3`,
/// or `17.2.3.5` between two 16ths.
fn bar(at: BarBeat) -> String {
    let sixteenths = ((at.beat - 1.0) * 400.0).round() / 100.0;
    let beat = (sixteenths / 4.0).floor();
    let sixteenth = format!("{:.2}", sixteenths - beat * 4.0 + 1.0);
    let sixteenth = sixteenth.trim_end_matches('0').trim_end_matches('.');
    format!("{}.{}.{sixteenth}", at.bar, beat + 1.0)
}

/// `63.42 s (17.2.3)`.
fn point(at: TimePoint) -> String {
    match at.bar {
        Some(position) => format!("{} ({})", seconds(at), bar(position)),
        None => seconds(at),
    }
}

/// `60.00 s to 75.50 s (16.1.1-20.1.1)`. A range includes its start and
/// excludes its end.
fn range(start: TimePoint, end: TimePoint) -> String {
    let times = format!("{} to {}", seconds(start), seconds(end));
    match (start.bar, end.bar) {
        (Some(from), Some(to)) => format!("{times} ({}-{})", bar(from), bar(to)),
        _ => times,
    }
}

fn lanes(first: usize, last: usize) -> String {
    if first == last {
        format!("lane {first}")
    } else {
        format!("lanes {first} to {last}")
    }
}

pub async fn execution_thread(
    pool: &SqlitePool,
    thread_id: &str,
    principal: Option<&str>,
) -> Result<AgentThread, String> {
    let mut thread =
        crate::database::local::agent_threads::get_thread_row(pool, thread_id, principal).await?;
    let context: Option<String> = sqlx::query_scalar(
        "WITH RECURSIVE lineage AS (
            SELECT message.id, message.parent_message_id, message.depth, message.role, message.parts_json
            FROM agent_thread_messages message
            JOIN agent_thread_transcript_heads head ON head.head_message_id = message.id
            WHERE head.thread_id = ? AND head.uid IS ?
            UNION ALL
            SELECT parent.id, parent.parent_message_id, parent.depth, parent.role, parent.parts_json
            FROM agent_thread_messages parent JOIN lineage child ON parent.id = child.parent_message_id
         )
         SELECT json_extract(part.value, '$.data')
         FROM lineage, json_each(lineage.parts_json) part
         WHERE lineage.role = 'user' AND json_extract(part.value, '$.type') = ?
         ORDER BY lineage.depth DESC LIMIT 1"
    ).bind(thread_id).bind(principal).bind(PART_TYPE).fetch_optional(pool).await
        .map_err(|error| format!("read turn context: {error}"))?;
    if let Some(context) = context {
        let context: TurnContext = serde_json::from_str(&context)
            .map_err(|error| format!("invalid turn context: {error}"))?;
        apply(&mut thread, &context);
        thread.route()?;
    }
    Ok(thread)
}

fn apply(thread: &mut AgentThread, context: &TurnContext) {
    let scope = context.scope.as_ref();
    thread.agent_kind = scope
        .map_or("unbound", |scope| scope.agent_kind.as_str())
        .into();
    thread.subject_kind = scope.map(|scope| scope.subject_kind.as_str().into());
    thread.subject_id = scope.map(|scope| scope.subject_id.clone());
    thread.venue_id = scope.and_then(|scope| scope.venue_id.clone());
    thread.score_id = scope.and_then(|scope| scope.score_id.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ThreadScope;

    /// Beats every half second from 0 s, four to a bar, with the first
    /// downbeat at 1 s: 0 s and 0.5 s are a two-beat pickup.
    fn grid() -> BeatGrid {
        BeatGrid {
            beats: (0..200).map(|beat| beat as f32 * 0.5).collect(),
            downbeats: vec![1.0],
            bpm: 120.0,
            downbeat_offset: 1.0,
            beats_per_bar: 4,
        }
    }

    fn clip(id: &str, lane: usize, start: f64, end: f64, selected: bool) -> ClipSpan {
        ClipSpan {
            id: id.into(),
            name: format!("Clip {id}"),
            lane,
            z: 10 - lane as i64,
            start,
            end,
            selected,
        }
    }

    fn track_context(editor: EditorState) -> TurnContext {
        TurnContext {
            scope: Some(ThreadScope::track("track-1", "venue-1", "score-1")),
            editor: Some(editor),
        }
    }

    #[test]
    fn without_a_grid_times_are_seconds_only() {
        let editor = EditorState::capture(63.4249, None, [], None);
        let text = render(&track_context(editor));
        assert_eq!(
            text,
            "<editor-context>\n\
             Track: track-1  Venue: venue-1  Score: score-1\n\
             Playhead: 63.42 s\n\
             Cursor: none\n\
             </editor-context>"
        );
    }

    #[test]
    fn a_grid_gives_bar_and_beat() {
        let grid = grid();
        // 1 s is the first downbeat; 3.25 s is 4.5 beats after it.
        let editor = EditorState::capture(3.25, None, [], Some(&grid));
        assert_eq!(editor.playhead.bar, Some(BarBeat { bar: 2, beat: 1.5 }));
        assert!(render(&track_context(editor)).contains("Playhead: 3.25 s (2.1.3)"));
        let downbeat = EditorState::capture(1.0, None, [], Some(&grid));
        assert_eq!(downbeat.playhead.bar, Some(BarBeat { bar: 1, beat: 1.0 }));
    }

    #[test]
    fn a_pickup_is_bar_zero_and_never_panics() {
        let grid = grid();
        // Two beats before the first downbeat: the last two beats of bar 0.
        let editor = EditorState::capture(0.0, None, [], Some(&grid));
        assert_eq!(editor.playhead.bar, Some(BarBeat { bar: 0, beat: 3.0 }));
        // Far before the grid, extrapolated.
        let early = EditorState::capture(-10.0, None, [], Some(&grid));
        assert!(early
            .playhead
            .bar
            .is_some_and(|at| at.bar < 0 && (1.0..5.0).contains(&at.beat)));
    }

    #[test]
    fn a_beat_that_rounds_up_stays_in_its_bar() {
        let grid = grid();
        // 2.9999 s is 3.9998 beats in: shown as the next downbeat, not 16th 5.
        let editor = EditorState::capture(2.9999, None, [], Some(&grid));
        assert_eq!(editor.playhead.bar, Some(BarBeat { bar: 2, beat: 1.0 }));
    }

    #[test]
    fn bars_start_at_the_detected_downbeats_as_on_the_ruler() {
        // Bar 1 is two beats long: the downbeats say so, not the meter.
        let mut grid = grid();
        grid.downbeats = vec![1.0, 2.0, 4.0];
        let at = |seconds| {
            EditorState::capture(seconds, None, [], Some(&grid))
                .playhead
                .bar
        };
        assert_eq!(at(1.5), Some(BarBeat { bar: 1, beat: 2.0 }));
        assert_eq!(at(2.0), Some(BarBeat { bar: 2, beat: 1.0 }));
        assert_eq!(at(3.5), Some(BarBeat { bar: 2, beat: 4.0 }));
        assert_eq!(at(6.0), Some(BarBeat { bar: 4, beat: 1.0 }));
        assert_eq!(at(0.5), Some(BarBeat { bar: 0, beat: 4.0 }));
    }

    #[test]
    fn positions_read_bar_beat_sixteenth_from_one() {
        let at = |bar, beat| super::bar(BarBeat { bar, beat });
        assert_eq!(at(17, 1.0), "17.1.1");
        assert_eq!(at(17, 2.5), "17.2.3");
        assert_eq!(at(17, 4.875), "17.4.4.5");
        assert_eq!(at(0, 3.0), "0.3.1");
    }

    #[test]
    fn a_point_cursor_lists_the_clips_under_it_in_its_lane() {
        let cursor = CursorSpan {
            start: 5.0,
            end: None,
            first_lane: 2,
            last_lane: 2,
        };
        let clips = [
            clip("under", 2, 4.0, 6.0, false),
            clip("other-lane", 1, 4.0, 6.0, false),
            clip("ends-here", 2, 3.0, 5.0, false),
            clip("picked", 3, 20.0, 30.0, true),
        ];
        let editor = EditorState::capture(0.0, Some(cursor), clips, None);
        let ids: Vec<_> = editor.clips.iter().map(|clip| clip.id.as_str()).collect();
        assert_eq!(ids, ["under", "picked"]);
        let text = render(&track_context(editor));
        assert!(text.contains("Cursor: 5.00 s, lane 2\n"), "{text}");
        assert!(text.contains("Clips at the cursor (2):\n"), "{text}");
        assert!(
            text.contains("- under \"Clip under\" lane 2 z 8, 4.00 s to 6.00 s\n"),
            "{text}"
        );
        assert!(
            text.contains("- picked \"Clip picked\" lane 3 z 7, 20.00 s to 30.00 s, selected\n")
        );
    }

    #[test]
    fn a_range_lists_overlapping_clips_in_its_lanes() {
        let grid = grid();
        let cursor = CursorSpan {
            start: 3.0,
            end: Some(5.0),
            first_lane: 1,
            last_lane: 3,
        };
        let clips = [
            clip("late", 3, 4.5, 9.0, false),
            clip("early", 1, 1.0, 3.5, false),
            clip("touching", 2, 5.0, 6.0, false),
            clip("below", 4, 3.0, 5.0, false),
        ];
        let editor = EditorState::capture(4.0, Some(cursor), clips, Some(&grid));
        let ids: Vec<_> = editor.clips.iter().map(|clip| clip.id.as_str()).collect();
        assert_eq!(ids, ["early", "late"], "sorted by start");
        let text = render(&track_context(editor));
        assert!(
            text.contains("Selection: 3.00 s to 5.00 s (2.1.1-3.1.1), lanes 1 to 3\n"),
            "{text}"
        );
        assert!(text.contains("Clips in the selection (2):\n"), "{text}");
    }

    #[test]
    fn the_list_is_capped_and_says_how_many_are_left_out() {
        let clips = (0..55).map(|at| clip(&format!("c{at}"), 1, f64::from(at), 100.0, true));
        let editor = EditorState::capture(0.0, None, clips, None);
        assert_eq!(editor.clips.len(), MAX_CLIPS);
        assert_eq!(editor.clips_omitted, 55 - MAX_CLIPS);
        let text = render(&track_context(editor));
        assert!(text.contains("Selected clips (55):\n"), "{text}");
        assert!(text.contains("\n…and 15 more\n</editor-context>"), "{text}");
    }

    /// Rows written before messages carried the editor still parse, and say
    /// which ids were open.
    #[test]
    fn an_old_context_row_still_parses() {
        let mut old = serde_json::to_value(TurnContext {
            scope: Some(ThreadScope::track("track-1", "venue-1", "score-1")),
            editor: None,
        })
        .expect("context");
        old.as_object_mut().expect("object").remove("editor");
        let context: TurnContext = serde_json::from_value(old).expect("old row");
        assert!(context.editor.is_none());
        let message = AgentChatMessage {
            id: "u".into(),
            role: super::super::Role::User,
            parts: vec![AgentChatPart::Text { text: "hi".into() }, part(&context)],
        };
        let blocks = message_blocks(&message);
        assert_eq!(blocks[0], "hi");
        assert!(blocks[1].contains("Track: track-1"), "{blocks:?}");
        let empty = TurnContext {
            scope: None,
            editor: None,
        };
        assert!(render(&empty).contains("No track or venue is open."));
    }
}
