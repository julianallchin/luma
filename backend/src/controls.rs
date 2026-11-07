//! A venue's live controls: named values 0–1 that faders, pads, MIDI, OSC
//! and any other input set while the lights play.
//!
//! A control exists by being named. A score's pattern that reads `drop` (a
//! fader or button node named `drop`) makes `drop` a control of its venue,
//! and it reads 0 until something sets it.
//!
//! The Perform set plays through controls too, under names of its own
//! ([`perform`]): each pattern's level, each fader, live color and button
//! node of its graph (its pad among them), each fader cell, each color
//! cell's swatches, each target, the Perform master, and tap tempo. So everything that
//! can move a control (an on-screen pad, a MIDI key, an OSC message) can
//! play the set, and the bank is the one place that enforces how they play.
//!
//! A control is a fader, a button or a live color. A fader holds the value
//! it was set to. A button is declared with the [`Press`] that plays it
//! ([`install`]): the bank keeps its gate (when it went on and off) and its
//! value is its envelope through that gate, now. A press flips a toggle and
//! holds a flash; setting a button outright gates it on above ½ and off
//! below. A name pressed that no button declares is momentary: 1 while
//! held. Turning a pad on in a one-at-a-time row releases the row's others,
//! a fader cell moves every one of its targets, and a tap lands on the
//! venue's tempo.
//!
//! A button whose press starts on the beat or the bar ([`Start`]) gates on
//! or off at the set's next one: the bank keeps that time, still to come, as
//! the gate's, so its value stays a pure function of time. The opposite
//! change before then calls it off.
//!
//! A pattern can play on venue groups instead of its own selection: its pad
//! pressed while target cells are held plays an [`Instance`] of it on their
//! groups. Each instance is a button of its own, played by its pattern's
//! pad's press: pressed again with the same targets held, it toggles, and a
//! one-at-a-time row releases only the row's instances on the same targets.
//! Everything else of the pattern (its level, faders and colors) is the
//! pattern's, in every instance.
//!
//! A live color is set by a color cell: pressing one of its swatches fades
//! every target of the cell from the color it has now to the swatch's. The
//! bank keeps only that fade (from, to, when and how long), so a color too
//! is a pure function of time. A color cell that steps moves on to its next
//! swatch every so many bars of the set's clock.
//!
//! The set's clock is the bank's own tapped tempo, its bars starting at a
//! run's first tap, or, while the set follows a deck, the deck's beat grid
//! as the engine reads it at each frame ([`VenueControls::tick`]). Color
//! cells step on it and quantised presses land on it.
//!
//! Values are live state, like the render engine's, so they live here
//! rather than in the database: every scene an app compiles for a venue
//! reads that app's one bank of the venue ([`ControlBanks`]), and setting a
//! value never recompiles.
//!
//! [`install`]: VenueControls::install

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use luma_patterns::clip_graph::{LiveInput, LiveInputKind};
use luma_patterns::press::{Button, Gate, Press, PressMode, Shape, Start, TimeUnit};
use serde::Serialize;

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::perform::{CellContent, PerformLayout, PerformPattern, Step, Tempo};

/// The names the Perform set plays under, all `perform_…`. Nothing lists
/// them as the venue's own.
pub mod perform {
    use crate::models::perform::{ControlTarget, PerformPattern};

    /// Scales every Perform pattern, 1 until moved.
    pub const MASTER: &str = "perform_master";
    /// Pressed on each beat to tap the tempo.
    pub const TAP: &str = "perform_tap";

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
    /// targets, or a target's, 1 while it is held.
    pub fn cell(id: &str) -> String {
        format!("perform_c{}", key(id))
    }

    /// Swatch `index` of color cell `id`, from 0: pressed, it is chosen.
    pub fn swatch(id: &str, index: usize) -> String {
        format!("perform_c{}_s{index}", key(id))
    }

    /// The button of pad `pad` playing on groups `on`, sorted: the pad
    /// itself when `on` is empty. It is no control name, so nothing binds
    /// to an instance; hardware plays it through the pad and the targets.
    pub fn instance(pad: &str, on: &[String]) -> String {
        if on.is_empty() {
            pad.to_owned()
        } else {
            format!("{pad}@{}", on.join("+"))
        }
    }

