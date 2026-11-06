//! A venue's live controls: named values 0–1 that faders, pads, MIDI, OSC
//! and any other input set while the lights play.
//!
//! A control exists by being named. A score's pattern that reads `drop` (a
//! fader or button node named `drop`) makes `drop` a control of its venue,
//! and it reads 0 until something sets it.
//!
//! The Perform set plays through controls too, under names of its own
//! ([`perform`]): each pattern's level, each fader and button node of its
//! graph (its pad among them), each fader cell, the Perform master, and tap
//! tempo. So
//! everything that can move a control (an on-screen pad, a MIDI key, an OSC
//! message) can play the set, and the bank is the one place that enforces
//! how they play.
//!
//! A control is a fader or a button. A fader holds the value it was set to.
//! A button is declared with the [`Press`] that plays it ([`install`]): the
//! bank keeps its gate (when it went on and off) and its value is its
//! envelope through that gate, now. A press flips a toggle and holds a
//! flash; setting a button outright gates it on above ½ and off below. A
//! name pressed that no button declares is momentary: 1 while held. Turning
//! a pad on in a one-at-a-time row releases the row's others, a fader cell
//! moves every one of its targets, and a tap lands on the venue's tempo.
//!
//! Values are live process state, like the render engine's, so they live
//! here rather than in the database: every scene compiled for a venue reads
//! the one bank of that venue, and setting a value never recompiles.
//!
//! [`install`]: VenueControls::install

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use luma_patterns::clip_graph::{LiveInput, LiveInputKind};
use luma_patterns::press::{Button, Gate, Press};

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::perform::{CellContent, PerformLayout, PerformPattern, Tempo};

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

    /// Fader cell `id`'s own value, which moves its targets.
    pub fn cell(id: &str) -> String {
        format!("perform_c{}", key(id))
    }

    /// The control a fader cell's `target` moves.
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
    tempo: TapTempo,
    /// The tempo of the deck the set follows, while it follows one.
    deck_bpm: Option<f64>,
}

/// A declared button: how it plays, and when it went on and off since;
/// `None` until first pressed.
#[derive(Clone, Copy, Debug)]
struct Held {
    press: Press,
    gate: Option<(Instant, Option<Instant>)>,
}

impl Held {
    fn on(&self) -> bool {
        matches!(self.gate, Some((_, None)))
    }

    /// The button as it stands at `at`.
    fn at(&self, at: Instant) -> Button {
        let since = |then: Instant| at.saturating_duration_since(then).as_secs_f64();
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
                tempo: TapTempo::default(),
                deck_bpm: None,
            }),
        }
    }
}

