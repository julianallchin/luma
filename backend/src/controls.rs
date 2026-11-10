//! A venue's live controls: named values 0–1 that faders, pads, MIDI, OSC
//! and any other input set while the lights play.
//!
//! A control exists by being named. A score's pattern that reads `drop` (a
//! fader or button node named `drop`) makes `drop` a control of its venue,
//! and it reads 0 until something sets it.
//!
//! The Perform set plays through controls too, under names of its own
//! ([`perform`]): each pattern's level, each fader, live color and button
//! node of its graph (its pad among them), each fader, group and list cell,
//! each list's values, the master, tap, Select and Clear. So everything
//! that can move a control (an on-screen pad, a MIDI key, an OSC message)
//! can play the set, and the bank is the one place that enforces how they
//! play.
//!
//! A control is a fader, a button or a live color. A fader holds the value
//! it was set to. A button is declared with the [`Press`] that plays it
//! ([`install`]): the bank keeps its gate (when it went on and off) and its
//! value is its envelope through that gate, now. A press flips a toggle and
//! holds a flash; setting a button outright gates it on above ½ and off
//! below. A name pressed that no button declares is momentary: 1 while
//! held. A fader cell moves every one of its targets, and a tap lands on
//! the venue's tempo.
//!
//! A button whose press starts on the beat or the bar ([`Start`]) gates on
//! or off at the set's next one: the bank keeps that time, still to come, as
//! the gate's, so its value stays a pure function of time. The opposite
//! change before then calls it off.
//!
//! Perform plays on two axes: where (venue groups) and the stack (patterns
//! over each other). The rule is: choose where, then do something; nothing
//! chosen means everything. A group cell tapped is chosen or not, and stays
//! so; held while another control is pressed, that one press acts on the
//! held group alone and the choice stays as it was. Several chosen or held
//! act on all of them.
//!
//! Each pattern with a pad is an effect: on or off, and where it plays, a
//! set of groups (none is its own selection). Its pad tapped with nothing
//! chosen turns it on or off where it was made to play. Tapped with groups
//! chosen, it comes on there if it was off; if it was on there already it
//! leaves them (and goes off when it is left nowhere), else it adds them.
//! A one-at-a-time row takes the groups an effect comes on at from the
//! row's others, each going off when it is left nowhere; an effect on its
//! own selection takes the whole row.
//!
//! A list cell holds colors or numbers. A value tapped sets the list's
//! targets to it, a color fading by the list's fade. A color list with no
//! targets sets the base color: a color per light, under every effect,
//! which lights nothing itself. It sets the lights of the groups chosen, or
//! every light. Pressed while Select is held (or after Select is tapped, which
//! latches it), an effect is chosen instead of played; the next color tapped
//! on a list with no targets sets its own color, or with the color below
//! sets it back to taking the color under it (the blend
//! [`luma_patterns::BlendMode::Below`]). The blend is the pattern's, as
//! stored: the bank hands the new one to the set's write path
//! ([`VenueControls::restyle_to`]) and plays it once the set is installed
//! again. Clear tapped chooses nothing; held, it lets every effect go.
//!
//! The bank keeps each fade (from, to, when and how long), so a color too is
//! a pure function of time. A list that steps moves on to its next value
//! every so many bars of the set's clock.
//!
//! The set's clock is the bank's own tapped tempo, its bars starting at a
//! run's first tap, or, while the set follows a deck, the deck's beat grid
//! as the engine reads it at each frame ([`VenueControls::tick`]). Lists
//! step on it and quantised presses land on it.
//!
//! Values are live state, like the render engine's, so they live here
//! rather than in the database: every scene an app compiles for a venue
//! reads that app's one bank of the venue ([`ControlBanks`]), and setting a
//! value never recompiles. Where an effect plays changes what the engine
//! plays; it sees each change by [`VenueControls::changed`].
//!
//! [`install`]: VenueControls::install

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use luma_patterns::clip_graph::{LiveInput, LiveInputKind};
use luma_patterns::press::{Button, Gate, Press, PressMode, Presses, Shape, Start, TimeUnit};
use luma_patterns::BlendMode;
use serde::Serialize;

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::perform::{CellContent, ListValue, PerformLayout, PerformPattern, Step, Tempo};
use crate::models::universe::UniverseState;
pub use surface::{Job, Light, Look, Shine, Surface, AMBER};

/// The names the Perform set plays under, all `perform_…`, and which of
/// them each cell has ([`perform::parts`]). Nothing lists them as the
/// venue's own.
pub mod perform {
    use luma_patterns::clip_graph::LiveInputKind;

    use crate::models::perform::{Cell, CellContent, ControlTarget, PerformPattern};

    /// Scales every Perform pattern, 1 until moved.
    pub const MASTER: &str = "perform_master";
    /// Pressed on each beat to tap the tempo.
    pub const TAP: &str = "perform_tap";
    /// Held, or tapped to latch, while effects are pressed: it chooses them.
    pub const SELECT: &str = "perform_select";
    /// Tapped, nothing is chosen; held, every effect is let go.
    pub const CLEAR: &str = "perform_clear";

    /// Pattern `id`'s level, 1 until moved.
    pub fn level(id: &str) -> String {
        format!("perform_p{}_level", key(id))
    }

    /// The fader or button node `node` of pattern `id`'s graph, by node id,
    /// so renaming the node keeps whatever is bound to it.
    pub fn node(id: &str, node: &str) -> String {
        format!("perform_p{}_n_{}", key(id), key(node))
    }

    /// `pattern`'s pad, the button node of its graph that turns it on and
    /// off ([`luma_patterns::ClipGraph::pad`]); `None` when it has none.
    pub fn pad(pattern: &PerformPattern) -> Option<String> {
        Some(node(&pattern.id, &pattern.graph.pad()?.node))
    }

    /// Cell `id`'s own control: a fader cell's value, which moves its
    /// targets; a group cell's, pressed to choose it; or a list cell's,
    /// pressed for its next value.
    pub fn cell(id: &str) -> String {
        format!("perform_c{}", key(id))
    }

    /// Value `index` of list cell `id`, from 0: pressed, it is tapped.
    pub fn value(id: &str, index: usize) -> String {
        format!("perform_c{}_v{index}", key(id))
    }

    /// The control a fader or list cell's `target` moves.
    pub fn target(target: &ControlTarget) -> String {
        match target {
            ControlTarget::Level { pattern_id } => level(pattern_id),
            ControlTarget::Node {
                pattern_id,
                node: id,
            } => node(pattern_id, id),
            ControlTarget::Named { name } => name.clone(),
        }
    }

    /// Whether `name` is one of the Perform set's, not the venue's own.
    pub fn owns(name: &str) -> bool {
        name.starts_with("perform_")
    }

    /// One thing of a cell a hand plays, and the control it moves. An
    /// empty label is the cell's own name.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Part {
        pub control: String,
        pub label: String,
        pub kind: PartKind,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum PartKind {
        /// A pattern's pad: the button node its graph's alpha depends on.
        Pad,
        /// A fader, and what it reads before it is first moved.
        Fader {
            rest: f64,
        },
        /// Another button node of a pattern's graph.
        Button,
        Tap,
        /// A live color node of a pattern's graph, which a list sets.
        Color,
        /// Value `index` of a list: pressed, it is tapped.
        Value {
            index: usize,
        },
        /// A list's own control: pressed, its next value is tapped.
        Next,
        /// A group cell: tapped, it is chosen or no longer; held, a press
        /// meanwhile acts on it alone.
        Group,
        Select,
        Clear,
    }