    /// The control a fader or color cell's `target` moves.
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

/// One venue's control values, its buttons and its tempo.
#[derive(Debug)]
pub struct VenueControls {
    bank: RwLock<Bank>,
}

#[derive(Debug)]
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
    /// Each color cell, by cell id.
    palettes: HashMap<String, Palette>,
    /// Each swatch's control, to its color cell and its place there.
    swatches: HashMap<String, (String, usize)>,
    /// Each pad, to its pattern's id.
    pads: HashMap<String, String>,
    /// Each target cell's control, to its group.
    targets: HashMap<String, String>,
    /// Each instance's button, to its pad and what it is.
    instances: BTreeMap<String, (String, Instance)>,
    /// Each button held down, to the button its press went to: a pad's
    /// instance, so its release lets go of the same one.
    pressed: HashMap<String, String>,
    /// How many instances were ever made, for the engine to see a new one.
    made: u64,
    tempo: TapTempo,
    /// The deck the set follows, while it follows one.
    deck: Option<Deck>,
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

/// A pattern playing on venue groups instead of its own selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    pub pattern_id: String,
    /// The groups it plays on, by name, sorted.
    pub on: Vec<String>,
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

/// A live color's last fade and when it began.
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

/// A color cell as the bank plays it: its swatches, fade, targets and
/// step, from the layout, and the swatch on, which is live.
#[derive(Clone, Debug)]
struct Palette {
    swatches: Arc<[[f64; 3]]>,
    fade: f64,
    unit: TimeUnit,
    targets: Arc<[String]>,
    step: Option<Step>,
    /// The swatch on, `None` until one is chosen.
    on: Option<usize>,
    /// The bar of the set's clock the swatch on is counted from, once the
    /// clock has been read since it was chosen.
    counted_from: Option<i64>,
}

/// A declared button: how it plays, and when it goes on and off, either
/// still to come while a press waits for its beat; `None` until first
/// pressed.
#[derive(Clone, Copy, Debug)]
struct Held {
    press: Press,
    gate: Option<(Instant, Option<Instant>)>,
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
                buttons: HashMap::new(),
                exclusive: HashMap::new(),
                links: HashMap::new(),
                colors: HashMap::new(),
                palettes: HashMap::new(),
                swatches: HashMap::new(),
                pads: HashMap::new(),
                targets: HashMap::new(),
                instances: BTreeMap::new(),
                pressed: HashMap::new(),
                made: 0,
                tempo: TapTempo::default(),
                deck: None,
            }),
        }
    }
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

    /// Set `name` and, for a fader cell, its targets. A target is set as
    /// itself, never through its own links, so no two cells can loop.
    fn set(&mut self, name: &str, value: f64, at: Instant) {
        let targets = self.links.get(name).cloned();
        self.set_one(name, value, at);
        for target in targets.iter().flat_map(|targets| targets.iter()) {
            self.set_one(target, value, at);
        }
    }

    fn set_one(&mut self, name: &str, value: f64, at: Instant) {
        if self.buttons.contains_key(name) {
            let button = self.aim(name, None, value > 0.5);
            self.gate(&button, value > 0.5, at);
            return;
        }
        let was = self.values.insert(name.to_owned(), value).unwrap_or(0.0);
        if value <= 0.5 || was > 0.5 {
            return;
        }
        if name == perform::TAP {
            self.tempo.tap(at);
        }
        if let Some((cell, index)) = self.swatches.get(name).cloned() {
            self.choose(&cell, index, None, at);
        }
    }

    /// Put color cell `id` on swatch `index`, counted from bar
    /// `counted_from` (`None`: the next the clock reads), and fade each of
    /// its targets from where it is to the swatch.
    fn choose(&mut self, id: &str, index: usize, counted_from: Option<i64>, at: Instant) {
        let Some(palette) = self.palettes.get_mut(id) else {
            return;
        };
        let Some(&to) = palette.swatches.get(index) else {
            return;
        };
        palette.on = Some(index);
        palette.counted_from = counted_from;
        let (targets, length, unit) = (palette.targets.clone(), palette.fade, palette.unit);
        for target in targets.iter() {
            let from = self.color(target, at);
            self.colors.insert(
                target.clone(),
                Fading {
                    fade: Fade {
                        from,
                        to,
                        length,
                        unit,
                    },
                    start: at,
                },
            );
        }
    }

    /// Move each stepping color cell on whose bars are up at `at`. A
    /// cell's count starts at the first bar read after its swatch was
    /// chosen, and again when the clock goes back past it.
    fn step(&mut self, at: Instant) {
        if !self.palettes.values().any(|palette| palette.step.is_some()) {
            return;
        }
        let (beat, beats_per_bar) = self.beat(at);
        let bar = (beat / beats_per_bar).floor() as i64;
        let mut due = Vec::new();
        for (id, palette) in &mut self.palettes {
            let (Some(step), Some(on)) = (palette.step, palette.on) else {
                continue;
            };
            match palette.counted_from {
                Some(from) if (from..from + i64::from(step.bars)).contains(&bar) => {}
                Some(from) if bar > from => {
                    palette.counted_from = Some(bar);
                    let next = if on + 1 < palette.swatches.len() {
                        Some(on + 1)
                    } else {
                        step.loops.then_some(0)
                    };
                    due.extend(next.map(|next| (id.clone(), next)));
                }
                _ => palette.counted_from = Some(bar),
            }
        }
        for (id, index) in due {
            self.choose(&id, index, Some(bar), at);
        }
    }

    fn color(&self, name: &str, at: Instant) -> [f64; 3] {
        self.colors
            .get(name)
            .map_or(UNSET_COLOR, |fading| fading.at(at).value(self.bpm()))
    }

    /// The groups of the target cells held now, sorted.
    fn held(&self) -> Vec<String> {
        let mut on: Vec<String> = self
            .targets
            .iter()
            .filter(|(control, _)| self.values.get(*control).is_some_and(|value| *value > 0.5))
            .map(|(_, group)| group.clone())
            .collect();
        on.sort();
        on.dedup();
        on
    }

    /// The button that playing button `name` plays: for a pad, its instance
    /// on groups `on` (the targets held when `None`), made when it is to
    /// `make` and not made yet; the pad itself on no groups; any other button
    /// itself.
    fn aim(&mut self, name: &str, on: Option<&[String]>, make: bool) -> String {
        let Some(pattern_id) = self.pads.get(name).cloned() else {
            return name.to_owned();
        };
        let on = match on {
            Some(on) => {
                let mut on = on.to_vec();
                on.sort();
                on.dedup();
                on
            }
            None => self.held(),
        };
        let button = perform::instance(name, &on);
        if make && !on.is_empty() && !self.instances.contains_key(&button) {
            let press = self.buttons[name].press;
            self.buttons
                .insert(button.clone(), Held { press, gate: None });
            self.instances.insert(
                button.clone(),
                (name.to_owned(), Instance { pattern_id, on }),
            );
            self.made += 1;
        }
        button
    }

    /// Press (`down`) or release declared button `name` at `at`: a press
    /// goes to the button [`Self::aim`] picks, its release to the same one.
    fn press(&mut self, name: &str, down: bool, at: Instant) {
        let button = if down {
            let button = self.aim(name, None, true);
            self.pressed.insert(name.to_owned(), button.clone());
            button
        } else {
            self.pressed.remove(name).unwrap_or_else(|| name.to_owned())
        };
        let Some(&held) = self.buttons.get(&button) else {
            return;
        };
        match held.press.mode {
            PressMode::Toggle if down => self.gate(&button, !held.on(), at),
            PressMode::Toggle => {}
            PressMode::Flash => self.gate(&button, down, at),
        }
    }

    /// Gate declared button `name` on or off, asked for at `at`: at once,
    /// or on the set's next beat or bar, as its press starts. One on or
    /// going on stays so, and one off or going off. A change still to come
    /// is called off by the opposite one, but a flash let go before its beat
    /// stays on for one, so a tap lands. Gating a pad or an instance on
    /// releases its row's others on the same groups, when it goes on.
    fn gate(&mut self, name: &str, on: bool, at: Instant) {
        let Some(&Held { press, gate }) = self.buttons.get(name) else {
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
        let from = gate.0.max(at);
        if let Some(button) = self.buttons.get_mut(name) {
            button.gate = Some(gate);
        }
        if !on {
            return;
        }
        let (pad, groups) = match self.instances.get(name) {
            Some((pad, instance)) => (pad.clone(), instance.on.clone()),
            None => (name.to_owned(), Vec::new()),
        };
        if let Some(row) = self.exclusive.get(&pad).cloned() {
            for other in row.iter().filter(|other| **other != pad) {
                self.release(&perform::instance(other, &groups), at, from);
            }
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
            Some((since, _)) if since > at => None,
            Some((since, off)) if off.is_none_or(|off| off > when) => Some((since, Some(when))),
            gate => gate,
        };
    }

    fn value(&self, name: &str, at: Instant) -> f64 {
        match self.buttons.get(name) {
            Some(button) => button.at(at).value(self.bpm()),
            None => self.values.get(name).copied().unwrap_or(0.0),
        }
    }
}