impl Bank {
    /// The tempo a button's beats are counted in: the followed deck's, else
    /// the tapped one.
    fn bpm(&self) -> f64 {
        self.deck_bpm.unwrap_or(self.tempo.bpm)
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
            self.gate(name, value > 0.5, at);
            return;
        }
        let was = self.values.insert(name.to_owned(), value).unwrap_or(0.0);
        if name == perform::TAP && value > 0.5 && was <= 0.5 {
            self.tempo.tap(at);
        }
    }

    /// Gate declared button `name` on or off at `at`; one already so stays
    /// as it is. Gating a pad on releases its row's others.
    fn gate(&mut self, name: &str, on: bool, at: Instant) {
        let Some(button) = self.buttons.get_mut(name) else {
            return;
        };
        match (on, button.gate) {
            (true, Some((_, None))) | (false, None | Some((_, Some(_)))) => return,
            (true, _) => button.gate = Some((at, None)),
            (false, Some((since, None))) => button.gate = Some((since, Some(at))),
        }
        if !on {
            return;
        }
        if let Some(row) = self.exclusive.get(name).cloned() {
            for other in row.iter().filter(|other| *other != name) {
                self.gate(other, false, at);
            }
        }
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
        let Some(button) = bank.buttons.get(name) else {
            bank.set(name, f64::from(u8::from(down)), at);
            return;
        };
        match button.press.mode {
            luma_patterns::press::PressMode::Toggle if down => {
                let on = !button.on();
                bank.gate(name, on, at);
            }
            luma_patterns::press::PressMode::Toggle => {}
            luma_patterns::press::PressMode::Flash => bank.gate(name, down, at),
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

    /// Every declared button as it stands now. Its beats are counted in the
    /// set's tempo: the followed deck's, else the tapped one.
    pub fn buttons(&self) -> BTreeMap<String, Button> {
        let now = Instant::now();
        self.read()
            .buttons
            .iter()
            .map(|(name, held)| (name.clone(), held.at(now)))
            .collect()
    }

    /// Every control set so far. A button is not set but played: see
    /// [`Self::buttons`].
    pub fn values(&self) -> BTreeMap<String, f64> {
        self.read().values.clone()
    }

    /// Make `patterns` the venue's Perform set, laid out by `layout`: declare
    /// each button node of each pattern's graph, start each level at 1, link
    /// each fader cell to its targets, and make each one-at-a-time row's
    /// pads release each other. What already plays
    /// holds, so recompiling the set changes nothing that is playing.
    pub(crate) fn install(&self, patterns: &[PerformPattern], layout: &PerformLayout) {
        let pads: HashMap<&str, String> = patterns
            .iter()
            .filter_map(|pattern| Some((pattern.id.as_str(), perform::pad(pattern)?)))
            .collect();
        let mut rows: BTreeMap<(&str, u16), Vec<String>> = BTreeMap::new();
        let mut links = HashMap::new();
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
                    LiveInputKind::Fader => None,
                })
        });
        let mut bank = self.write();
        let mut buttons = HashMap::new();
        for (name, press) in declared {
            let gate = bank.buttons.get(&name).and_then(|button| button.gate);
            buttons.insert(name, Held { press, gate });
        }
        bank.buttons = buttons;
        for pattern in patterns {
            bank.values
                .entry(perform::level(&pattern.id))
                .or_insert(1.0);
        }
        bank.links = links;
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

    /// Count buttons' beats at the tempo of the deck the set follows, or
    /// with `None`, at the tapped tempo again.
    pub(crate) fn follow(&self, deck_bpm: Option<f64>) {
        self.write().deck_bpm = deck_bpm.filter(|bpm| bpm.is_finite() && *bpm > 0.0);
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Bank> {
        self.bank.read().expect("controls poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Bank> {
        self.bank.write().expect("controls poisoned")
    }
}

/// A score reads the venue's controls by name: a fader node its value, a
/// button node too, as a plain gate. Its own envelope is the Perform set's
/// alone, where it is declared.
impl luma_patterns::ControlSource for VenueControls {
    fn value(&self, input: &LiveInput) -> f64 {
        VenueControls::value(self, &input.name)
    }
}

/// What a Perform pattern's graph reads: each fader and button node is a
/// value of its own pattern ([`perform::node`]), not a venue control.
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
}

/// Tempo from taps: the beat runs on at `bpm` from `anchor`, where it was
/// `anchor_beat`. Each tap lands on the nearest beat, so tapping also sets
/// the phase.
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
        self.taps.push_back(at);
        while self.taps.len() > TAPS_AVERAGED {
            self.taps.pop_front();
        }
        let beat = self.beat_at(at).round();
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

fn banks() -> &'static Mutex<HashMap<String, Arc<VenueControls>>> {
    static BANKS: OnceLock<Mutex<HashMap<String, Arc<VenueControls>>>> = OnceLock::new();
    BANKS.get_or_init(Default::default)
}

/// The bank of `venue_id`, made on first use.
pub fn venue(venue_id: &str) -> Arc<VenueControls> {
    banks()
        .lock()
        .expect("controls poisoned")
        .entry(venue_id.to_owned())
        .or_default()
        .clone()
}

/// Forget every venue's values: another person signed in.
pub(crate) fn clear() {
    banks().lock().expect("controls poisoned").clear();
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
pub(crate) async fn list(access: &mut impl AuthorizedVenue) -> Result<Vec<Control>, String> {
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
    let values = venue(&venue_id).values();
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
mod tests {
    use super::*;
    use crate::models::perform::{Cell, ControlTarget, LayoutPage};

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

    /// A pattern whose pad plays as `mode` ("toggle" or "flash"), fading in
    /// over `attack` and out over `release`, linearly, in `unit`.
    fn faded(id: &str, mode: &str, attack: f64, release: f64, unit: &str) -> PerformPattern {
        let mut graph = luma_patterns::ClipGraph::from_json(
            r#"{"version": 3, "nodes": {"color1": {"kind": "color"}}}"#,
        )
        .unwrap()
        .with_pad();
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

    /// Whether button `name` is on.
    fn lit(controls: &VenueControls, name: &str) -> bool {
        controls.buttons()[name].on()
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
        let followed = controls.value_at(&on, after(start, 1.0));
        assert!(tapped < followed, "{tapped} < {followed}");
        assert_eq!(followed, 1.0);
        controls.follow(None);
        assert_eq!(controls.value_at(&on, after(start, 1.0)), tapped);
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
}