    impl PartKind {
        /// The way a controller binds to it; `None` for a live color,
        /// which only a list sets.
        pub fn mode(self) -> Option<crate::inputs::Mode> {
            match self {
                Self::Fader { .. } => Some(crate::inputs::Mode::Fader),
                Self::Color => None,
                _ => Some(crate::inputs::Mode::Momentary),
            }
        }
    }

    /// What a hand plays in `cell`, of a set of `patterns`: for a pattern
    /// its pad first, when its graph has one, then its level and its
    /// graph's other faders, buttons and live colors; for a list each
    /// value, then its Next.
    pub fn parts(cell: &Cell, patterns: &[PerformPattern]) -> Vec<Part> {
        let part = |control: String, label: &str, kind| Part {
            control,
            label: label.to_owned(),
            kind,
        };
        match &cell.content {
            CellContent::Pattern { pattern_id } => {
                let level = part(level(pattern_id), "Level", PartKind::Fader { rest: 1. });
                let Some(pattern) = patterns.iter().find(|one| &one.id == pattern_id) else {
                    return vec![level];
                };
                let pad = pattern.graph.pad().map(|pad| pad.node);
                let mut parts: Vec<Part> = pad
                    .iter()
                    .map(|id| part(node(pattern_id, id), "Pad", PartKind::Pad))
                    .collect();
                parts.push(level);
                parts.extend(
                    pattern
                        .graph
                        .live_inputs()
                        .into_iter()
                        .filter(|input| pad.as_ref() != Some(&input.node))
                        .map(|input| {
                            let kind = match input.kind {
                                LiveInputKind::Fader => PartKind::Fader { rest: 0. },
                                LiveInputKind::Color => PartKind::Color,
                                LiveInputKind::Button(_) => PartKind::Button,
                            };
                            part(node(pattern_id, &input.node), &input.name, kind)
                        }),
                );
                parts
            }
            CellContent::Master => vec![part(MASTER.into(), "", PartKind::Fader { rest: 1. })],
            CellContent::Tap => vec![part(TAP.into(), "", PartKind::Tap)],
            CellContent::Select => vec![part(SELECT.into(), "", PartKind::Select)],
            CellContent::Clear => vec![part(CLEAR.into(), "", PartKind::Clear)],
            CellContent::Fader { .. } => {
                vec![part(self::cell(&cell.id), "", PartKind::Fader { rest: 0. })]
            }
            CellContent::List { values, .. } => (0..values.len())
                .map(|index| {
                    part(
                        value(&cell.id, index),
                        &format!("Value {}", index + 1),
                        PartKind::Value { index },
                    )
                })
                .chain([part(self::cell(&cell.id), "Next", PartKind::Next)])
                .collect(),
            CellContent::Group { .. } => vec![part(self::cell(&cell.id), "", PartKind::Group)],
        }
    }

    /// An id as a control-name part: lowercase letters and digits only, so
    /// a UUID is 32 characters and every name stays within the 64 a control
    /// name may have.
    fn key(id: &str) -> String {
        id.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }
}

/// Clear held this long lets every effect go; let go sooner, it is a tap.
pub const CLEAR_HOLD: Duration = Duration::from_millis(500);

/// The group that is every light: what nothing chosen acts on. Every other
/// group is named by its id.
pub const ALL: &str = "all";

/// The lights of `groups` together, as a selection.
#[must_use]
pub fn selection(groups: &[String]) -> luma_patterns::Selection {
    if groups.iter().any(|group| group == ALL) {
        return luma_patterns::Selection::All;
    }
    luma_patterns::Selection::groups(groups)
}

/// One venue's control values, its buttons and its tempo.
#[derive(Debug)]
pub struct VenueControls {
    bank: RwLock<Bank>,
}

#[derive(Debug, Default)]
struct Bank {
    /// Every fader's value, and every undeclared name pressed.
    values: BTreeMap<String, f64>,
    buttons: HashMap<String, Held>,
    /// Each pad of a one-at-a-time row, to every pad of its row.
    exclusive: HashMap<String, Arc<[String]>>,
    /// Each fader cell's control, to the controls it moves.
    links: HashMap<String, Arc<[String]>>,
    /// Every live color set so far.
    colors: HashMap<String, Fading>,
    /// Each list cell, by cell id.
    lists: HashMap<String, List>,
    /// Each list's controls: a value's, to its list and its place there,
    /// and the list's own, to its list and no place (its next value).
    taps: HashMap<String, (String, Option<usize>)>,
    /// Each pad, to its pattern's id.
    pads: HashMap<String, String>,
    /// Every pattern, by id.
    effects: HashMap<String, Effect>,
    /// Each group cell's control, to its group.
    groups: HashMap<String, String>,
    /// Each group's lights, `ALL` among them.
    lights: HashMap<String, Arc<[String]>>,
    /// The groups chosen.
    chosen: BTreeSet<String>,
    /// Each group cell held down, and whether another control was pressed
    /// while it was, so its release is no tap.
    held: BTreeMap<String, bool>,
    select: Select,
    /// The effects chosen by Select, by pattern id.
    selected: BTreeSet<String>,
    /// When Clear went down, while it is.
    clearing: Option<Instant>,
    /// Where Select sends an effect's new blend, to be stored: the set's
    /// one write path stores it and installs the set again.
    restyle: Option<tokio::sync::mpsc::UnboundedSender<(String, BlendMode)>>,
    /// Each light's base color.
    base: HashMap<String, Fading>,
    /// Each control on the layout's pages, and what it is.
    faces: BTreeMap<String, surface::Face>,
    /// Bumped whenever an effect's groups change, for the engine.
    changed: u64,
    /// Bumped whenever a set is installed: the set as stored may differ.
    installs: u64,
    tempo: TapTempo,
    /// The deck the set follows, while it follows one.
    deck: Option<Deck>,
}

/// Select: down, and whether an effect or value was pressed meanwhile, so
/// its release is no tap; and latched, by a tap.
#[derive(Clone, Copy, Debug, Default)]
struct Select {
    down: Option<bool>,
    latched: bool,
}

impl Select {
    fn on(self) -> bool {
        self.down.is_some() || self.latched
    }
}

/// A pattern as the bank plays it.
#[derive(Clone, Debug)]
struct Effect {
    /// The groups it plays on, sorted; none is its own selection.
    groups: Vec<String>,
    /// Its own color: the first live color node of its graph.
    color: Option<String>,
    /// Its blend as stored. Select changes it by storing another.
    blend: BlendMode,
    /// Its own selection ([`luma_patterns::Selection::key`]), where it plays
    /// on no groups.
    selection: String,
}

/// The deck the set follows: its tempo, and its beat as the engine last
/// read it.
#[derive(Clone, Copy, Debug)]
struct Deck {
    bpm: f64,
    read: Option<Reading>,
}

/// The deck's beat at `at`, on a beat grid of `beats_per_bar`.
#[derive(Clone, Copy, Debug)]
struct Reading {
    beat: f64,
    at: Instant,
    beats_per_bar: f64,
}