impl VenueControls {
    /// Set `name` to `value`, clamped to 0–1. A button gates on above ½ and
    /// off below, whatever its mode; a fader cell sets its targets too;
    /// pressing [`perform::TAP`] taps the tempo.
    pub fn set(&self, name: &str, value: f64) {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.write().set(name, value, Instant::now());
    }

    /// Press (`down`) or release button `name` at `at`. A toggle flips on
    /// the press and ignores the release; a flash is on while held. A name
    /// no button declares is momentary: 1 while held.
    pub fn press(&self, name: &str, down: bool, at: Instant) {
        let mut bank = self.write();
        if bank.buttons.contains_key(name) {
            bank.press(name, down, at);
        } else {
            bank.set(name, f64::from(u8::from(down)), at);
        }
    }

    /// Set pad `name` on (above ½) or off, playing on groups `on` whatever
    /// targets are held: the pad itself on none, else its instance there.
    /// Any other control is set as [`Self::set`] sets it.
    pub fn set_on(&self, name: &str, value: f64, on: &[String]) {
        let mut bank = self.write();
        let button = bank.aim(name, Some(on), value > 0.5);
        if bank.buttons.contains_key(&button) {
            bank.gate(&button, value > 0.5, Instant::now());
        } else {
            drop(bank);
            self.set(name, value);
        }
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

    /// Everything the bank plays, as it stands now.
    pub fn playing(&self) -> Playing {
        let now = Instant::now();
        let bank = self.read();
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
            swatches: bank
                .palettes
                .iter()
                .filter_map(|(id, palette)| Some((id.clone(), palette.on?)))
                .collect(),
            instances: bank
                .instances
                .iter()
                .map(|(name, (_, instance))| (name.clone(), instance.clone()))
                .collect(),
        }
    }

    /// Every instance as it stands, by its button, and how many were ever
    /// made: a count the engine compares to see a new one.
    pub(crate) fn instances(&self) -> (u64, Vec<(String, Instance)>) {
        let bank = self.read();
        let instances = bank
            .instances
            .iter()
            .map(|(name, (_, instance))| (name.clone(), instance.clone()))
            .collect();
        (bank.made, instances)
    }

    /// How many instances were ever made.
    pub(crate) fn made(&self) -> u64 {
        self.read().made
    }

    /// Read the set's clock at `at`: `deck` is the followed deck's beat
    /// and the beats in its bar, `None` on the tapped tempo. Then move each
    /// stepping color cell on. The engine calls it as it renders the set.
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

    /// Make `patterns` the venue's Perform set, laid out by `layout`: declare
    /// each button node of each pattern's graph, start each level at 1, link
    /// each fader cell to its targets, give each color cell its swatches,
    /// know each target cell's group, and make each one-at-a-time row's pads
    /// release each other. What already plays holds, so recompiling the set
    /// changes nothing that is playing; an instance that has gone quiet is
    /// forgotten, to be made again when next played.
    pub(crate) fn install(&self, patterns: &[PerformPattern], layout: &PerformLayout) {
        let pads: HashMap<&str, String> = patterns
            .iter()
            .filter_map(|pattern| Some((pattern.id.as_str(), perform::pad(pattern)?)))
            .collect();
        let mut rows: BTreeMap<(&str, u16), Vec<String>> = BTreeMap::new();
        let mut links = HashMap::new();
        let mut palettes = HashMap::new();
        let mut targets = HashMap::new();
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
                    CellContent::Color {
                        swatches,
                        fade,
                        unit,
                        targets,
                        step,
                        ..
                    } => {
                        palettes.insert(
                            cell.id.clone(),
                            Palette {
                                swatches: swatches.as_slice().into(),
                                fade: *fade,
                                unit: *unit,
                                targets: targets.iter().map(perform::target).collect(),
                                step: *step,
                                on: None,
                                counted_from: None,
                            },
                        );
                    }
                    CellContent::Target { group, .. } => {
                        targets.insert(perform::cell(&cell.id), group.clone());
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
        let now = Instant::now();
        let mut bank = self.write();
        let mut buttons = HashMap::new();
        for (name, press) in declared {
            let gate = bank.buttons.get(&name).and_then(|button| button.gate);
            buttons.insert(name, Held { press, gate });
        }
        let bpm = bank.bpm();
        let instances = std::mem::take(&mut bank.instances);
        for (name, (pad, instance)) in instances {
            let (Some(pad_button), Some(held)) = (buttons.get(&pad), bank.buttons.get(&name))
            else {
                continue;
            };
            let held = Held {
                press: pad_button.press,
                gate: held.gate,
            };
            if held.on() || held.at(now).value(bpm) > 0.0 {
                buttons.insert(name.clone(), held);
                bank.instances.insert(name, (pad, instance));
            }
        }
        bank.buttons = buttons;
        bank.pads = patterns
            .iter()
            .filter_map(|pattern| {
                Some((pads.get(pattern.id.as_str())?.clone(), pattern.id.clone()))
            })
            .collect();
        bank.targets = targets;
        for pattern in patterns {
            bank.values
                .entry(perform::level(&pattern.id))
                .or_insert(1.0);
        }
        bank.links = links;
        for (id, palette) in &mut palettes {
            if let Some(was) = bank.palettes.get(id) {
                palette.on = was.on.filter(|on| *on < palette.swatches.len());
                palette.counted_from = was.counted_from;
            }
        }
        bank.swatches = palettes
            .iter()
            .flat_map(|(id, palette)| {
                (0..palette.swatches.len())
                    .map(move |index| (perform::swatch(id, index), (id.clone(), index)))
            })
            .collect();
        bank.palettes = palettes;
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
    /// Each color cell's swatch on, by cell id; a cell none is on is absent.
    pub swatches: BTreeMap<String, usize>,
    /// Every pattern playing, or played, on venue groups, by its button,
    /// which is among `buttons`.
    pub instances: BTreeMap<String, Instance>,
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
}

/// What a Perform pattern's graph reads: each fader, live color and button
/// node is a value of its own pattern ([`perform::node`]), not a venue
/// control. An instance's pad node reads the instance's button instead.
#[derive(Debug)]
pub(crate) struct PatternControls {
    bank: Arc<VenueControls>,
    pattern_id: String,
    /// An instance's pad node, and the button it reads.
    pad: Option<(String, String)>,
}

impl PatternControls {
    pub(crate) fn new(bank: Arc<VenueControls>, pattern_id: &str) -> Self {
        Self {
            bank,
            pattern_id: pattern_id.to_owned(),
            pad: None,
        }
    }

    /// Read pad node `node` as `button`, an instance's.
    pub(crate) fn with_pad(self, node: &str, button: &str) -> Self {
        Self {
            pad: Some((node.to_owned(), button.to_owned())),
            ..self
        }
    }
}

impl luma_patterns::ControlSource for PatternControls {
    fn value(&self, input: &LiveInput) -> f64 {
        match &self.pad {
            Some((node, button)) if *node == input.node => self.bank.value(button),
            _ => self
                .bank
                .value(&perform::node(&self.pattern_id, &input.node)),
        }
    }

    fn color(&self, input: &LiveInput) -> [f64; 3] {
        self.bank
            .color(&perform::node(&self.pattern_id, &input.node))
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
    fn beat_at(&self, at: Instant) -> f64 {
        let elapsed = at.saturating_duration_since(self.anchor).as_secs_f64();
        self.anchor_beat + elapsed * self.bpm / 60.0
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::models::perform::{Cell, ControlTarget, LayoutPage};
    use luma_patterns::ClipGraph;

    /// `graph` with a pad ([`ClipGraph::with_pad`]) that plays at once, not
    /// on the next beat, for a test that presses it and looks straight away.
    pub(crate) fn with_pad_at_once(graph: ClipGraph) -> ClipGraph {
        let mut graph = graph.with_pad();
        if let Some(pad) = graph.pad() {
            if let Some(node) = graph.nodes.get_mut(&pad.node) {
                node.settings.insert("start".into(), "now".into());
            }
        }
        graph
    }

    /// A pattern of `id` with no pad.
    fn pattern(id: &str) -> PerformPattern {
        PerformPattern {
            id: id.into(),
            venue_id: "venue".into(),
            name: id.into(),
            selection: luma_patterns::Selection {
                expression: "all".into(),
            },
            z_index: 0,
            blend_mode: luma_patterns::BlendMode::Replace,
            seed: 1,
            graph: luma_patterns::ClipGraph::default(),
        }
    }

    /// A pattern whose pad plays as `mode` ("toggle" or "flash"), at once,
    /// fading in over `attack` and out over `release`, linearly, in `unit`.
    fn faded(id: &str, mode: &str, attack: f64, release: f64, unit: &str) -> PerformPattern {
        let mut graph = with_pad_at_once(
            ClipGraph::from_json(r#"{"version": 3, "nodes": {"color1": {"kind": "color"}}}"#)
                .unwrap(),
        );
        let node = graph.pad().unwrap().node;
        let settings = &mut graph.nodes.get_mut(&node).unwrap().settings;
        for (name, value) in [
            ("mode", mode.to_owned()),
            ("attack", attack.to_string()),
            ("release", release.to_string()),
            ("unit", unit.to_owned()),
            ("shape", "linear".to_owned()),
        ] {
            settings.insert(name.into(), value);
        }
        PerformPattern {
            graph,
            ..pattern(id)
        }
    }

    fn pad(pattern: &PerformPattern) -> String {
        perform::pad(pattern).expect("a pad")
    }

    fn cell(id: &str, column: u16, row: u16, content: CellContent) -> Cell {
        Cell {
            id: id.into(),
            column,
            row,
            width: 1,
            height: 1,
            content,
        }
    }

    /// `rows` of pattern ids, the first row one at a time, and `extra`
    /// cells.
    fn layout(rows: &[&[&str]], extra: Vec<Cell>) -> PerformLayout {
        let mut cells: Vec<Cell> = rows
            .iter()
            .enumerate()
            .flat_map(|(row, ids)| {
                ids.iter().enumerate().map(move |(column, id)| {
                    cell(
                        &format!("cell-{id}"),
                        column as u16,
                        row as u16,
                        CellContent::Pattern {
                            pattern_id: (*id).into(),
                        },
                    )
                })
            })
            .collect();
        cells.extend(extra);
        PerformLayout {
            pages: vec![LayoutPage {
                id: "page".into(),
                name: "Main".into(),
                columns: 8,
                rows: 4,
                one_at_a_time: vec![0],
                cells,
            }],
        }
    }

    fn after(start: Instant, seconds: f64) -> Instant {
        start + Duration::from_secs_f64(seconds)
    }

    /// Whether button `name` is on, or going on, as of its last press.
    fn lit(controls: &VenueControls, name: &str) -> bool {
        controls.read().buttons[name].on()
    }

    #[test]
    fn a_pad_fades_in_and_out_by_its_envelope() {
        let controls = VenueControls::default();
        let wash = faded("wash", "toggle", 1.0, 2.0, "seconds");
        let on = pad(&wash);
        controls.install(&[wash], &PerformLayout::default());
        let start = Instant::now();
        assert_eq!(controls.value_at(&on, start), 0.0, "never pressed");
        controls.press(&on, true, start);
        controls.press(&on, false, after(start, 0.1));
        assert!(lit(&controls, &on), "a toggle holds after release");
        let rising = controls.value_at(&on, after(start, 0.5));
        assert!(0.0 < rising && rising < 1.0, "halfway up: {rising}");
        assert_eq!(controls.value_at(&on, after(start, 1.5)), 1.0);

        controls.press(&on, true, after(start, 2.0));
        assert!(!lit(&controls, &on), "the next press turns it off");
        let falling = controls.value_at(&on, after(start, 3.0));
        assert!(0.0 < falling && falling < 1.0, "releasing: {falling}");
        assert_eq!(controls.value_at(&on, after(start, 4.5)), 0.0);
    }

    #[test]
    fn a_flash_is_on_while_held() {
        let controls = VenueControls::default();
        let hit = faded("hit", "flash", 0.0, 0.0, "seconds");
        let on = pad(&hit);
        controls.install(&[hit], &PerformLayout::default());
        let start = Instant::now();
        controls.press(&on, true, start);
        assert_eq!(controls.value_at(&on, after(start, 0.5)), 1.0);
        controls.press(&on, false, after(start, 1.0));
        assert_eq!(controls.value_at(&on, after(start, 1.0)), 0.0);
        assert!(!lit(&controls, &on));
    }

    #[test]
    fn beats_follow_the_deck_the_set_follows() {
        let controls = VenueControls::default();
        let slow = faded("wash", "toggle", 4.0, 0.0, "beats");
        let on = pad(&slow);
        controls.install(&[slow], &PerformLayout::default());
        let start = Instant::now();
        controls.press(&on, true, start);
        // Four beats: two seconds at the tapped 120, one at a deck's 240.
        let tapped = controls.value_at(&on, after(start, 1.0));
        controls.follow(Some(240.0));
        let unread = controls.value_at(&on, after(start, 1.0));
        assert_eq!(unread, tapped, "a deck not read yet is no clock");
        controls.tick(Some((8.0, 4.0)), start);
        let followed = controls.value_at(&on, after(start, 1.0));
        assert!(tapped < followed, "{tapped} < {followed}");
        assert_eq!(followed, 1.0);
        controls.follow(None);
        assert_eq!(controls.value_at(&on, after(start, 1.0)), tapped);
    }

    #[test]
    fn a_followed_deck_not_read_yet_leaves_the_tapped_clock_whole() {
        let start = Instant::now();
        let wash = quantised("wash", "toggle", "beat");
        let on = pad(&wash);
        let controls = tapped(&[wash], &PerformLayout::default(), start);
        // No live set reads the deck: a press half a beat in waits for the
        // tapped tempo's next beat, counted at its tempo, not the deck's.
        controls.follow(Some(240.0));
        controls.press(&on, true, beat(start, 0.5));
        assert_eq!(state_at(&controls, &on, beat(start, 0.9)), (false, true));
        assert_eq!(state_at(&controls, &on, beat(start, 1.0)), (true, false));
    }

    #[test]
    fn a_one_at_a_time_row_releases_the_others() {
        let controls = VenueControls::default();
        let set = [
            faded("red", "toggle", 0.0, 1.0, "seconds"),
            faded("blue", "toggle", 0.0, 0.0, "seconds"),
            faded("chase", "toggle", 0.0, 0.0, "seconds"),
            pattern("plain"),
        ];
        let [red, blue, chase] = [&set[0], &set[1], &set[2]].map(pad);
        controls.install(
            &set,
            &layout(&[&["red", "blue", "plain"], &["chase"]], Vec::new()),
        );
        assert_eq!(
            controls.value(&perform::level("red")),
            1.0,
            "levels start full"
        );
        let start = Instant::now();
        let press = |name: &str, at| controls.press(name, true, at);
        press(&red, start);
        press(&chase, start);
        press(&blue, after(start, 1.0));
        assert!(!lit(&controls, &red), "blue released red");
        assert!(lit(&controls, &blue));
        assert!(lit(&controls, &chase), "in another row");
        let fading = controls.value_at(&red, after(start, 1.5));
        assert!(0.0 < fading && fading < 1.0, "red fades by its own release");
    }

    /// A target cell `id` on group `group`.
    fn aimed(id: &str, group: &str) -> Cell {
        cell(
            id,
            7,
            3,
            CellContent::Target {
                label: String::new(),
                group: group.into(),
            },
        )
    }

    /// Hold (`down`) or let go of target cell `id` at `at`.
    fn hold(controls: &VenueControls, id: &str, down: bool, at: Instant) {
        controls.press(&perform::cell(id), down, at);
    }

    fn on(group: &[&str]) -> Vec<String> {
        group.iter().map(|group| (*group).to_owned()).collect()
    }

    #[test]
    fn a_pad_pressed_on_targets_plays_an_instance_there_and_toggles_it() {
        let controls = VenueControls::default();
        let wash = faded("wash", "toggle", 0.0, 0.0, "seconds");
        let own = pad(&wash);
        controls.install(
            &[wash],
            &layout(&[], vec![aimed("front", "front"), aimed("back", "back")]),
        );
        let at = Instant::now();
        let front = perform::instance(&own, &on(&["front"]));
        hold(&controls, "front", true, at);
        controls.press(&own, true, at);
        controls.press(&own, false, at);
        assert!(lit(&controls, &front), "on front");
        assert!(!lit(&controls, &own), "not on its own selection");
        assert_eq!(
            controls.playing().instances[&front],
            Instance {
                pattern_id: "wash".into(),
                on: on(&["front"]),
            }
        );

        // Both held: one instance on both, their union.
        hold(&controls, "back", true, at);
        controls.press(&own, true, at);
        let both = perform::instance(&own, &on(&["back", "front"]));
        assert!(lit(&controls, &both));
        assert!(lit(&controls, &front), "front's is another instance");
        hold(&controls, "back", false, at);

        // Front held again: the same instance, toggled off.
        controls.press(&own, true, at);
        assert!(!lit(&controls, &front), "the same targets toggle it");
        hold(&controls, "front", false, at);
        controls.press(&own, true, at);
        assert!(lit(&controls, &own), "no target held: its own selection");
        assert!(lit(&controls, &both), "an instance plays on by itself");
    }

    #[test]
    fn a_flash_lets_go_of_the_instance_its_press_went_to() {
        let controls = VenueControls::default();
        let hit = faded("hit", "flash", 0.0, 0.0, "seconds");
        let own = pad(&hit);
        controls.install(&[hit], &layout(&[], vec![aimed("front", "front")]));
        let at = Instant::now();
        let front = perform::instance(&own, &on(&["front"]));
        hold(&controls, "front", true, at);
        controls.press(&own, true, at);
        hold(&controls, "front", false, at);
        assert!(lit(&controls, &front), "held");
        controls.press(&own, false, at);
        assert!(
            !lit(&controls, &front),
            "let go, though the target was first"
        );
        assert!(!lit(&controls, &own));
    }

    #[test]
    fn a_one_at_a_time_row_releases_the_others_on_the_same_targets() {
        let controls = VenueControls::default();
        let set = [
            faded("a", "toggle", 0.0, 0.0, "seconds"),
            faded("b", "toggle", 0.0, 0.0, "seconds"),
        ];
        let [a, b] = [&set[0], &set[1]].map(pad);
        controls.install(
            &set,
            &layout(
                &[&["a", "b"]],
                vec![aimed("front", "front"), aimed("back", "back")],
            ),
        );
        let at = Instant::now();
        let instance = |pad: &str, group: &str| perform::instance(pad, &on(&[group]));
        controls.press(&a, true, at);
        for group in ["front", "back"] {
            hold(&controls, group, true, at);
            controls.press(&a, true, at);
            hold(&controls, group, false, at);
        }
        hold(&controls, "front", true, at);
        controls.press(&b, true, at);
        hold(&controls, "front", false, at);
        assert!(lit(&controls, &instance(&b, "front")));
        assert!(
            !lit(&controls, &instance(&a, "front")),
            "b released a on front"
        );
        assert!(lit(&controls, &instance(&a, "back")), "on back a plays on");
        assert!(lit(&controls, &a), "and on its own selection");

        // Set outright, on groups named.
        controls.set_on(&b, 1.0, &on(&["back"]));
        assert!(
            !lit(&controls, &instance(&a, "back")),
            "b released a on back"
        );
        assert!(lit(&controls, &a), "its own still plays");
    }

    #[test]
    fn a_recompile_keeps_the_instances_that_sound() {
        let controls = VenueControls::default();
        let wash = faded("wash", "toggle", 0.0, 0.0, "seconds");
        let own = pad(&wash);
        let layout = layout(&[], vec![aimed("front", "front"), aimed("back", "back")]);
        controls.install(std::slice::from_ref(&wash), &layout);
        let at = Instant::now();
        for group in ["front", "back"] {
            hold(&controls, group, true, at);
            controls.press(&own, true, at);
            hold(&controls, group, false, at);
        }
        let front = perform::instance(&own, &on(&["front"]));
        let back = perform::instance(&own, &on(&["back"]));
        hold(&controls, "back", true, at);
        controls.press(&own, true, at);
        hold(&controls, "back", false, at);
        assert_eq!(controls.made(), 2);
        controls.install(&[wash], &layout);
        let playing = controls.playing();
        assert!(playing.instances.contains_key(&front), "on, kept");
        assert!(lit(&controls, &front));
        assert!(!playing.instances.contains_key(&back), "off, forgotten");
        assert!(!playing.buttons.contains_key(&back));
    }

    #[test]
    fn a_fader_cell_moves_every_target() {
        let controls = VenueControls::default();
        let speeds = CellContent::Fader {
            label: "Speed".into(),
            targets: vec![
                ControlTarget::Level {
                    pattern_id: "red".into(),
                },
                ControlTarget::Node {
                    pattern_id: "chase".into(),
                    node: "control1".into(),
                },
                ControlTarget::Named {
                    name: "drop".into(),
                },
            ],
        };
        controls.install(
            &[pattern("red"), pattern("chase")],
            &layout(&[], vec![cell("speeds", 0, 1, speeds)]),
        );
        controls.set(&perform::cell("speeds"), 0.25);
        for name in [
            perform::cell("speeds"),
            perform::level("red"),
            perform::node("chase", "control1"),
            "drop".to_owned(),
        ] {
            assert_eq!(controls.value(&name), 0.25, "{name}");
        }
    }

    /// A color cell `id` of three swatches, red, green and blue, fading
    /// over `fade` beats into pattern `wash`'s node `hue`, stepping as
    /// `step` says.
    fn colors(id: &str, fade: f64, step: Option<Step>) -> Cell {
        cell(
            id,
            0,
            2,
            CellContent::Color {
                label: "Colors".into(),
                swatches: vec![[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                fade,
                unit: TimeUnit::Beats,
                targets: vec![ControlTarget::Node {
                    pattern_id: "wash".into(),
                    node: "hue".into(),
                }],
                step,
            },
        )
    }

    /// Choose swatch `index` of color cell `id` at `at`, as a pad press.
    fn choose(controls: &VenueControls, id: &str, index: usize, at: Instant) {
        controls.press(&perform::swatch(id, index), true, at);
        controls.press(&perform::swatch(id, index), false, at);
    }

    #[test]
    fn a_swatch_fades_its_targets_to_it_in_time() {
        let controls = VenueControls::default();
        controls.install(&[], &layout(&[], vec![colors("cell", 2., None)]));
        let hue = perform::node("wash", "hue");
        let start = Instant::now();
        assert_eq!(controls.color_at(&hue, start), UNSET_COLOR, "never set");
        choose(&controls, "cell", 0, start);
        // Two beats at the untapped 120 BPM is one second.
        let halfway = controls.color_at(&hue, after(start, 0.5));
        assert_eq!(
            halfway,
            controls.color_at(&hue, after(start, 0.5)),
            "a pure function of time"
        );
        assert!(
            halfway[0] == 1. && 0. < halfway[1] && halfway[1] < 1.,
            "on the way from white to red: {halfway:?}"
        );
        assert_eq!(controls.color_at(&hue, after(start, 1.)), [1., 0., 0.]);

        // A swatch chosen midway fades from where the color is then.
        let again = after(start, 2.);
        choose(&controls, "cell", 2, again);
        let mid = controls.color_at(&hue, after(again, 0.5));
        assert!(
            mid[0] > 0. && mid[2] > 0. && mid[1] == 0.,
            "red to blue: {mid:?}"
        );
        assert_eq!(controls.color_at(&hue, after(again, 1.)), [0., 0., 1.]);
        assert_eq!(controls.playing().swatches["cell"], 2);
    }

    #[test]
    fn a_stepping_cell_moves_on_at_its_bars() {
        let controls = VenueControls::default();
        let step = |loops| Step { bars: 2, loops };
        controls.install(
            &[],
            &layout(
                &[],
                vec![
                    colors("looping", 0., Some(step(true))),
                    colors("once", 0., Some(step(false))),
                ],
            ),
        );
        let start = Instant::now();
        let on = |id: &str| controls.playing().swatches.get(id).copied();
        // A deck's clock, read at its beats.
        controls.follow(Some(120.));
        let read = |beat| controls.tick(Some((beat, 4.)), start);
        read(1.5);
        assert_eq!(
            on("looping"),
            None,
            "nothing steps before a swatch is chosen"
        );
        choose(&controls, "looping", 0, start);
        choose(&controls, "once", 1, start);
        // Chosen in the bar from beat 4; two bars later is beat 12.
        for beat in [5., 8., 11.9] {
            read(beat);
            assert_eq!(on("looping"), Some(0), "still in its bars at beat {beat}");
        }
        read(12.);
        assert_eq!(on("looping"), Some(1), "on at the bar");
        assert_eq!(on("once"), Some(2));
        read(20.);
        assert_eq!(on("looping"), Some(2));
        assert_eq!(on("once"), Some(2), "it stays on the last");
        read(28.);
        assert_eq!(on("looping"), Some(0), "back to the first");

        // Recompiling the set keeps the swatch on.
        controls.install(
            &[],
            &layout(&[], vec![colors("looping", 0., Some(step(true)))]),
        );
        assert_eq!(on("looping"), Some(0));
    }

    #[test]
    fn a_button_node_is_declared_and_read_by_its_pattern() {
        let graph = luma_patterns::ClipGraph::from_json(
            r#"{"version": 3, "nodes": {
                "button1": {"kind": "button", "settings": {"name": "hit", "mode": "toggle"}},
                "curve1": {"kind": "curve", "inputs": {"x": {"node": "button1"}}},
                "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}}"#,
        )
        .unwrap();
        let controls = Arc::new(VenueControls::default());
        controls.install(
            &[PerformPattern {
                graph: graph.clone(),
                ..pattern("chase")
            }],
            &PerformLayout::default(),
        );
        let input = &graph.live_inputs()[0];
        let source = PatternControls::new(controls.clone(), "chase");
        use luma_patterns::ControlSource as _;
        assert_eq!(source.value(input), 0.0);
        let name = perform::node("chase", "button1");
        controls.press(&name, true, Instant::now());
        controls.press(&name, false, Instant::now());
        assert_eq!(source.value(input), 1.0, "a toggle node holds");
        assert_eq!(
            controls.value("hit"),
            0.0,
            "the venue's control of its name is another"
        );
    }

    #[test]
    fn an_undeclared_name_is_momentary() {
        let controls = VenueControls::default();
        controls.press("drop", true, Instant::now());
        assert_eq!(controls.value("drop"), 1.0);
        controls.press("drop", false, Instant::now());
        assert_eq!(controls.value("drop"), 0.0);
    }

    #[test]
    fn taps_set_the_tempo_and_land_on_a_beat() {
        let controls = VenueControls::default();
        let start = Instant::now();
        for tap in 0..4 {
            let at = start + Duration::from_millis(500 * tap);
            controls.press(perform::TAP, true, at);
            controls.press(perform::TAP, false, at + Duration::from_millis(50));
        }
        let last = start + Duration::from_millis(1500);
        let tempo = controls.tempo(last);
        assert!((tempo.bpm - 120.0).abs() < 1e-6, "{}", tempo.bpm);
        assert_eq!(tempo.beat.fract(), 0.0, "the last tap is on a beat");
        let half = controls.tempo(last + Duration::from_millis(250)).beat;
        assert!((half - tempo.beat - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_first_tap_of_a_run_starts_a_bar() {
        let controls = VenueControls::default();
        let start = Instant::now();
        let tap = |at: Instant| {
            controls.press(perform::TAP, true, at);
            controls.press(perform::TAP, false, at + Duration::from_millis(50));
        };
        tap(start);
        tap(start + Duration::from_millis(500));
        // A pause, then a tap a beat and a bit later: a new run, so the 1.
        let again = start + Duration::from_millis(3_200);
        tap(again);
        let beat = controls.tempo(again).beat;
        assert_eq!(beat.rem_euclid(TAPPED_BEATS_PER_BAR), 0.0, "{beat}");
        tap(again + Duration::from_millis(500));
        let next = controls.tempo(again + Duration::from_millis(500)).beat;
        assert_eq!(next - beat, 1.0, "a later tap is the next beat");
    }

    /// A pattern whose pad, a toggle, starts on `start` ("beat" or "bar"),
    /// at once on and off.
    fn quantised(id: &str, mode: &str, start: &str) -> PerformPattern {
        let mut pattern = faded(id, mode, 0.0, 0.0, "seconds");
        let node = pattern.graph.pad().unwrap().node;
        pattern
            .graph
            .nodes
            .get_mut(&node)
            .unwrap()
            .settings
            .insert("start".into(), start.into());
        pattern
    }

    /// The instant of beat `beat` of the tapped tempo, 120 BPM from a
    /// first tap at `start`, which is beat 0.
    fn beat(start: Instant, beat: f64) -> Instant {
        after(start, beat * 0.5)
    }

    /// A bank whose tapped tempo starts a bar at `start`.
    fn tapped(set: &[PerformPattern], layout: &PerformLayout, start: Instant) -> VenueControls {
        let controls = VenueControls::default();
        controls.install(set, layout);
        controls.press(perform::TAP, true, start);
        controls.press(perform::TAP, false, start);
        controls
    }

    /// Whether button `name` is on at `at`, and whether it waits then.
    fn state_at(controls: &VenueControls, name: &str, at: Instant) -> (bool, bool) {
        let held = controls.read().buttons[name];
        let button = held.at(at);
        (button.on(), button.waiting())
    }

    #[test]
    fn a_press_on_the_beat_opens_at_the_next_and_lets_go_on_one_too() {
        let start = Instant::now();
        let wash = quantised("wash", "toggle", "beat");
        let on = pad(&wash);
        let controls = tapped(&[wash], &PerformLayout::default(), start);
        controls.press(&on, true, beat(start, 2.3));
        assert_eq!(
            state_at(&controls, &on, beat(start, 2.9)),
            (false, true),
            "waits for beat 3"
        );
        assert_eq!(controls.value_at(&on, beat(start, 2.9)), 0.0);
        assert_eq!(state_at(&controls, &on, beat(start, 3.01)), (true, false));
        assert_eq!(controls.value_at(&on, beat(start, 3.01)), 1.0);

        // Its release lands on a beat too: on until beat 6.
        controls.press(&on, true, beat(start, 5.5));
        assert_eq!(state_at(&controls, &on, beat(start, 5.9)), (true, true));
        assert_eq!(controls.value_at(&on, beat(start, 5.9)), 1.0);
        assert_eq!(state_at(&controls, &on, beat(start, 6.01)), (false, false));
        assert_eq!(controls.value_at(&on, beat(start, 6.01)), 0.0);
    }

    #[test]
    fn a_press_on_the_bar_opens_at_the_next_bar_of_the_taps() {
        let start = Instant::now();
        let wash = quantised("wash", "toggle", "bar");
        let on = pad(&wash);
        let controls = tapped(&[wash], &PerformLayout::default(), start);
        controls.press(&on, true, beat(start, 2.3));
        assert!(
            state_at(&controls, &on, beat(start, 3.9)).1,
            "waits for the bar"
        );
        assert_eq!(state_at(&controls, &on, beat(start, 4.01)), (true, false));
    }

    #[test]
    fn a_press_lands_on_the_followed_decks_bars() {
        let start = Instant::now();
        let wash = quantised("wash", "toggle", "bar");
        let on = pad(&wash);
        let controls = tapped(&[wash], &PerformLayout::default(), start);
        // A deck at 240 BPM in bars of 3, read at beat 4.5 of its grid: its
        // next bar is beat 6, a beat and a half, 0.375 s, on.
        controls.follow(Some(240.));
        controls.tick(Some((4.5, 3.)), start);
        controls.press(&on, true, start);
        assert!(state_at(&controls, &on, after(start, 0.37)).1);
        assert_eq!(state_at(&controls, &on, after(start, 0.38)), (true, false));
    }

    #[test]
    fn a_change_still_to_come_is_called_off_by_the_opposite() {
        let start = Instant::now();
        let set = [
            quantised("toggle", "toggle", "beat"),
            quantised("flash", "flash", "beat"),
        ];
        let [toggle, flash] = [&set[0], &set[1]].map(pad);
        let controls = tapped(&set, &PerformLayout::default(), start);
        controls.press(&toggle, true, beat(start, 2.3));
        controls.press(&toggle, true, beat(start, 2.6));
        let (on, waiting) = state_at(&controls, &toggle, beat(start, 3.5));
        assert!(!on && !waiting, "pressed twice before its beat: nothing");

        // A flash let go before its beat still plays for one, so a tap lands.
        controls.press(&flash, true, beat(start, 2.3));
        controls.press(&flash, false, beat(start, 2.4));
        assert_eq!(state_at(&controls, &flash, beat(start, 3.5)), (true, true));
        assert_eq!(
            state_at(&controls, &flash, beat(start, 4.01)),
            (false, false)
        );
    }

    #[test]
    fn a_quantised_press_releases_its_row_when_it_goes_on() {
        let start = Instant::now();
        let set = [
            quantised("a", "toggle", "beat"),
            quantised("b", "toggle", "bar"),
            quantised("c", "toggle", "beat"),
        ];
        let [a, b, c] = [&set[0], &set[1], &set[2]].map(pad);
        let controls = tapped(&set, &layout(&[&["a", "b", "c"]], Vec::new()), start);
        controls.press(&a, true, beat(start, 0.5));
        controls.press(&b, true, beat(start, 1.5));
        assert!(
            state_at(&controls, &a, beat(start, 3.9)).0,
            "a plays to b's bar"
        );
        assert_eq!(state_at(&controls, &a, beat(start, 4.01)), (false, false));
        assert_eq!(state_at(&controls, &b, beat(start, 4.01)), (true, false));

        // b let go, to its bar; a pressed, then c, both on beat 5.
        controls.press(&b, true, beat(start, 4.5));
        controls.press(&a, true, beat(start, 4.6));
        controls.press(&c, true, beat(start, 4.7));
        let (on, waiting) = state_at(&controls, &a, beat(start, 5.5));
        assert!(
            !on && !waiting,
            "a, still to come when c was pressed, is called off"
        );
        assert_eq!(state_at(&controls, &c, beat(start, 5.01)), (true, false));
        assert_eq!(
            state_at(&controls, &b, beat(start, 5.01)),
            (false, false),
            "b goes as c comes, not at its own bar"
        );
    }

    #[test]
    fn an_instance_waits_for_its_beat_too() {
        let start = Instant::now();
        let wash = quantised("wash", "toggle", "beat");
        let own = pad(&wash);
        let controls = tapped(&[wash], &layout(&[], vec![aimed("front", "front")]), start);
        let front = perform::instance(&own, &on(&["front"]));
        hold(&controls, "front", true, beat(start, 2.3));
        controls.press(&own, true, beat(start, 2.3));
        hold(&controls, "front", false, beat(start, 2.4));
        assert_eq!(state_at(&controls, &front, beat(start, 2.5)), (false, true));
        assert_eq!(
            state_at(&controls, &front, beat(start, 3.01)),
            (true, false)
        );
        // Recompiled while it waits, it is kept.
        controls.install(
            &[quantised("wash", "toggle", "beat")],
            &layout(&[], vec![aimed("front", "front")]),
        );
        assert!(controls.playing().instances.contains_key(&front));
    }
}