/// Seconds from `then` to `at`, less than 0 when `then` is later.
fn seconds(then: Instant, at: Instant) -> f64 {
    match at.checked_duration_since(then) {
        Some(since) => since.as_secs_f64(),
        None => -then.duration_since(at).as_secs_f64(),
    }
}

/// A live color nothing has set: white.
pub const UNSET_COLOR: [f64; 3] = [1.0; 3];

/// A live color's move from one color to another, each linear Rec. 2020
/// rgb, over `length` in `unit`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fade {
    pub from: [f64; 3],
    pub to: [f64; 3],
    pub length: f64,
    pub unit: TimeUnit,
}

/// A live color as it stands: its last fade, begun `since` seconds ago.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveColor {
    pub fade: Fade,
    pub since: f64,
}

impl LiveColor {
    /// How far through its fade it is, 0–1, at `bpm`.
    fn progress(&self, bpm: f64) -> f64 {
        let length = self.fade.unit.seconds(self.fade.length, bpm);
        if length <= 0.0 {
            1.0
        } else {
            (self.since / length).clamp(0.0, 1.0)
        }
    }

    /// Its color now, at `bpm` (for a fade in beats).
    pub fn value(&self, bpm: f64) -> [f64; 3] {
        let t = Shape::Smooth.eval(self.progress(bpm));
        let Fade { from, to, .. } = self.fade;
        std::array::from_fn(|c| from[c] + (to[c] - from[c]) * t)
    }

    /// Whether it is still fading.
    pub fn moving(&self, bpm: f64) -> bool {
        self.progress(bpm) < 1.0
    }

    /// As it stands `seconds` later, nothing set meanwhile.
    pub fn later(self, seconds: f64) -> Self {
        Self {
            since: self.since + seconds,
            ..self
        }
    }
}

/// A color's last fade and when it began.
#[derive(Clone, Copy, Debug)]
struct Fading {
    fade: Fade,
    start: Instant,
}

impl Fading {
    fn at(&self, at: Instant) -> LiveColor {
        LiveColor {
            fade: self.fade,
            since: at.saturating_duration_since(self.start).as_secs_f64(),
        }
    }
}

/// A list cell as the bank plays it: its values, fade, targets and step,
/// from the layout, and the value on, which is live.
#[derive(Clone, Debug)]
struct List {
    values: Arc<[ListValue]>,
    fade: f64,
    unit: TimeUnit,
    targets: Arc<[String]>,
    step: Option<Step>,
    /// The value on, `None` until one is tapped.
    on: Option<usize>,
    /// The bar of the set's clock the value on is counted from, once the
    /// clock has been read since it was tapped.
    counted_from: Option<i64>,
}

/// A declared button: how it plays, and when it goes on and off, either
/// still to come while a press waits for its beat; `None` until first
/// pressed. Beside its gate, its turns on: what a press clock counts.
#[derive(Clone, Debug)]
struct Held {
    press: Press,
    gate: Option<(Instant, Option<Instant>)>,
    turns_on: TurnsOn,
}

/// How many times a button has turned on, and when its last [`KEPT_TURNS`]
/// did (or will, while a press waits for its beat), oldest first.
#[derive(Clone, Debug, Default)]
struct TurnsOn {
    count: u64,
    at: VecDeque<Instant>,
}

/// The turns on of a button a press clock reads: more than a beat's worth
/// of hits pressed as fast as hands go.
const KEPT_TURNS: usize = 16;

impl TurnsOn {
    fn push(&mut self, at: Instant) {
        self.count += 1;
        self.at.push_back(at);
        if self.at.len() > KEPT_TURNS {
            self.at.pop_front();
        }
    }

    /// Call off the last, which was still to come.
    fn pop(&mut self) {
        if self.at.pop_back().is_some() {
            self.count -= 1;
        }
    }
}

impl Held {
    /// On, or going on.
    fn on(&self) -> bool {
        matches!(self.gate, Some((_, None)))
    }

    /// The button as it stands at `at`.
    fn at(&self, at: Instant) -> Button {
        let since = |then: Instant| seconds(then, at);
        Button {
            press: self.press,
            gate: self.gate.map(|(on, off)| Gate {
                on_for: since(on),
                off_for: off.map(since),
            }),
        }
    }
}

impl Default for VenueControls {
    fn default() -> Self {
        Self {
            bank: RwLock::new(Bank {
                values: BTreeMap::from([(perform::MASTER.to_owned(), 1.0)]),
                ..Bank::default()
            }),
        }
    }
}

/// `groups` sorted, each once.
fn sorted(groups: impl IntoIterator<Item = String>) -> Vec<String> {
    groups
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl Bank {
    /// The deck the set's clock reads: the one it follows, once the engine
    /// has read its beat. Until then the set's clock is the tapped tempo,
    /// its beat and its tempo both.
    fn read_deck(&self) -> Option<(f64, Reading)> {
        self.deck.and_then(|deck| Some((deck.bpm, deck.read?)))
    }

    /// The tempo a button's beats are counted in: the set's clock's.
    fn bpm(&self) -> f64 {
        self.read_deck().map_or(self.tempo.bpm, |(bpm, _)| bpm)
    }

    /// The set's beat at `at`, and the beats in its bar: the read deck's,
    /// run on at its tempo; else the tapped tempo's.
    fn beat(&self, at: Instant) -> (f64, f64) {
        match self.read_deck() {
            Some((bpm, read)) => (
                read.beat + seconds(read.at, at) * bpm / 60.0,
                read.beats_per_bar,
            ),
            None => (self.tempo.beat_at(at), TAPPED_BEATS_PER_BAR),
        }
    }

    /// How many beats `start` counts in: none, one, or a bar's.
    fn every(&self, start: Start, at: Instant) -> f64 {
        match start {
            Start::Now => 0.0,
            Start::Beat => 1.0,
            Start::Bar => self.beat(at).1,
        }
    }

    /// `beats` of the set's tempo after `at`.
    fn after_beats(&self, at: Instant, beats: f64) -> Instant {
        at + Duration::from_secs_f64((beats * 60.0 / self.bpm()).max(0.0))
    }

    /// When a change asked for at `at` takes effect by `start`: at once, or
    /// on the set's next beat or bar.
    fn when(&self, start: Start, at: Instant) -> Instant {
        let every = self.every(start, at);
        if every <= 0.0 {
            return at;
        }
        let beat = self.beat(at).0;
        self.after_beats(at, (beat / every).ceil() * every - beat)
    }

    /// The groups an action acts on: those held down, else those chosen;
    /// none is everything.
    fn aimed(&self) -> Vec<String> {
        if self.held.is_empty() {
            self.chosen.iter().cloned().collect()
        } else {
            sorted(
                self.held
                    .keys()
                    .filter_map(|control| self.groups.get(control).cloned()),
            )
        }
    }

    /// Press (`down`) or release `name` at `at`. A press marks every group
    /// held, and Select, as used: their release is no tap.
    fn press(&mut self, name: &str, down: bool, at: Instant) {
        if down {
            for (control, used) in &mut self.held {
                *used |= control != name;
            }
            if name != perform::SELECT {
                if let Some(used) = &mut self.select.down {
                    *used = true;
                }
            }
        }
        if self.groups.contains_key(name) {
            return self.hold(name, down);
        }
        match name {
            perform::SELECT => return self.press_select(down),
            perform::CLEAR => return self.press_clear(down, at),
            _ => {}
        }
        if let Some(pattern) = self.pads.get(name).cloned() {
            return self.press_pad(name, &pattern, down, at);
        }
        let Some((mode, on)) = self
            .buttons
            .get(name)
            .map(|held| (held.press.mode, held.on()))
        else {
            return self.set(name, f64::from(u8::from(down)), at);
        };
        match mode {
            PressMode::Toggle if down => self.gate(name, !on, at),
            PressMode::Toggle => {}
            PressMode::Flash => self.gate(name, down, at),
        }
    }

    /// A group cell's control down or let go: let go with nothing pressed
    /// meanwhile, its group is chosen, or no longer.
    fn hold(&mut self, control: &str, down: bool) {
        if down {
            self.held.insert(control.to_owned(), false);
            return;
        }
        if self.held.remove(control) == Some(false) {
            let group = self.groups[control].clone();
            if !self.chosen.remove(&group) {
                self.chosen.insert(group);
            }
        }
    }

    /// Select down, or let go: let go with nothing pressed meanwhile, it
    /// latches, or unlatches and chooses no effect.
    fn press_select(&mut self, down: bool) {
        if down {
            self.select.down = Some(false);
        } else if self.select.down.take() == Some(false) {
            self.select.latched = !self.select.latched;
            if !self.select.latched {
                self.selected.clear();
            }
        }
    }

    /// Clear down, or let go: held long, every effect goes; else nothing is
    /// chosen.
    fn press_clear(&mut self, down: bool, at: Instant) {
        if down {
            self.clearing = Some(at);
            return;
        }
        match self.clearing.take() {
            Some(since) if at.saturating_duration_since(since) >= CLEAR_HOLD => {
                self.release_all(at)
            }
            Some(_) => self.clear(),
            None => {}
        }
    }

    /// Let every effect go.
    fn release_all(&mut self, at: Instant) {
        let pads: Vec<String> = self.pads.keys().cloned().collect();
        for pad in pads {
            self.gate(&pad, false, at);
        }
    }

    /// Choose nothing: no group, no effect, Select unlatched.
    fn clear(&mut self) {
        self.chosen.clear();
        self.selected.clear();
        self.select.latched = false;
    }

    /// Pad `pad` of `pattern` pressed or let go. While Select is on, a press
    /// chooses the effect, or no longer. A flash plays while held, on the
    /// groups aimed at; a toggle plays by the rule of the module's doc.
    fn press_pad(&mut self, pad: &str, pattern: &str, down: bool, at: Instant) {
        if down && self.select.on() {
            if !self.selected.remove(pattern) {
                self.selected.insert(pattern.to_owned());
            }
            return;
        }
        let Some((mode, on)) = self
            .buttons
            .get(pad)
            .map(|held| (held.press.mode, held.on()))
        else {
            return;
        };
        let aimed = self.aimed();
        match mode {
            PressMode::Flash if down => self.play(pad, pattern, aimed.clone(), &aimed, at),
            PressMode::Flash => self.gate(pad, false, at),
            PressMode::Toggle if !down => {}
            PressMode::Toggle if aimed.is_empty() => match on {
                true => self.gate(pad, false, at),
                false => self.play(pad, pattern, Vec::new(), &[], at),
            },
            PressMode::Toggle if !on => self.play(pad, pattern, aimed.clone(), &aimed, at),
            PressMode::Toggle => {
                let groups = self.effects[pattern].groups.clone();
                if aimed.iter().all(|group| groups.contains(group)) {
                    let left: Vec<String> = groups
                        .iter()
                        .filter(|group| !aimed.contains(group))
                        .cloned()
                        .collect();
                    if left.is_empty() {
                        self.gate(pad, false, at);
                    } else {
                        self.aim(pattern, left);
                    }
                } else {
                    let more = groups.iter().cloned().chain(aimed.iter().cloned());
                    self.play(pad, pattern, sorted(more), &aimed, at);
                }
            }
        }
    }

    /// Play `pattern` by its pad `pad` on `groups`, and take `claimed` from
    /// its one-at-a-time row's others when it comes on.
    fn play(
        &mut self,
        pad: &str,
        pattern: &str,
        groups: Vec<String>,
        claimed: &[String],
        at: Instant,
    ) {
        self.aim(pattern, groups);
        self.gate(pad, true, at);
        let from = self
            .buttons
            .get(pad)
            .and_then(|button| button.gate)
            .map_or(at, |(on, _)| on.max(at));
        let Some(row) = self.exclusive.get(pad).cloned() else {
            return;
        };
        for other in row.iter().filter(|other| *other != pad) {
            let Some(theirs) = self.pads.get(other).cloned() else {
                continue;
            };
            let on = self.buttons.get(other).is_some_and(Held::on);
            let groups = self.effects[&theirs].groups.clone();
            let left: Vec<String> = groups
                .iter()
                .filter(|group| !claimed.contains(group))
                .cloned()
                .collect();
            if on && !claimed.is_empty() && !left.is_empty() {
                self.aim(&theirs, left);
            } else {
                // Off, or going off, it goes no later than this comes on.
                self.release(other, at, from);
            }
        }
    }

    /// Play `pattern` on `groups` from now.
    fn aim(&mut self, pattern: &str, groups: Vec<String>) {
        if let Some(effect) = self.effects.get_mut(pattern) {
            if effect.groups != groups {
                effect.groups = groups;
                self.changed += 1;
            }
        }
    }

    /// Set `name` and, for a fader cell, its targets. A target is set as
    /// itself, never through its own links, so no two cells can loop.
    fn set(&mut self, name: &str, value: f64, at: Instant) {
        let targets = self.links.get(name).cloned();
        self.set_one(name, value, at);
        for target in targets.iter().flat_map(|targets| targets.iter()) {
            self.set_one(target, value, at);
        }
    }

    /// Set `name` outright: a group is chosen above ½, Select latched, a
    /// pad on (where the groups aimed at say) or off, any other button
    /// gated; Clear, tap and a list's controls act as they rise past ½.
    fn set_one(&mut self, name: &str, value: f64, at: Instant) {
        let on = value > 0.5;
        if let Some(group) = self.groups.get(name).cloned() {
            if on {
                self.chosen.insert(group);
            } else {
                self.chosen.remove(&group);
            }
            return;
        }
        if name == perform::SELECT {
            self.select.latched = on;
            if !on {
                self.selected.clear();
            }
            return;
        }
        if let Some(pattern) = self.pads.get(name).cloned() {
            let aimed = self.aimed();
            return match on {
                true => self.play(name, &pattern, aimed.clone(), &aimed, at),
                false => self.gate(name, false, at),
            };
        }
        if self.buttons.contains_key(name) {
            return self.gate(name, on, at);
        }
        let was = self.values.insert(name.to_owned(), value).unwrap_or(0.0);
        if !on || was > 0.5 {
            return;
        }
        if name == perform::TAP {
            self.tempo.tap(at);
        }
        if name == perform::CLEAR {
            self.clear();
        }
        if let Some((list, index)) = self.taps.get(name).cloned() {
            let index = index.unwrap_or_else(|| {
                let list = &self.lists[&list];
                list.on.map_or(0, |on| (on + 1) % list.values.len())
            });
            self.tap_value(&list, index, at);
        }
    }

    /// Value `index` of list `id` tapped at `at`: with effects chosen by
    /// Select and no targets, their color; else what [`Self::put`] sets. A
    /// color is an effect's own: its live color fades to it here, and one
    /// stored as Below is stored again as Replace. The color below is
    /// stored as Below.
    fn tap_value(&mut self, id: &str, index: usize, at: Instant) {
        let Some(list) = self.lists.get(id) else {
            return;
        };
        let Some(&value) = list.values.get(index) else {
            return;
        };
        if !list.targets.is_empty() || self.selected.is_empty() || !value.is_color() {
            return self.put(id, index, None, at);
        }
        let (length, unit) = (list.fade, list.unit);
        for pattern in std::mem::take(&mut self.selected) {
            let Some(effect) = self.effects.get(&pattern) else {
                continue;
            };
            let blend = match value {
                ListValue::Below => BlendMode::Below,
                _ if effect.blend == BlendMode::Below => BlendMode::Replace,
                _ => effect.blend,
            };
            let color = effect.color.clone();
            if blend != effect.blend {
                if let Some(restyle) = &self.restyle {
                    let _ = restyle.send((pattern.clone(), blend));
                }
            }
            if let (ListValue::Color(to), Some(color)) = (value, color) {
                self.fade(&color, to, length, unit, at);
            }
        }
        self.select.latched = false;
    }

    /// Put list `id` on value `index`, counted from bar `counted_from`
    /// (`None`: the next the clock reads): its targets fade or move to it,
    /// or with none, a color is the base color of the lights aimed at.
    fn put(&mut self, id: &str, index: usize, counted_from: Option<i64>, at: Instant) {
        let Some(list) = self.lists.get_mut(id) else {
            return;
        };
        let Some(&value) = list.values.get(index) else {
            return;
        };
        list.on = Some(index);
        list.counted_from = counted_from;
        let (targets, length, unit) = (list.targets.clone(), list.fade, list.unit);
        match value {
            ListValue::Number(number) => {
                for target in targets.iter() {
                    self.set_one(target, number, at);
                }
            }
            ListValue::Color(to) if targets.is_empty() => {
                let aimed = self.aimed();
                self.paint(&aimed, to, length, unit, at);
            }
            ListValue::Color(to) => {
                for target in targets.iter() {
                    self.fade(target, to, length, unit, at);
                }
            }
            ListValue::Below => {}
        }
    }

    /// Fade the base color of the lights of `groups` (none: every light)
    /// from where each is to `to`.
    fn paint(&mut self, groups: &[String], to: [f64; 3], length: f64, unit: TimeUnit, at: Instant) {
        let groups = if groups.is_empty() {
            &[ALL.to_owned()][..]
        } else {
            groups
        };
        let lights: BTreeSet<String> = groups
            .iter()
            .filter_map(|group| self.lights.get(group))
            .flat_map(|lights| lights.iter())
            .cloned()
            .collect();
        for light in lights {
            let from = self.base_at(&light, at);
            self.base.insert(light, faded(from, to, length, unit, at));
        }
    }

    /// Fade live color `name` from where it is to `to`.
    fn fade(&mut self, name: &str, to: [f64; 3], length: f64, unit: TimeUnit, at: Instant) {
        let from = self.color(name, at);
        self.colors
            .insert(name.to_owned(), faded(from, to, length, unit, at));
    }

    /// Move each stepping list on whose bars are up at `at`. A list's count
    /// starts at the first bar read after its value was tapped, and again
    /// when the clock goes back past it.
    fn step(&mut self, at: Instant) {
        if !self.lists.values().any(|list| list.step.is_some()) {
            return;
        }
        let (beat, beats_per_bar) = self.beat(at);
        let bar = (beat / beats_per_bar).floor() as i64;
        let mut due = Vec::new();
        for (id, list) in &mut self.lists {
            let (Some(step), Some(on)) = (list.step, list.on) else {
                continue;
            };
            match list.counted_from {
                Some(from) if (from..from + i64::from(step.bars)).contains(&bar) => {}
                Some(from) if bar > from => {
                    list.counted_from = Some(bar);
                    let next = if on + 1 < list.values.len() {
                        Some(on + 1)
                    } else {
                        step.loops.then_some(0)
                    };
                    due.extend(next.map(|next| (id.clone(), next)));
                }
                _ => list.counted_from = Some(bar),
            }
        }
        for (id, index) in due {
            self.put(&id, index, Some(bar), at);
        }
    }

    fn color(&self, name: &str, at: Instant) -> [f64; 3] {
        self.colors
            .get(name)
            .map_or(UNSET_COLOR, |fading| fading.at(at).value(self.bpm()))
    }

    /// Light `light`'s base color at `at`, white while none is set.
    fn base_at(&self, light: &str, at: Instant) -> [f64; 3] {
        self.base
            .get(light)
            .map_or(UNSET_COLOR, |fading| fading.at(at).value(self.bpm()))
    }

    /// Gate declared button `name` on or off, asked for at `at`: at once,
    /// or on the set's next beat or bar, as its press starts. One on or
    /// going on stays so, and one off or going off. A change still to come
    /// is called off by the opposite one, but a flash let go before its beat
    /// stays on for one, so a tap lands.
    /// A new gate on is a turn on, which a press clock counts.
    fn gate(&mut self, name: &str, on: bool, at: Instant) {
        let Some((press, gate)) = self.buttons.get(name).map(|held| (held.press, held.gate)) else {
            return;
        };
        let when = self.when(press.start, at);
        let gate = match (on, gate) {
            (true, Some((_, None))) => return,
            (true, Some((since, Some(off)))) if off > at => (since, None),
            (true, _) => (when, None),
            (false, Some((since, None))) if since > at && press.mode == PressMode::Flash => {
                let beats = self.every(press.start, since);
                (since, Some(self.after_beats(since, beats)))
            }
            (false, _) => return self.release(name, at, when),
        };
        if let Some(button) = self.buttons.get_mut(name) {
            if button.gate.is_none_or(|(since, _)| since != gate.0) {
                button.turns_on.push(gate.0);
            }
            button.gate = Some(gate);
        }
    }

    /// Let go of declared button `name` at `when`, or sooner if it goes off
    /// sooner, asked for at `at`: one whose on is still to come at `at` is
    /// called off instead.
    fn release(&mut self, name: &str, at: Instant, when: Instant) {
        let Some(button) = self.buttons.get_mut(name) else {
            return;
        };
        button.gate = match button.gate {
            Some((since, _)) if since > at => {
                button.turns_on.pop();
                None
            }
            Some((since, off)) if off.is_none_or(|off| off > when) => Some((since, Some(when))),
            gate => gate,
        };
    }

    /// Declared button `name`'s turns on, each on the set's clock as it
    /// reads now: one still to come is a beat still to come.
    fn presses(&self, name: &str) -> Presses {
        self.buttons
            .get(name)
            .map(|button| Presses {
                count: button.turns_on.count,
                beats: button
                    .turns_on
                    .at
                    .iter()
                    .map(|on| self.beat(*on).0)
                    .collect(),
            })
            .unwrap_or_default()
    }

    fn value(&self, name: &str, at: Instant) -> f64 {
        match self.buttons.get(name) {
            Some(button) => button.at(at).value(self.bpm()),
            None => self.values.get(name).copied().unwrap_or(0.0),
        }
    }
}

/// A fade from `from` to `to` over `length` in `unit`, begun at `at`.
fn faded(from: [f64; 3], to: [f64; 3], length: f64, unit: TimeUnit, at: Instant) -> Fading {
    Fading {
        fade: Fade {
            from,
            to,
            length,
            unit,
        },
        start: at,
    }
}

impl VenueControls {
    /// Set `name` to `value`, clamped to 0–1. A button gates on above ½ and
    /// off below, whatever its mode, a pad on the groups chosen; a fader
    /// cell sets its targets too; a group is chosen above ½; pressing
    /// [`perform::TAP`] taps the tempo.
    pub fn set(&self, name: &str, value: f64) {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.write().set(name, value, Instant::now());
    }

    /// Press (`down`) or release control `name` at `at`. A toggle flips on
    /// the press and ignores the release; a flash is on while held; a pad,
    /// a group, Select and Clear play as the module's doc says. A name
    /// nothing declares is momentary: 1 while held.
    pub fn press(&self, name: &str, down: bool, at: Instant) {
        self.write().press(name, down, at);
    }

    /// Choose exactly `groups`, of those a group cell holds: none is
    /// everything.
    pub fn choose(&self, groups: &[String]) {
        let mut bank = self.write();
        let chosen = groups
            .iter()
            .filter(|group| bank.groups.values().any(|held| held == *group))
            .cloned()
            .collect();
        bank.chosen = chosen;
    }

    /// Choose nothing, as Clear tapped does; with `effects`, let every
    /// effect go instead, as Clear held does.
    pub fn clear(&self, effects: bool) {
        let mut bank = self.write();
        match effects {
            true => bank.release_all(Instant::now()),
            false => bank.clear(),
        }
    }

    /// Fade the base color of the lights of `groups` (none: every light)
    /// to `color` over `fade` in `unit`.
    pub fn base(&self, groups: &[String], color: [f64; 3], fade: f64, unit: TimeUnit) {
        self.write()
            .paint(groups, color, fade, unit, Instant::now());
    }

    /// Choose the effects of `patterns` by Select, so the next color tapped
    /// on a list with no targets sets theirs.
    pub fn select(&self, patterns: &[String]) {
        let mut bank = self.write();
        let selected = patterns
            .iter()
            .filter(|id| bank.effects.contains_key(*id))
            .cloned()
            .collect();
        bank.selected = selected;
    }

    /// Control `name` now: a fader's value, a button's envelope, 0 for a
    /// name nothing has set.
    pub fn value(&self, name: &str) -> f64 {
        self.value_at(name, Instant::now())
    }

    pub(crate) fn value_at(&self, name: &str, at: Instant) -> f64 {
        self.read().value(name, at)
    }

    /// Live color `name` now: linear Rec. 2020 rgb, [`UNSET_COLOR`] for a
    /// name nothing has set.
    pub fn color(&self, name: &str) -> [f64; 3] {
        self.color_at(name, Instant::now())
    }

    pub(crate) fn color_at(&self, name: &str, at: Instant) -> [f64; 3] {
        self.read().color(name, at)
    }

    /// Button `name`'s turns on, on the set's clock: what a press clock on
    /// it counts. None for a name no button declares.
    pub fn presses(&self, name: &str) -> Presses {
        self.read().presses(name)
    }

    /// Everything the bank plays, as it stands now.
    pub fn playing(&self) -> Playing {
        let now = Instant::now();
        let bank = self.read();
        let base = bank
            .lights
            .iter()
            .filter(|(_, lights)| lights.iter().any(|light| bank.base.contains_key(light)))
            .map(|(group, _)| (group.clone(), bank.base_of(group, now)))
            .collect();
        Playing {
            controls: bank.values.clone(),
            buttons: bank
                .buttons
                .iter()
                .map(|(name, held)| (name.clone(), held.at(now)))
                .collect(),
            colors: bank
                .colors
                .iter()
                .map(|(name, fading)| (name.clone(), fading.at(now)))
                .collect(),
            lists: bank
                .lists
                .iter()
                .filter_map(|(id, list)| Some((id.clone(), list.on?)))
                .collect(),
            chosen: bank.chosen.iter().cloned().collect(),
            held: sorted(
                bank.held
                    .keys()
                    .filter_map(|control| bank.groups.get(control).cloned()),
            ),
            selecting: bank.select.on(),
            selected: bank.selected.iter().cloned().collect(),
            effects: bank
                .effects
                .iter()
                .map(|(id, effect)| {
                    let played = Played {
                        groups: effect.groups.clone(),
                        below: effect.blend == BlendMode::Below,
                    };
                    (id.clone(), played)
                })
                .collect(),
            base,
            surface: bank.surface(now),
            installs: bank.installs,
        }
    }

    /// Bumped whenever an effect's groups change: the engine compares it
    /// at each frame.
    pub(crate) fn changed(&self) -> u64 {
        self.read().changed
    }

    /// Where pattern `id` plays, sorted (none is its own selection);
    /// `None` for a pattern the bank does not hold.
    pub(crate) fn groups(&self, id: &str) -> Option<Vec<String>> {
        Some(self.read().effects.get(id)?.groups.clone())
    }

    /// Store the blends Select gives effects through `restyle`.
    pub(crate) fn restyle_to(
        &self,
        restyle: tokio::sync::mpsc::UnboundedSender<(String, BlendMode)>,
    ) {
        self.write().restyle = Some(restyle);
    }

    /// Whether pattern `id` plays on `groups` now.
    pub(crate) fn plays_on(&self, id: &str, groups: &[String]) -> bool {
        self.read()
            .effects
            .get(id)
            .is_some_and(|effect| effect.groups == groups)
    }

    /// Whether any light has a base color.
    pub(crate) fn has_base(&self) -> bool {
        !self.read().base.is_empty()
    }

    /// Give each light of `frame` its base color at `at`, and its dimmer
    /// none where it has no light yet: the base lights nothing.
    pub(crate) fn color_base(&self, frame: &mut UniverseState, at: Instant) {
        let bank = self.read();
        for (light, fading) in &bank.base {
            let rgb = fading.at(at).value(bank.bpm()).map(|c| c as f32);
            match frame.primitives.get_mut(light) {
                Some(head) => head.color = rgb,
                None => {
                    let head = crate::eval::composite::nothing();
                    frame.primitives.insert(
                        light.clone(),
                        crate::models::universe::PrimitiveState { color: rgb, ..head },
                    );
                }
            }
        }
    }

    /// Read the set's clock at `at`: `deck` is the followed deck's beat
    /// and the beats in its bar, `None` on the tapped tempo. Then move each
    /// stepping list on. The engine calls it as it renders the set.
    pub(crate) fn tick(&self, deck: Option<(f64, f64)>, at: Instant) {
        let mut bank = self.write();
        if let (Some(followed), Some((beat, beats_per_bar))) = (bank.deck.as_mut(), deck) {
            if beat.is_finite() && beats_per_bar > 0.0 {
                followed.read = Some(Reading {
                    beat,
                    at,
                    beats_per_bar,
                });
            }
        }
        bank.step(at);
    }

    /// Make `patterns` the venue's Perform set, laid out by `layout`, with
    /// `lights` each group's lights, [`ALL`] among them: declare each button
    /// node of each pattern's graph, start each level at 1, link each fader
    /// cell to its targets, give each list its values, know each group
    /// cell's group, and make each one-at-a-time row's pads take from each
    /// other. What already plays holds, so recompiling the set changes
    /// nothing that is playing: where each effect plays, what is chosen and
    /// each list's value on.
    pub(crate) fn install(
        &self,
        patterns: &[PerformPattern],
        layout: &PerformLayout,
        lights: HashMap<String, Arc<[String]>>,
    ) {
        let pads: HashMap<&str, String> = patterns
            .iter()
            .filter_map(|pattern| Some((pattern.id.as_str(), perform::pad(pattern)?)))
            .collect();
        let mut rows: BTreeMap<(&str, u16), Vec<String>> = BTreeMap::new();
        let mut links = HashMap::new();
        let mut lists = HashMap::new();
        let mut groups = HashMap::new();
        for page in &layout.pages {
            for cell in &page.cells {
                match &cell.content {
                    CellContent::Pattern { pattern_id }
                        if page.one_at_a_time.contains(&cell.row) =>
                    {
                        if let Some(pad) = pads.get(pattern_id.as_str()) {
                            rows.entry((page.id.as_str(), cell.row))
                                .or_default()
                                .push(pad.clone());
                        }
                    }
                    CellContent::Fader { targets, .. } => {
                        links.insert(
                            perform::cell(&cell.id),
                            targets.iter().map(perform::target).collect(),
                        );
                    }
                    CellContent::List {
                        values,
                        fade,
                        unit,
                        targets,
                        step,
                        ..
                    } => {
                        lists.insert(
                            cell.id.clone(),
                            List {
                                values: values.as_slice().into(),
                                fade: *fade,
                                unit: *unit,
                                targets: targets.iter().map(perform::target).collect(),
                                step: *step,
                                on: None,
                                counted_from: None,
                            },
                        );
                    }
                    CellContent::Group { group, .. } => {
                        groups.insert(perform::cell(&cell.id), group.clone());
                    }
                    _ => {}
                }
            }
        }
        let declared = patterns.iter().flat_map(|pattern| {
            pattern
                .graph
                .live_inputs()
                .into_iter()
                .filter_map(|input| match input.kind {
                    LiveInputKind::Button(press) => {
                        Some((perform::node(&pattern.id, &input.node), press))
                    }
                    LiveInputKind::Fader | LiveInputKind::Color => None,
                })
        });
        let mut bank = self.write();
        let mut buttons = HashMap::new();
        for (name, press) in declared {
            let (gate, turns_on) = bank
                .buttons
                .get(&name)
                .map(|button| (button.gate, button.turns_on.clone()))
                .unwrap_or_default();
            buttons.insert(
                name,
                Held {
                    press,
                    gate,
                    turns_on,
                },
            );
        }
        bank.buttons = buttons;
        bank.pads = patterns
            .iter()
            .filter_map(|pattern| {
                Some((pads.get(pattern.id.as_str())?.clone(), pattern.id.clone()))
            })
            .collect();
        let effects = patterns
            .iter()
            .map(|pattern| {
                let was = bank.effects.get(&pattern.id);
                let color = pattern
                    .graph
                    .live_inputs()
                    .into_iter()
                    .find(|input| input.kind == LiveInputKind::Color)
                    .map(|input| perform::node(&pattern.id, &input.node));
                let effect = Effect {
                    groups: was
                        .filter(|_| pads.contains_key(pattern.id.as_str()))
                        .map(|was| was.groups.clone())
                        .unwrap_or_default(),
                    color,
                    blend: pattern.blend_mode,
                    selection: pattern.selection.key(),
                };
                (pattern.id.clone(), effect)
            })
            .collect();
        bank.effects = effects;
        bank.changed += 1;
        bank.installs += 1;
        for pattern in patterns {
            bank.values
                .entry(perform::level(&pattern.id))
                .or_insert(1.0);
        }
        bank.links = links;
        for (id, list) in &mut lists {
            if let Some(was) = bank.lists.get(id) {
                list.on = was.on.filter(|on| *on < list.values.len());
                list.counted_from = was.counted_from;
            }
        }
        bank.taps = lists
            .iter()
            .flat_map(|(id, list)| {
                (0..list.values.len())
                    .map(move |index| (perform::value(id, index), (id.clone(), Some(index))))
                    .chain([(perform::cell(id), (id.clone(), None))])
            })
            .collect();
        bank.lists = lists;
        bank.faces = surface::faces(layout, patterns);
        let known: BTreeSet<&String> = groups.values().collect();
        let chosen = std::mem::take(&mut bank.chosen);
        bank.chosen = chosen.into_iter().filter(|g| known.contains(g)).collect();
        bank.held.retain(|control, _| groups.contains_key(control));
        bank.groups = groups;
        bank.lights = lights;
        let selected = std::mem::take(&mut bank.selected);
        bank.selected = selected
            .into_iter()
            .filter(|id| bank.effects.contains_key(id))
            .collect();
        bank.exclusive = rows
            .into_values()
            .flat_map(|row| {
                let row: Arc<[String]> = row.into();
                row.iter()
                    .map(|pad| (pad.clone(), row.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
    }

    /// The tapped tempo as of `at`.
    pub fn tempo(&self, at: Instant) -> Tempo {
        let bank = self.read();
        Tempo {
            bpm: bank.tempo.bpm,
            beat: bank.tempo.beat_at(at),
        }
    }

    /// Set the tapped tempo outright, keeping the beat where it is.
    pub fn set_bpm(&self, bpm: f64) {
        self.write().tempo.set_bpm(bpm, Instant::now());
    }

    /// Count beats at the tempo of the deck the set follows, on its beat
    /// grid as [`Self::tick`] reads it, or with `None`, on the tapped tempo
    /// again.
    pub(crate) fn follow(&self, deck_bpm: Option<f64>) {
        let mut bank = self.write();
        let read = bank.deck.and_then(|deck| deck.read);
        bank.deck = deck_bpm
            .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
            .map(|bpm| Deck { bpm, read });
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Bank> {
        self.bank.read().expect("controls poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Bank> {
        self.bank.write().expect("controls poisoned")
    }
}

/// Everything a bank plays as it stood when read, for the Perform screen.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playing {
    /// Every control set so far, by name. A button is not set but played.
    pub controls: BTreeMap<String, f64>,
    /// Every declared button, for a reader to play its envelope on at the
    /// set's tempo.
    pub buttons: BTreeMap<String, Button>,
    /// Every live color set so far, to play its fade on likewise.
    pub colors: BTreeMap<String, LiveColor>,
    /// Each list cell's value on, by cell id; a list none is on is absent.
    pub lists: BTreeMap<String, usize>,
    /// The groups chosen, and those held down, sorted.
    pub chosen: Vec<String>,
    pub held: Vec<String>,
    /// Whether Select is down or latched, and the effects it chose.
    pub selecting: bool,
    pub selected: Vec<String>,
    /// Every pattern, by id: where it plays, and whether it takes the color
    /// below.
    pub effects: BTreeMap<String, Played>,
    /// Each group's base color now, the mean of its lights'; a group none
    /// of whose lights has one is absent.
    pub base: BTreeMap<String, [f64; 3]>,
    /// How every control on the surface looks: what the screen and the
    /// controllers draw.
    pub surface: Surface,
    /// How many sets the bank has had installed. It moves whenever the set
    /// as stored may have changed, Select's blends among them, so a reader
    /// that keeps a copy reads it again.
    pub installs: u64,
}

/// One pattern as the bank plays it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Played {
    /// The groups it plays on, sorted; none is its own selection.
    pub groups: Vec<String>,
    /// Whether it takes the color below rather than its own.
    pub below: bool,
}

/// A score reads the venue's controls by name: a fader node its value, a
/// button node too, as a plain gate, and a live color its color. A button's
/// own envelope is the Perform set's alone, where it is declared.
impl luma_patterns::ControlSource for VenueControls {
    fn value(&self, input: &LiveInput) -> f64 {
        VenueControls::value(self, &input.name)
    }

    fn color(&self, input: &LiveInput) -> [f64; 3] {
        VenueControls::color(self, &input.name)
    }

    /// In a score nobody presses: a button there is a plain gate.
    fn presses(&self, _: &LiveInput) -> Presses {
        Presses::default()
    }
}

/// What a Perform pattern's graph reads: each fader, live color and button
/// node is a value of its own pattern ([`perform::node`]), not a venue
/// control.
#[derive(Debug)]
pub(crate) struct PatternControls {
    bank: Arc<VenueControls>,
    pattern_id: String,
}

impl PatternControls {
    pub(crate) fn new(bank: Arc<VenueControls>, pattern_id: &str) -> Self {
        Self {
            bank,
            pattern_id: pattern_id.to_owned(),
        }
    }
}

impl luma_patterns::ControlSource for PatternControls {
    fn value(&self, input: &LiveInput) -> f64 {
        self.bank
            .value(&perform::node(&self.pattern_id, &input.node))
    }

    fn color(&self, input: &LiveInput) -> [f64; 3] {
        self.bank
            .color(&perform::node(&self.pattern_id, &input.node))
    }

    fn presses(&self, input: &LiveInput) -> Presses {
        self.bank
            .presses(&perform::node(&self.pattern_id, &input.node))
    }
}

/// Tempo from taps: the beat runs on at `bpm` from `anchor`, where it was
/// `anchor_beat`. The first tap of a run lands on the nearest bar's first
/// beat, so it says where the bar starts; each later tap lands on the
/// nearest beat.
#[derive(Debug)]
struct TapTempo {
    bpm: f64,
    anchor: Instant,
    anchor_beat: f64,
    /// The taps of the current run, oldest first.
    taps: VecDeque<Instant>,
}

/// A gap longer than this starts a new run of taps.
const TAP_RUN_GAP: Duration = Duration::from_secs(2);
/// How many taps of a run the tempo averages.
const TAPS_AVERAGED: usize = 8;
const BPM_RANGE: (f64, f64) = (30.0, 300.0);
/// The tapped tempo before any tap.
pub const UNTAPPED_BPM: f64 = 120.0;
/// The bar the tapped tempo counts.
pub const TAPPED_BEATS_PER_BAR: f64 = 4.0;

impl Default for TapTempo {
    fn default() -> Self {
        Self {
            bpm: UNTAPPED_BPM,
            anchor: Instant::now(),
            anchor_beat: 0.0,
            taps: VecDeque::new(),
        }
    }
}

impl TapTempo {
    /// The beat at `at`, before the anchor too: a press made before the
    /// last tap keeps its age.
    fn beat_at(&self, at: Instant) -> f64 {
        self.anchor_beat + seconds(self.anchor, at) * self.bpm / 60.0
    }

    fn tap(&mut self, at: Instant) {
        if self
            .taps
            .back()
            .is_some_and(|last| at.saturating_duration_since(*last) > TAP_RUN_GAP)
        {
            self.taps.clear();
        }
        let grid = if self.taps.is_empty() {
            TAPPED_BEATS_PER_BAR
        } else {
            1.0
        };
        let beat = (self.beat_at(at) / grid).round() * grid;
        self.taps.push_back(at);
        while self.taps.len() > TAPS_AVERAGED {
            self.taps.pop_front();
        }
        if let (Some(first), Some(last)) = (self.taps.front(), self.taps.back()) {
            let span = last.saturating_duration_since(*first).as_secs_f64();
            if self.taps.len() >= 2 && span > 0.0 {
                let bpm = 60.0 * (self.taps.len() - 1) as f64 / span;
                self.bpm = bpm.clamp(BPM_RANGE.0, BPM_RANGE.1);
            }
        }
        self.anchor = at;
        self.anchor_beat = beat;
    }

    fn set_bpm(&mut self, bpm: f64, at: Instant) {
        if !bpm.is_finite() {
            return;
        }
        self.anchor_beat = self.beat_at(at);
        self.anchor = at;
        self.bpm = bpm.clamp(BPM_RANGE.0, BPM_RANGE.1);
        self.taps.clear();
    }
}

/// Every venue's bank, one set per app: the engine, the inputs, the
/// Perform session and the commands all reach a venue's bank here, so two
/// apps in one process never play each other's.
#[derive(Debug, Default)]
pub struct ControlBanks {
    venues: Mutex<HashMap<String, Arc<VenueControls>>>,
}

impl ControlBanks {
    /// The bank of `venue_id`, made on first use.
    pub fn venue(&self, venue_id: &str) -> Arc<VenueControls> {
        self.lock().entry(venue_id.to_owned()).or_default().clone()
    }

    /// Forget every venue's values: another person signed in.
    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<VenueControls>>> {
        self.venues.lock().expect("controls poisoned")
    }
}

/// One control as the Perform screen lists it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    pub name: String,
    pub value: f64,
}

/// The venue's own controls: every one a score's pattern of the venue names,
/// and every other one set, by name. The Perform set's own ([`perform`]),
/// its patterns' fader and button nodes among them, are not listed.
pub(crate) async fn list(
    access: &mut impl AuthorizedVenue,
    banks: &ControlBanks,
) -> Result<Vec<Control>, String> {
    let venue_id = access.venue_id().to_owned();
    let graphs: Vec<String> = sqlx::query_scalar(
        "SELECT clip.graph_json FROM clips clip
         JOIN scores score ON score.id = clip.score_id
         WHERE score.venue_id = ?",
    )
    .bind(&venue_id)
    .fetch_all(access.connection())
    .await
    .map_err(|error| format!("Failed to read the venue's patterns: {error}"))?;
    let mut names: BTreeSet<String> = graphs
        .iter()
        .filter_map(|json| luma_patterns::ClipGraph::from_json(json).ok())
        .flat_map(|graph| {
            graph
                .controls()
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();
    let values = banks.venue(&venue_id).playing().controls;
    names.extend(values.keys().cloned());
    Ok(names
        .into_iter()
        .filter(|name| !perform::owns(name))
        .map(|name| Control {
            value: values.get(&name).copied().unwrap_or(0.0),
            name,
        })
        .collect())
}

mod surface;

#[cfg(test)]
pub(crate) mod tests;
