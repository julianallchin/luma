//! The Perform tab: one venue's live show.
//!
//! It has two views. Perform is the show: the venue's pages of cells, to
//! play and nothing else. Setup is the same grid to arrange: cells placed
//! from an empty place's "+", moved and resized by dragging, rows that play
//! one at a time, and an inspector for the selected cell: its pattern's name
//! and group, and what it is mapped to.
//!
//! Beside the stage, in either view, the deck panel holds the decks being
//! followed (or, with no deck to follow, the tapped tempo) and, under them,
//! the DJ source: "Add source" to connect one, then change or disconnect it,
//! with its latency and what the inputs are doing.
//!
//! A cell is a pattern of the venue's Perform set: its pad (the button node
//! its graph's alpha depends on) turns it on and off, its level fader and the
//! other faders and buttons of its graph play it, and it shows its live
//! colors. A cell can also hold a venue group to choose, a list of values (a
//! color list with no targets sets the base color), a fader of its own that
//! moves several things at once, or a utility: the master, tap, Select or
//! Clear. Choose where, then do something: a group tapped stays chosen, and a
//! pad tapped then plays its pattern there (the bank's rules, in
//! `luma_lib::controls`). Everything moves a control of the venue: the
//! backend's control bank holds them and every input writes there, so a pad
//! pressed here, a MIDI key and an OSC button are one thing; this tab shows
//! what the bank holds, as of its last read. Learn binds the next input heard
//! to one of a cell's controls.
//!
//! With the Virtual source, Luma's own player is the deck: Choose song opens
//! the add-song dialog to pick any library track, which loads and plays. It
//! is for practising and seeing patterns, on the player the track editor
//! uses.
//!
//! The backend owns everything live (the follow session, the Perform set,
//! the control bank, the input hub), so this tab only reads and asks. It
//! polls while it is open: decks, values and the last input heard change
//! many times a second and are nobody's event.

mod add;
mod deck_strip;
mod grid;
mod inspector;
pub(crate) mod pattern_sheet;
mod surface;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, Focusable as _, Hsla, SharedString, Window};
pub(super) use luma_lib::controls::perform::{Part, PartKind};
use luma_lib::controls::{perform as names, Control, Look, Played, UNSET_COLOR, UNTAPPED_BPM};
use luma_lib::dispatch::handlers::inputs::{Address, Binding, ControllerState, InputState, Mode};
use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::perform::{
    Cell, CellContent, ListValue, PerformClock, PerformLayout, PerformPattern, PerformSource,
    PerformState,
};
use luma_patterns::press::Button;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::{float, glass, ladder};

use crate::shell::Body;
use crate::workspace::Target;
use crate::Luma;
use surface::{Edit, Rect, View};

/// How often the decks, values and inputs are read while the tab is open.
const POLL: Duration = Duration::from_millis(40);
/// The venue's own controls come from its patterns, which change rarely:
/// read them every this many polls.
const CONTROLS_EVERY: u64 = 25;
/// How long a value moved here wins over what a poll read, so a read that
/// left before the move cannot pull it back.
const HELD_FOR: Duration = Duration::from_millis(400);
/// How long a swatch's color rests in its picker before it is saved.
const SAVE_COLOR_AFTER: Duration = Duration::from_millis(300);

pub(crate) struct Perform {
    pub(crate) venue_id: String,
    pub(crate) chat: crate::agent::BodyChat,
    follow: Option<PerformState>,
    inputs: Option<InputState>,
    /// The venue's Perform set, the top first. `None` until read.
    pub(crate) patterns: Option<Vec<PerformPattern>>,
    /// The venue's layout. `None` until read.
    layout: Option<PerformLayout>,
    /// Bumped by every edit made here, to the set and to the layout. A read
    /// that left before an edit would put back what the edit replaced, so it
    /// lands only if nothing was edited since it left.
    edits: u64,
    /// The bank's count of sets installed, as of the last poll: when it
    /// moves, the set as stored may have changed, and is read again.
    installs: u64,
    /// The venue's own controls, for a fader cell to move.
    controls: Vec<Control>,
    /// The venue's named groups, for a pattern to light.
    groups: Vec<String>,
    surface: surface::Surface,
    /// The graph editor, open on one of the set's patterns.
    pub(crate) pattern_sheet: Option<pattern_sheet::PatternSheet>,
    /// Values moved here, and when, so a stale read does not snap them back.
    held: HashMap<String, (f64, Instant)>,
    /// The button the pointer holds down, so only its own release lets go.
    pressed: Option<String>,
    /// The control a Learn click waits to bind, and the poll it began at:
    /// a read that left before then cannot say it ended.
    learning: Option<String>,
    learning_since: u64,
    /// A name being typed: a page's, a pattern's or a fader, list or group
    /// cell's.
    draft: Option<Draft>,
    /// The value of a list Setup edits.
    value: Option<ValueEditor>,
    /// The inspector's group menu is open.
    group_menu: bool,
    /// Each matched deck's waveform and beats, read once per track.
    tracks: HashMap<String, Rc<deck_strip::TrackView>>,
    fetching: HashSet<String>,
    /// When the decks' positions were read, to run them on between polls.
    polled_at: Instant,
    /// The app's audio player, as of the last poll: what the virtual deck
    /// plays, and the session its transport speaks for.
    player: Option<HostAudioSnapshot>,
    /// The source just clicked, shown until a read reports it (or the request
    /// fails), so a poll already in flight cannot flip the chooser back.
    choosing: Option<Option<PerformSource>>,
    /// The deck panel's source picker is open.
    source_menu: bool,
    error: Option<String>,
    polls: u64,
}

/// A name being typed, and what it names.
struct Draft {
    of: Named,
    field: Entity<TextInput>,
    _subscriptions: Vec<gpui::Subscription>,
}

#[derive(Clone, Debug, PartialEq)]
enum Named {
    Page(String),
    Pattern(String),
    /// A fader, list or group cell's label, by cell id.
    Label(String),
    /// A controller's name, by its id.
    Controller(String),
}

/// A list's value open in Setup: a color in the picker, which stays open
/// while it is dragged, or a number or the color below, which need none.
pub(super) struct ValueEditor {
    pub cell: String,
    pub index: usize,
    pub picker: Option<(
        Entity<luma_ui::arg::color::ColorArgEditor>,
        gpui::Subscription,
    )>,
}

impl Perform {
    /// Control `name` now: a button's envelope, played on from the last
    /// poll; else what was just moved here, or what the bank holds; `rest`
    /// while the bank has nothing for it.
    fn value(&self, name: &str, rest: f64) -> f64 {
        if let Some(button) = self.button(name) {
            return button.value(self.bpm());
        }
        match self.held.get(name) {
            Some((value, at)) if at.elapsed() < HELD_FOR => *value,
            _ => self
                .follow
                .as_ref()
                .and_then(|follow| follow.bank.controls.get(name).copied())
                .unwrap_or(rest),
        }
    }

    /// Live color `name` now, its fade played on from the last poll; white
    /// while nothing has set it.
    pub(super) fn color(&self, name: &str) -> [f64; 3] {
        self.follow
            .as_ref()
            .and_then(|follow| follow.bank.colors.get(name))
            .map_or(UNSET_COLOR, |color| {
                color
                    .later(self.polled_at.elapsed().as_secs_f64())
                    .value(self.bpm())
            })
    }

    /// How control `name` looks, as the bank last said: what the screen
    /// and the controllers both draw.
    pub(super) fn look(&self, name: &str) -> Option<&Look> {
        self.follow.as_ref()?.bank.surface.get(name)
    }

    /// The value on in list cell `cell`, if one is.
    pub(super) fn value_on(&self, cell: &Cell) -> Option<usize> {
        let CellContent::List { values, .. } = &cell.content else {
            return None;
        };
        (0..values.len()).find(|index| {
            self.look(&names::value(&cell.id, *index))
                .is_some_and(|look| look.on)
        })
    }

    /// Whether any button's envelope or live color's fade is still moving,
    /// so the surface draws every frame until it settles.
    pub(super) fn moving(&self) -> bool {
        let bpm = self.bpm();
        let since = self.polled_at.elapsed().as_secs_f64();
        self.follow.as_ref().is_some_and(|follow| {
            follow
                .bank
                .buttons
                .keys()
                .filter_map(|name| self.button(name))
                .any(|button| button.moving(bpm))
                || follow
                    .bank
                    .colors
                    .values()
                    .any(|color| color.later(since).moving(bpm))
        })
    }

    /// Button `name` as it stands now: as last polled, run on since.
    fn button(&self, name: &str) -> Option<Button> {
        let button = self.follow.as_ref()?.bank.buttons.get(name)?;
        Some(button.later(self.polled_at.elapsed().as_secs_f64()))
    }

    /// The tempo the set's buttons count beats in.
    fn bpm(&self) -> f64 {
        self.tempo().map_or(UNTAPPED_BPM, |(bpm, _)| bpm)
    }

    /// Pattern `id`'s pad control, when its graph has a pad.
    fn pad(&self, id: &str) -> Option<String> {
        names::pad(self.pattern(id)?)
    }

    /// Where pattern `id` plays and whether it takes the color below, as
    /// the bank last said.
    pub(super) fn effect(&self, id: &str) -> Option<&Played> {
        self.follow.as_ref()?.bank.effects.get(id)
    }

    /// `group`'s base color, when its lights have one.
    pub(super) fn base(&self, group: &str) -> Option<[f64; 3]> {
        self.follow.as_ref()?.bank.base.get(group).copied()
    }

    fn pattern(&self, id: &str) -> Option<&PerformPattern> {
        self.patterns
            .as_ref()?
            .iter()
            .find(|pattern| pattern.id == id)
    }

    fn controller(&self, id: &str) -> Option<&ControllerState> {
        self.inputs
            .as_ref()?
            .controllers
            .iter()
            .find(|controller| controller.id == id)
    }

    fn page(&self) -> Option<&luma_lib::models::perform::LayoutPage> {
        self.surface.page(self.layout.as_ref()?)
    }

    fn cell(&self, id: &str) -> Option<&Cell> {
        self.page()?.cells.iter().find(|cell| cell.id == id)
    }

    fn selected_cell(&self) -> Option<&Cell> {
        self.cell(self.surface.selected.as_deref()?)
    }

    fn bindings<'a>(&'a self, control: &'a str) -> impl Iterator<Item = &'a Binding> + 'a {
        self.inputs
            .iter()
            .flat_map(|inputs| inputs.bindings.iter())
            .filter(move |binding| binding.control == control)
    }

    /// What a cell is called.
    fn title(&self, cell: &Cell) -> String {
        match &cell.content {
            CellContent::Pattern { pattern_id } => self
                .pattern(pattern_id)
                .map_or_else(|| "Deleted pattern".into(), |pattern| pattern.name.clone()),
            CellContent::Master => "Master".into(),
            CellContent::Tap => "Tap".into(),
            CellContent::Select => "Select".into(),
            CellContent::Clear => "Clear".into(),
            CellContent::Fader { label, .. } if label.is_empty() => "Fader".into(),
            CellContent::List { label, .. } if label.is_empty() => match cell.content.colors() {
                true => "Colors".into(),
                false => "Numbers".into(),
            },
            CellContent::Group { label, group } if label.is_empty() => group_label(group),
            CellContent::Fader { label, .. }
            | CellContent::List { label, .. }
            | CellContent::Group { label, .. } => label.clone(),
        }
    }

    /// A cell's colour: the color its pattern plays (as its pad looks), a
    /// list's color on, a group's base color, or a quiet white.
    fn tint(&self, cell: &Cell) -> Hsla {
        let quiet = ladder::foreground_alpha(0.8);
        match &cell.content {
            CellContent::Pattern { pattern_id } => self
                .pad(pattern_id)
                .and_then(|pad| self.look(&pad))
                .map_or_else(
                    || ladder::pattern(pattern_id).into(),
                    |look| paint(look.color),
                ),
            CellContent::Fader { .. } => Hsla::from(ladder::accent()),
            CellContent::List { values, .. } => {
                match self.value_on(cell).and_then(|on| values.get(on)) {
                    Some(ListValue::Color(rgb)) => paint(*rgb),
                    _ => Hsla::from(ladder::accent()),
                }
            }
            CellContent::Group { group, .. } => self.base(group).map_or(quiet, paint),
            CellContent::Master | CellContent::Tap | CellContent::Select | CellContent::Clear => {
                quiet
            }
        }
    }

    /// What a hand plays in a cell, each named: a cell's own control by
    /// the cell's name.
    fn parts(&self, cell: &Cell) -> Vec<Part> {
        names::parts(cell, self.patterns.as_deref().unwrap_or_default())
            .into_iter()
            .map(|part| match part.label.is_empty() {
                true => Part {
                    label: self.title(cell),
                    ..part
                },
                false => part,
            })
            .collect()
    }

    /// The tempo the set plays at now, and where its beat is when known.
    fn tempo(&self) -> Option<(f64, Option<f64>)> {
        let follow = self.follow.as_ref()?;
        match follow.clock.as_ref()? {
            PerformClock::Tap(tempo) => Some((
                tempo.bpm,
                Some(tempo.beat + self.polled_at.elapsed().as_secs_f64() * tempo.bpm / 60.),
            )),
            PerformClock::Deck { deck } => follow
                .decks
                .iter()
                .find(|one| one.id == *deck)
                .map(|one| (one.bpm, None)),
        }
    }

    /// The tab as a chat message reports it: the page on screen, the
    /// selected cell, and what plays. Read once per message sent.
    pub(crate) fn agent_context(&self) -> luma_lib::agent::PerformView {
        luma_lib::agent::PerformView::capture(
            self.surface.view == View::Setup,
            self.layout.as_ref(),
            self.surface.page,
            self.surface.selected.as_deref(),
            self.patterns.as_deref().unwrap_or_default(),
            self.follow.as_ref(),
        )
    }

    /// The source shown: the one just chosen, or the one followed.
    fn source(&self) -> Option<&PerformSource> {
        match &self.choosing {
            Some(choice) => choice.as_ref(),
            None => self
                .follow
                .as_ref()
                .and_then(|follow| follow.source.as_ref()),
        }
    }
}

/// A light color (linear Rec. 2020) as the screen shows it.
pub(super) fn paint(rgb: [f64; 3]) -> Hsla {
    luma_ui::arg::color::Light::opaque(rgb).display().into()
}

/// A group's name as a person reads it: `front_wash` is "Front wash".
pub(super) fn group_label(expression: &str) -> String {
    let words = expression.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl Luma {
    /// Open the venue's Perform set in Live, or bring Live on screen.
    pub(crate) fn open_live(&mut self, thread: Option<&str>, cx: &mut Context<Self>) {
        let Some(browser) = &self.songs else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let target = Target::Perform {
            venue: venue_id.clone(),
        };
        if self.workspace.body(&target).is_some() {
            self.workspace.set_mode(target.mode());
            cx.notify();
            return;
        }
        let chat = self.body_chat(&target, thread, cx);
        let state = Perform {
            venue_id: venue_id.clone(),
            chat,
            follow: None,
            inputs: None,
            patterns: None,
            layout: None,
            edits: 0,
            installs: 0,
            controls: Vec::new(),
            groups: Vec::new(),
            surface: surface::Surface::default(),
            pattern_sheet: None,
            held: HashMap::new(),
            pressed: None,
            learning: None,
            learning_since: 0,
            draft: None,
            value: None,
            group_menu: false,
            tracks: HashMap::new(),
            fetching: HashSet::new(),
            polled_at: Instant::now(),
            player: None,
            choosing: None,
            source_menu: false,
            error: None,
            polls: 0,
        };
        self.open_body(target.clone(), move || Body::Perform(Box::new(state)), cx);
        self.poll_perform(target.clone(), cx);
        self.reload_perform_set(&target, cx);
    }

    /// Bind the venue's inputs, reading its controllers and bindings again.
    fn open_inputs(&mut self, target: &Target, cx: &mut Context<Self>) {
        let opened = self.library.input_open(target.venue());
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let opened = opened.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.perform_mut(&target) {
                    match opened {
                        Ok(inputs) => state.inputs = Some(inputs),
                        Err(error) => state.error = Some(error.to_string()),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn perform_mut(&mut self, target: &Target) -> Option<&mut Perform> {
        match self.venues.body_mut(&mut self.workspace, target) {
            Some(Body::Perform(state)) => Some(state),
            _ => None,
        }
    }

    /// Read the follow session, the inputs and (now and then) the controls,
    /// for as long as the tab is open.
    fn poll_perform(&mut self, target: Target, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                // The reads are made inside the update: the library is the app's,
                // and a tab that closed since the last poll ends the loop here.
                let Ok(Some((polled, follow, inputs, controls, player))) =
                    this.update(cx, |this, _| {
                        let venue = this.perform_mut(&target).map(|state| {
                            state.polls += 1;
                            (state.polls, state.venue_id.clone())
                        });
                        venue.map(|(polls, venue)| {
                            (
                                polls,
                                this.library.perform_state(),
                                this.library.input_state(),
                                (polls % CONTROLS_EVERY == 1)
                                    .then(|| this.library.control_list(&venue)),
                                this.library.transport_after(Duration::ZERO),
                            )
                        })
                    })
                else {
                    return;
                };
                let follow = follow.await;
                let inputs = inputs.await;
                let player = player.await;
                let controls = match controls {
                    Some(controls) => Some(controls.await),
                    None => None,
                };
                let landed = this.update(cx, |this, cx| {
                    let Some(state) = this.perform_mut(&target) else {
                        return None;
                    };
                    if let Ok(player) = player {
                        state.player = Some(player);
                    }
                    // A failed read keeps what we knew: dropping to `None` would
                    // flash the chooser to Off until the next poll.
                    let mut installed = false;
                    if let Ok(follow) = follow {
                        if state.choosing.as_ref().is_some_and(|choice| {
                            same_source(choice.as_ref(), follow.source.as_ref())
                        }) {
                            state.choosing = None;
                        }
                        installed = follow.bank.installs != state.installs;
                        state.installs = follow.bank.installs;
                        state.follow = Some(follow);
                        state.polled_at = Instant::now();
                    }
                    if let Ok(inputs) = inputs {
                        // Learning ended somewhere other than here: a binding
                        // landed, or it was cancelled.
                        if inputs.learning.is_none() && polled > state.learning_since {
                            state.learning = None;
                        }
                        state.inputs = Some(inputs);
                    }
                    match controls {
                        Some(Ok(controls)) => state.controls = controls,
                        Some(Err(error)) => state.error = Some(error.to_string()),
                        None => {}
                    }
                    state.held.retain(|_, (_, at)| at.elapsed() < HELD_FOR);
                    let wanted: Vec<String> = state
                        .follow
                        .iter()
                        .flat_map(|follow| follow.decks.iter())
                        .filter_map(deck_strip::track_id)
                        .filter(|id| {
                            !state.tracks.contains_key(*id) && !state.fetching.contains(*id)
                        })
                        .map(str::to_owned)
                        .collect();
                    state.fetching.extend(wanted.iter().cloned());
                    // The set was installed anew: read it, for what changed
                    // in it without this tab, such as Select's blends.
                    let reread = installed && state.patterns.is_some();
                    for track_id in wanted {
                        this.load_deck_track(&target, track_id, cx);
                    }
                    if reread {
                        this.read_perform_set(&target, false, cx);
                    }
                    cx.notify();
                    Some(this.library.debounce(POLL))
                });
                let Ok(Some(wait)) = landed else {
                    return;
                };
                wait.await;
            }
        })
        .detach();
    }

    /// The tab closed: stop waiting on a Learn it started. Following and the
    /// controls carry on; the show does not stop because a tab did.
    pub(crate) fn close_perform(&mut self, state: &Perform, cx: &mut Context<Self>) {
        if state.learning.is_some() {
            let cancel = self.library.input_cancel_learn();
            cx.background_spawn(async move {
                cancel.await.ok();
            })
            .detach();
        }
    }

    /// Set control `name` to `value`, 0–1, as a fader here moved it.
    fn set_perform_value(
        &mut self,
        target: &Target,
        name: &str,
        value: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let value = f64::from(value.clamp(0., 1.));
        state.held.insert(name.to_owned(), (value, Instant::now()));
        let set = self.library.perform_control(name, value);
        cx.notify();
        self.after_request(target.clone(), set, cx);
    }

    /// A button of a cell pressed (`down`) or let go: a pad or another button
    /// node, tap, a group, a list's value, Select or Clear. The backend plays
    /// each as the bank says. A release counts only for the button the
    /// pointer pressed.
    fn press_part(
        &mut self,
        target: &Target,
        name: &str,
        kind: PartKind,
        down: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        match state.pressed.take() {
            _ if down => state.pressed = Some(name.to_owned()),
            Some(pressed) if pressed == name => {}
            pressed => {
                state.pressed = pressed;
                return;
            }
        }
        let request: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> = match kind {
            PartKind::Tap => Box::pin(
                self.library
                    .perform_control(name, f64::from(u8::from(down))),
            ),
            _ => Box::pin(self.library.perform_press(vec![(name.to_owned(), down)])),
        };
        cx.notify();
        self.after_request(target.clone(), request, cx);
    }

    /// Learn: bind the next input heard to control `name`, the way `mode`
    /// says. A second click on the one waiting stops waiting.
    fn learn(&mut self, target: &Target, name: &str, mode: Mode, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        if state.learning.as_deref() == Some(name) {
            return self.stop_learning(target, cx);
        }
        state.learning = Some(name.to_owned());
        state.learning_since = state.polls;
        state.error = None;
        let learn = self.library.input_learn(target.venue(), name, mode);
        let (target, name) = (target.clone(), name.to_owned());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let learned = learn.await;
            this.update(cx, |this, cx| {
                let Some(state) = this.perform_mut(&target) else {
                    return;
                };
                if state.learning.as_deref() == Some(name.as_str()) {
                    state.learning = None;
                    if let Err(error) = learned {
                        state.error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn stop_learning(&mut self, target: &Target, cx: &mut Context<Self>) {
        if let Some(state) = self.perform_mut(target) {
            if state.learning.take().is_some() {
                let cancel = self.library.input_cancel_learn();
                cx.notify();
                self.after_request(target.clone(), cancel, cx);
            }
        }
    }

    /// Put the page on screen on controller `controller_id`'s grid, by its
    /// profile, in place of what it had bound there.
    fn map_page(&mut self, target: &Target, controller_id: &str, cx: &mut Context<Self>) {
        let Some(page) = self
            .perform_mut(target)
            .and_then(|state| state.page())
            .map(|page| page.id.clone())
        else {
            return;
        };
        let map = self
            .library
            .input_map_page(target.venue(), controller_id, &page);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let mapped = map.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.perform_mut(&target) {
                    state.error = mapped.err().map(|error| error.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Change the surface or the layout from the screen. Leaving the cell or
    /// the view a Learn was for stops it, and closes the inspector's menu.
    fn edit_surface(&mut self, target: &Target, edit: Edit, cx: &mut Context<Self>) {
        if matches!(
            edit,
            Edit::View(_) | Edit::Page(_) | Edit::Select(_) | Edit::Adding(_)
        ) {
            self.stop_learning(target, cx);
            if let Some(state) = self.perform_mut(target) {
                state.group_menu = false;
                state.value = None;
            }
        }
        self.apply_surface(target, edit, cx);
    }

    /// Apply `edit`; a changed layout is saved.
    fn apply_surface(&mut self, target: &Target, edit: Edit, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let Some(layout) = state.layout.as_mut() else {
            return;
        };
        let changed = match state.surface.apply(layout, edit) {
            Ok(true) => Some(layout.clone()),
            Ok(false) => None,
            Err(message) => {
                state.error = Some(message);
                None
            }
        };
        if let Some(layout) = changed {
            state.edits += 1;
            state.error = None;
            let save = self.library.save_perform_layout(target.venue(), layout);
            self.after_request(target.clone(), save, cx);
        }
        cx.notify();
    }

    fn change_binding(
        &mut self,
        target: &Target,
        binding: &Binding,
        change: BindingChange,
        cx: &mut Context<Self>,
    ) {
        let venue = target.venue();
        let pending = match change {
            BindingChange::NextMode => self.library.input_update_binding(
                venue,
                &binding.id,
                next_mode(binding.mode),
                binding.invert,
            ),
            BindingChange::Invert => {
                self.library
                    .input_update_binding(venue, &binding.id, binding.mode, !binding.invert)
            }
            BindingChange::Remove => {
                let unbind = self.library.input_unbind(venue, &binding.id);
                return self.after_request(target.clone(), unbind, cx);
            }
        };
        self.after_request(target.clone(), pending, cx);
    }

    fn after_request(
        &mut self,
        target: Target,
        pending: impl std::future::Future<Output = Result<(), crate::LibraryError>> + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let done = pending.await;
            this.update(cx, |this, cx| {
                if let (Some(state), Err(error)) = (this.perform_mut(&target), done) {
                    state.error = Some(error.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn follow_source(
        &mut self,
        target: &Target,
        source: Option<PerformSource>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        state.error = None;
        state.choosing = Some(source.clone());
        state.source_menu = false;
        cx.notify();
        let target = target.clone();
        let request: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> = match source {
            Some(source) => Box::pin(self.library.perform_start(target.venue(), source)),
            None => Box::pin(self.library.perform_stop()),
        };
        cx.spawn(async move |this, cx| {
            let done = request.await;
            this.update(cx, |this, cx| {
                if let (Some(state), Err(error)) = (this.perform_mut(&target), done) {
                    // The backend fell back to not following; show that.
                    state.choosing = None;
                    state.error = Some(error.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Read a matched deck's waveform and beats for its strip.
    fn load_deck_track(&mut self, target: &Target, track_id: String, cx: &mut Context<Self>) {
        let waveform = self.library.track_waveform(&track_id);
        let beats = self.library.track_beats(&track_id);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let view = deck_strip::TrackView::new(waveform.await.ok(), beats.await.ok().flatten());
            this.update(cx, |this, cx| {
                if let Some(state) = this.perform_mut(&target) {
                    state.fetching.remove(&track_id);
                    state.tracks.insert(track_id, Rc::new(view));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Read the venue's Perform set, layout, controls, groups, controllers
    /// and bindings, and have the backend play the set: when the tab opens,
    /// after a failed change here, and when a sync or an agent brings one. A
    /// venue with no layout yet gets one laid out from its set.
    pub(crate) fn reload_perform_set(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.open_inputs(target, cx);
        self.read_perform_set(target, true, cx);
    }

    /// Read the venue's Perform set, layout, controls and groups; with
    /// `open`, have the backend play the set.
    fn read_perform_set(&mut self, target: &Target, open: bool, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let venue_id = state.venue_id.clone();
        let edits = state.edits;
        let patterns = self.library.perform_patterns(&venue_id);
        let layout = self.library.perform_layout(&venue_id);
        let controls = self.library.control_list(&venue_id);
        let groups = self.library.venue_groups(&venue_id);
        let open = open.then(|| self.library.perform_open_set(&venue_id));
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let (patterns, layout) = (patterns.await, layout.await);
            let controls = controls.await.unwrap_or_default();
            let groups = groups.await.unwrap_or_default();
            let opened = match open {
                Some(open) => open.await,
                None => Ok(()),
            };
            this.update(cx, |this, cx| {
                let Some(state) = this.perform_mut(&target) else {
                    return;
                };
                state.groups = groups
                    .into_iter()
                    .filter_map(|group| group.name.filter(|name| !name.is_empty()))
                    .collect();
                state.controls = controls;
                let mut generated = None;
                match (patterns, layout) {
                    // Edited here since the read left: what is here is newer.
                    (Ok(_), Ok(_)) if state.edits != edits => {}
                    (Ok(patterns), Ok(layout)) => {
                        let layout = layout.unwrap_or_else(|| {
                            let made = PerformLayout::generated(&patterns);
                            generated = Some(made.clone());
                            made
                        });
                        state.patterns = Some(patterns);
                        state.layout = Some(layout);
                    }
                    (Err(error), _) | (_, Err(error)) => state.error = Some(error.to_string()),
                }
                if let Err(error) = opened {
                    state.error = Some(error.to_string());
                }
                if let Some(layout) = generated {
                    let save = this.library.save_perform_layout(target.venue(), layout);
                    this.after_request(target.clone(), save, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Store `pattern` (new or changed) and show it in the set at once.
    pub(crate) fn store_perform_pattern(
        &mut self,
        target: &Target,
        pattern: PerformPattern,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        state.edits += 1;
        if let Some(patterns) = state.patterns.as_mut() {
            match patterns.iter_mut().find(|one| one.id == pattern.id) {
                Some(one) => *one = pattern.clone(),
                None => patterns.push(pattern.clone()),
            }
            patterns.sort_by(|a, b| b.z_index.cmp(&a.z_index));
        }
        let save = self.library.save_perform_pattern(target.venue(), pattern);
        cx.notify();
        self.save_pattern(target, save, cx);
    }

    fn save_pattern(
        &mut self,
        target: &Target,
        save: impl std::future::Future<Output = Result<PerformPattern, crate::LibraryError>> + 'static,
        cx: &mut Context<Self>,
    ) {
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let saved = save.await;
            this.update(cx, |this, cx| {
                if let Err(error) = saved {
                    if let Some(state) = this.perform_mut(&target) {
                        state.error = Some(error.to_string());
                    }
                    // What was stored is what stands.
                    this.reload_perform_set(&target, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The top of the set, for a new pattern to go on.
    fn top_of_set(state: &Perform) -> i64 {
        state
            .patterns
            .iter()
            .flatten()
            .map(|pattern| pattern.z_index)
            .max()
            .map_or(0, |z| z + 1)
    }

    /// Where a new cell goes: the place whose "+" menu is open, or else the
    /// first free place of the page shown.
    fn new_place(state: &Perform) -> Option<Rect> {
        match &state.surface.adding {
            Some(adding) => Some(Rect::at(adding.column, adding.row)),
            None => surface::room(state.page()?, 1, 1),
        }
    }

    /// Start a pattern from `preset` at the top of the set, in the place
    /// whose "+" menu is open, lighting the group chosen there (everything,
    /// when none was). From the graph editor's preset browser, it goes back
    /// to the grid with the new cell selected.
    pub(crate) fn add_perform_pattern(
        &mut self,
        target: &Target,
        preset: &luma_patterns::ClipPreset,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let Some(place) = Self::new_place(state) else {
            state.error = Some("No room on this page. Make it bigger or clear a place.".into());
            return cx.notify();
        };
        let expression = state
            .surface
            .adding
            .as_ref()
            .map_or_else(|| "all".to_owned(), |adding| adding.group.clone());
        let pattern = PerformPattern {
            id: uuid::Uuid::new_v4().to_string(),
            venue_id: state.venue_id.clone(),
            name: preset.name.clone(),
            selection: luma_patterns::Selection { expression },
            z_index: Self::top_of_set(state),
            blend_mode: preset.blend_mode,
            seed: uuid::Uuid::new_v4().as_u64_pair().0,
            graph: preset.graph.clone().with_pad(),
        };
        // Picked in the browser: nothing was edited there to save.
        if state
            .pattern_sheet
            .as_ref()
            .is_some_and(|sheet| sheet.pattern.is_none())
        {
            state.pattern_sheet = None;
        }
        let content = CellContent::Pattern {
            pattern_id: pattern.id.clone(),
        };
        self.store_perform_pattern(target, pattern, cx);
        self.apply_surface(target, Edit::Place(place, content), cx);
    }

    /// Add a busk page made from the venue's rig, and show it. It goes after
    /// the set's pages.
    fn add_busk_page(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let index = state.layout.as_ref().map_or(0, |layout| layout.pages.len());
        let made = self.library.perform_add_busk_page(target.venue());
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let made = made.await;
            this.update(cx, |this, cx| {
                let Some(state) = this.perform_mut(&target) else {
                    return;
                };
                match made {
                    Ok(_) => {
                        state.error = None;
                        state.surface.page = index;
                        state.surface.selected = None;
                        state.surface.adding = None;
                        this.reload_perform_set(&target, cx);
                    }
                    Err(error) => state.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put the master, tap or a new fader in the place whose "+" menu is
    /// open.
    fn add_cell(&mut self, target: &Target, content: CellContent, cx: &mut Context<Self>) {
        let Some(place) = self
            .perform_mut(target)
            .and_then(|state| Self::new_place(state))
        else {
            return;
        };
        self.apply_surface(target, Edit::Place(place, content), cx);
    }

    /// Copy cell `cell_id`'s pattern into a new cell of the same size, in the
    /// first place on the page with room for it.
    fn duplicate_cell(&mut self, target: &Target, cell_id: &str, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let (Some(cell), Some(page)) = (state.cell(cell_id), state.page()) else {
            return;
        };
        let Some(original) = cell.pattern_id().and_then(|id| state.pattern(id)) else {
            return;
        };
        let place =
            surface::room(page, cell.width, cell.height).or_else(|| surface::room(page, 1, 1));
        let Some(place) = place else {
            state.error = Some("No room on this page for a copy.".into());
            return cx.notify();
        };
        let copy = PerformPattern {
            id: uuid::Uuid::new_v4().to_string(),
            name: format!("{} copy", original.name),
            z_index: Self::top_of_set(state),
            ..original.clone()
        };
        let content = CellContent::Pattern {
            pattern_id: copy.id.clone(),
        };
        self.store_perform_pattern(target, copy, cx);
        self.apply_surface(target, Edit::Place(place, content), cx);
    }

    /// Ask before deleting a pattern: its cell, its graph and its mappings
    /// go.
    fn ask_delete_pattern(&mut self, target: &Target, pattern_id: &str, cx: &mut Context<Self>) {
        let Some(name) = self
            .perform_mut(target)
            .and_then(|state| state.pattern(pattern_id))
            .map(|pattern| pattern.name.clone())
        else {
            return;
        };
        self.ask(
            crate::confirm::Confirm {
                title: format!("Delete {name}?").into(),
                body: "The pattern and its cell go. This can't be undone.".into(),
                verb: "Delete pattern".into(),
                action: crate::confirm::Action::DeletePerformPattern {
                    venue_id: target.venue().into(),
                    pattern_id: pattern_id.into(),
                },
            },
            cx,
        );
    }

    /// Delete pattern `pattern_id` of venue `venue_id`'s set, and its cell.
    pub(crate) fn delete_perform_pattern(
        &mut self,
        venue_id: &str,
        pattern_id: &str,
        cx: &mut Context<Self>,
    ) {
        let target = Target::Perform {
            venue: venue_id.to_owned(),
        };
        let Some(state) = self.perform_mut(&target) else {
            return;
        };
        state.edits += 1;
        state
            .patterns
            .iter_mut()
            .for_each(|list| list.retain(|pattern| pattern.id != pattern_id));
        if let Some(layout) = state.layout.as_mut() {
            layout.forget(pattern_id);
        }
        if state
            .surface
            .selected
            .as_deref()
            .is_some_and(|id| state.cell(id).is_none())
        {
            state.surface.selected = None;
        }
        if state
            .pattern_sheet
            .as_ref()
            .is_some_and(|sheet| sheet.pattern_id() == pattern_id)
        {
            state.pattern_sheet = None;
        }
        let delete = self.library.delete_perform_pattern(venue_id, pattern_id);
        cx.notify();
        self.after_request(target, delete, cx);
    }

    /// Start typing a name in place of `of`'s.
    fn start_draft(
        &mut self,
        target: &Target,
        of: Named,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let (placeholder, text) = match &of {
            Named::Page(id) => (
                "Page name",
                state
                    .layout
                    .iter()
                    .flat_map(|layout| layout.pages.iter())
                    .find(|page| page.id == *id)
                    .map(|page| page.name.clone()),
            ),
            Named::Pattern(id) => ("Pattern name", state.pattern(id).map(|p| p.name.clone())),
            Named::Controller(id) => (
                "Controller name",
                state.controller(id).map(|c| c.name.clone()),
            ),
            Named::Label(id) => (
                "Name",
                state.cell(id).and_then(|cell| match &cell.content {
                    CellContent::Fader { label, .. }
                    | CellContent::List { label, .. }
                    | CellContent::Group { label, .. } => Some(label.clone()),
                    _ => None,
                }),
            ),
        };
        let Some(text) = text else {
            return;
        };
        let field = cx.new(|cx| {
            let mut input = TextInput::search(placeholder, cx);
            input.set_text(text, cx);
            input
        });
        let keys = {
            let target = target.clone();
            cx.subscribe_in(
                &field,
                window,
                move |this: &mut Luma, _, event: &text_input::Event, _, cx| match event {
                    text_input::Event::Submitted | text_input::Event::Blurred => {
                        this.finish_draft(&target, true, cx)
                    }
                    text_input::Event::Cancelled => this.finish_draft(&target, false, cx),
                    _ => {}
                },
            )
        };
        // Focus leaving by the keyboard; whichever of this and `Blurred`
        // comes second finds nothing to commit.
        let focus = field.read(cx).focus_handle(cx);
        let blur = {
            let (luma, target) = (cx.entity().downgrade(), target.clone());
            window.on_focus_out(&focus, cx, move |_, _, cx| {
                luma.update(cx, |this, cx| this.finish_draft(&target, true, cx))
                    .ok();
            })
        };
        if let Some(state) = self.perform_mut(target) {
            state.draft = Some(Draft {
                of,
                field,
                _subscriptions: vec![keys, blur],
            });
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// End the name being typed; `save` keeps it, when it is not empty.
    fn finish_draft(&mut self, target: &Target, save: bool, cx: &mut Context<Self>) {
        let Some(draft) = self
            .perform_mut(target)
            .and_then(|state| state.draft.take())
        else {
            return;
        };
        cx.notify();
        let typed = draft.field.read(cx).text().trim().to_owned();
        if !save || typed.is_empty() {
            return;
        }
        match draft.of {
            Named::Page(id) => {
                let shown = self
                    .perform_mut(target)
                    .and_then(|state| state.page())
                    .is_some_and(|page| page.id == id);
                if shown {
                    self.apply_surface(target, Edit::RenamePage(typed), cx);
                }
            }
            Named::Pattern(id) => {
                let renamed = self
                    .perform_mut(target)
                    .and_then(|state| state.pattern(&id))
                    .filter(|pattern| pattern.name != typed)
                    .map(|pattern| PerformPattern {
                        name: typed,
                        ..pattern.clone()
                    });
                if let Some(pattern) = renamed {
                    self.store_perform_pattern(target, pattern, cx);
                }
            }
            Named::Label(id) => {
                let content = self
                    .perform_mut(target)
                    .and_then(|state| state.cell(&id))
                    .and_then(|cell| {
                        let mut content = cell.content.clone();
                        match &mut content {
                            CellContent::Fader { label, .. }
                            | CellContent::List { label, .. }
                            | CellContent::Group { label, .. } => {
                                *label = typed;
                                Some(content)
                            }
                            _ => None,
                        }
                    });
                if let Some(content) = content {
                    self.apply_surface(target, Edit::SetContent(id, content), cx);
                }
            }
            Named::Controller(id) => {
                let rename = self
                    .library
                    .input_rename_controller(target.venue(), &id, typed);
                self.after_request(target.clone(), rename, cx);
            }
        }
    }

    /// Open the Perform chat with `prompt` in its composer, to send or edit.
    fn ask_agent(
        &mut self,
        target: &Target,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let chat = state.chat.panel.clone();
        if !self.agent_wanted() {
            self.toggle_agent();
        }
        chat.update(cx, |chat, cx| chat.suggest(prompt, window, cx));
        cx.notify();
    }

    /// Open value `index` of list `cell` in Setup, a color in the picker,
    /// or with `None` close it.
    fn open_value(
        &mut self,
        target: &Target,
        cell: &str,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let value = index.and_then(|index| match &state.cell(cell)?.content {
            CellContent::List { values, .. } => values.get(index).copied(),
            _ => None,
        });
        let (Some(index), Some(value)) = (index, value) else {
            state.value = None;
            return cx.notify();
        };
        use luma_ui::arg::color::{ColorArg, ColorArgEditor, ColorArgEvent};
        let picker = match value {
            ListValue::Color(rgb) => {
                let picker = cx.new(|cx| {
                    ColorArgEditor::new(
                        format!("Value {}", index + 1),
                        ColorArg::decode(rgb.map(|c| c as f32), 1.),
                        cx,
                    )
                    .rgb_only()
                });
                let changed = {
                    let (target, cell) = (target.clone(), cell.to_owned());
                    cx.subscribe(
                        &picker,
                        move |this: &mut Luma, _, event: &ColorArgEvent, cx| {
                            let ColorArgEvent::Changed(value) = *event;
                            let rgb = value.encode().0.map(f64::from);
                            this.set_value_color(&target, &cell, index, rgb, cx);
                        },
                    )
                };
                Some((picker, changed))
            }
            ListValue::Number(_) | ListValue::Below => None,
        };
        if let Some(state) = self.perform_mut(target) {
            state.value = Some(ValueEditor {
                cell: cell.to_owned(),
                index,
                picker,
            });
        }
        cx.notify();
    }

    /// Give value `index` of list `cell` the color `rgb`, as its picker is
    /// dragged: it shows at once and is saved once it rests.
    fn set_value_color(
        &mut self,
        target: &Target,
        cell: &str,
        index: usize,
        rgb: [f64; 3],
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let value = state
            .layout
            .iter_mut()
            .flat_map(|layout| layout.pages.iter_mut())
            .flat_map(|page| page.cells.iter_mut())
            .find(|one| one.id == cell)
            .and_then(|one| match &mut one.content {
                CellContent::List { values, .. } => values.get_mut(index),
                _ => None,
            });
        let Some(value) = value else {
            return;
        };
        *value = ListValue::Color(rgb);
        state.edits += 1;
        let edits = state.edits;
        let wait = self.library.debounce(SAVE_COLOR_AFTER);
        let target = target.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            wait.await;
            this.update(cx, |this, cx| {
                // A later edit saves the layout, this color in it.
                let layout = this
                    .perform_mut(&target)
                    .filter(|state| state.edits == edits)
                    .and_then(|state| state.layout.clone());
                if let Some(layout) = layout {
                    let save = this.library.save_perform_layout(target.venue(), layout);
                    this.after_request(target.clone(), save, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open or close the inspector's group menu.
    /// Open or close the deck panel's source picker.
    fn toggle_follow_menu(&mut self, target: &Target, cx: &mut Context<Self>) {
        if let Some(state) = self.perform_mut(target) {
            state.source_menu = !state.source_menu;
            cx.notify();
        }
    }

    fn toggle_group_menu(&mut self, target: &Target, cx: &mut Context<Self>) {
        if let Some(state) = self.perform_mut(target) {
            state.group_menu = !state.group_menu;
            cx.notify();
        }
    }

    /// Have cell `id` light `expression`, a group's name or `all`: its
    /// pattern's selection, or a group cell's group.
    fn set_group(&mut self, target: &Target, id: &str, expression: String, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let Some(content) = state.cell(id).map(|cell| cell.content.clone()) else {
            return;
        };
        match content {
            CellContent::Pattern { pattern_id } => {
                let regrouped = state
                    .pattern(&pattern_id)
                    .filter(|pattern| pattern.selection.expression != expression)
                    .map(|pattern| PerformPattern {
                        selection: luma_patterns::Selection { expression },
                        ..pattern.clone()
                    });
                if let Some(pattern) = regrouped {
                    self.store_perform_pattern(target, pattern, cx);
                }
            }
            CellContent::Group { label, group } if group != expression => {
                let content = CellContent::Group {
                    label,
                    group: expression,
                };
                self.apply_surface(target, Edit::SetContent(id.to_owned(), content), cx);
            }
            _ => {}
        }
    }

    /// Set the tapped tempo, from the tempo strip's nudges.
    fn set_tap_bpm(&mut self, target: &Target, bpm: f64, cx: &mut Context<Self>) {
        let set = self.library.perform_set_bpm(target.venue(), bpm);
        self.after_request(target.clone(), set, cx);
    }

    /// Load `track_id` onto the player and play it from the top: the
    /// virtual deck follows whatever the player has.
    pub(crate) fn play_virtual(&mut self, target: &Target, track_id: &str, cx: &mut Context<Self>) {
        let (session, load) = self.library.load_audio(track_id);
        let play = self.library.play(session, 0.);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let done = match load.await {
                Ok(()) => play.await,
                Err(error) => Err(error),
            };
            if let Err(error) = done {
                this.update(cx, |this, cx| {
                    if let Some(state) = this.perform_mut(&target) {
                        state.error = Some(error.to_string());
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn virtual_transport(
        &mut self,
        target: &Target,
        session: u64,
        transport: Transport,
        cx: &mut Context<Self>,
    ) {
        let request: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> = match transport {
            Transport::PlayFrom(at) => Box::pin(self.library.play(session, at)),
            Transport::Pause => Box::pin(self.library.pause(session)),
            Transport::SeekTo(at) => Box::pin(self.library.seek(session, at)),
        };
        self.after_request(target.clone(), request, cx);
    }

    fn set_perform_latency(&mut self, latency_ms: f32, cx: &mut Context<Self>) {
        let set = self.library.perform_set_latency(latency_ms.round());
        cx.background_spawn(async move {
            set.await.ok();
        })
        .detach();
    }
}

#[derive(Clone, Copy)]
enum BindingChange {
    NextMode,
    Invert,
    Remove,
}

fn next_mode(mode: Mode) -> Mode {
    match mode {
        Mode::Fader => Mode::Momentary,
        Mode::Momentary => Mode::Toggle,
        Mode::Toggle => Mode::Fader,
    }
}

/// How a controller's input plays its control, in a word.
fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Fader => "Fader",
        Mode::Momentary => "Hold",
        Mode::Toggle => "Latching",
    }
}

/// An input as a person reads it: "CC 7 · ch 1", "Note 36 · ch 10",
/// "/glove/tilt 2".
fn address_label(address: &Address) -> String {
    match address {
        Address::MidiCc { channel, number } => format!("CC {number} · ch {}", channel + 1),
        Address::MidiNote { channel, number } => format!("Note {number} · ch {}", channel + 1),
        Address::Osc { address, arg: 0 } => address.clone(),
        Address::Osc { address, arg } => format!("{address} {arg}"),
    }
}

/// An input in a few letters, for a badge on a cell: "CC7", "N36", "OSC".
fn address_badge(address: &Address) -> String {
    match address {
        Address::MidiCc { number, .. } => format!("CC{number}"),
        Address::MidiNote { number, .. } => format!("N{number}"),
        Address::Osc { .. } => "OSC".into(),
    }
}

/// The same kind of source, whatever its settings.
fn same_source(a: Option<&PerformSource>, b: Option<&PerformSource>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => std::mem::discriminant(a) == std::mem::discriminant(b),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// What the virtual deck's buttons ask of the player.
#[derive(Clone, Copy)]
enum Transport {
    PlayFrom(f32),
    Pause,
    SeekTo(f32),
}

pub(crate) fn perform(state: &Perform, app: &Entity<Luma>) -> AnyElement {
    let target = Target::Perform {
        venue: state.venue_id.clone(),
    };
    // The graph editor takes the whole tab until its way back.
    if let Some(page) = pattern_sheet::page(state, &target, app) {
        return div()
            .id("perform")
            .size_full()
            .flex()
            .flex_col()
            .child(page)
            .into_any_element();
    }
    let setup = state.surface.view == View::Setup;
    let main = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(12.))
        .px(px(18.))
        .pt(px(12.))
        .pb(px(16.))
        .children(state.error.clone().map(float::error_row))
        .child(grid::surface(state, &target, app));
    div()
        .id("perform")
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(main)
                .children(setup.then(|| inspector::panel(state, &target, app))),
        )
        .into_any_element()
}

/// The way into the layout's editing and back out, at the end of the page
/// tabs.
///
/// The grid is played by default; "Edit layout" turns the same grid into the
/// one to arrange — the Setup view — and "Done" turns it back. What is
/// followed is the deck panel's and the status bar's to say, not this
/// button's row.
fn edit_layout(state: &Perform, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let (label, next) = if state.surface.view == View::Setup {
        ("Done", View::Perform)
    } else {
        ("Edit layout", View::Setup)
    };
    let (pressed, edited) = (app.clone(), target.clone());
    luma_ui::button(label, luma_ui::Enabled::Yes)
        .id("perform-edit-layout")
        .on_click(move |_, _, cx| {
            pressed.update(cx, |this, cx| {
                this.edit_surface(&edited, Edit::View(next), cx)
            });
        })
        .agent_node(Role::Button, label)
}

impl Luma {
    /// What Live is following, in a few words — the status bar's line while
    /// Live is on screen.
    pub(crate) fn live_line(&self) -> Option<String> {
        let slot = self.workspace.of_mode(crate::workspace::Mode::Live)?;
        let Body::Perform(state) = &slot.body else {
            return None;
        };
        let line = match state
            .follow
            .as_ref()
            .and_then(|follow| follow.problem.clone())
        {
            Some(problem) => problem,
            None => following(state),
        };
        (!line.is_empty()).then_some(line)
    }
}

/// What is followed, in a few words: "Pro DJ Link · 2 decks".
fn following(state: &Perform) -> String {
    let Some(follow) = state.follow.as_ref() else {
        return String::new();
    };
    let Some(source) = follow.source.as_ref().map(source_name) else {
        return "Not following a DJ".into();
    };
    match (follow.connected, follow.decks.len()) {
        (false, _) => format!("{source} · connecting…"),
        (true, 0) => format!("{source} · no decks yet"),
        (true, 1) => format!("{source} · 1 deck"),
        (true, decks) => format!("{source} · {decks} decks"),
    }
}

/// A source's name as the title bar and the deck panel say it.
fn source_name(source: &PerformSource) -> &'static str {
    match source {
        PerformSource::RkbxLink { .. } => "Rekordbox",
        PerformSource::ProDjLink { .. } => "Pro DJ Link",
        PerformSource::StageLinq => "StageLinQ",
        PerformSource::Virtual => "Virtual deck",
    }
}

/// The source the deck panel shows: the one just picked says it is
/// connecting until a read reports it followed.
fn source_line(state: &Perform, source: &PerformSource) -> String {
    let followed = state
        .follow
        .as_ref()
        .and_then(|follow| follow.source.as_ref());
    if same_source(Some(source), followed) {
        following(state)
    } else {
        format!("{} · connecting…", source_name(source))
    }
}

/// The sources Live can follow, as the picker lists them. "Off" is not one:
/// disconnecting is its own button once a source is connected.
fn source_choices() -> [(&'static str, PerformSource); 4] {
    [
        ("Rekordbox", PerformSource::RkbxLink { address: None }),
        ("Pro DJ Link", PerformSource::ProDjLink { device_num: 5 }),
        ("StageLinQ", PerformSource::StageLinq),
        ("Virtual", PerformSource::Virtual),
    ]
}

/// Where a DJ source is connected, under the deck cards: "Add source" while
/// the set runs on the tapped tempo, and the source with its way to change or
/// disconnect once one is followed. The picker opens inline, with the
/// latency; what the source and the inputs are doing sits under it.
fn source_section(state: &Perform, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let follow = state.follow.as_ref();
    // Until the first read lands nothing is known: an unknown source is not
    // "none", and offering "Add source" would flicker for a followed one.
    let known = follow.is_some() || state.choosing.is_some();
    let source = state.source().filter(|_| known);
    let head = match source {
        None => {
            let (app, target) = (app.clone(), target.clone());
            float::nav_row(
                float::RowState::of(state.source_menu, false),
                "perform-add-source",
            )
            .id("perform-add-source")
            .w_full()
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| this.toggle_follow_menu(&target, cx));
            })
            .child(
                gpui_component::Icon::new(luma_ui::icons::IconName::Plus)
                    .size(px(11.))
                    .text_color(glass::ink(glass::TEXT_TERTIARY)),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .text_color(glass::ink(glass::TEXT_SECONDARY))
                    .child("Add source"),
            )
            .agent_node(Role::Button, "Add source")
            .into_any_element()
        }
        Some(chosen) => {
            let (change_app, change_target) = (app.clone(), target.clone());
            let (off_app, off_target) = (app.clone(), target.clone());
            let line = source_line(state, chosen);
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(glass::ink(glass::TEXT_SECONDARY))
                        .child(line.clone())
                        .agent_node(Role::Text, format!("Source: {line}")),
                )
                .child(
                    luma_ui::button("Change", luma_ui::Enabled::Yes)
                        .id("perform-change-source")
                        .on_click(move |_, _, cx| {
                            change_app
                                .update(cx, |this, cx| this.toggle_follow_menu(&change_target, cx));
                        })
                        .agent_node(Role::Button, "Change source")
                        .agent_focused(state.source_menu),
                )
                .child(
                    luma_ui::button("Disconnect", luma_ui::Enabled::Yes)
                        .id("perform-disconnect-source")
                        .on_click(move |_, _, cx| {
                            off_app
                                .update(cx, |this, cx| this.follow_source(&off_target, None, cx));
                        })
                        .agent_node(Role::Button, "Disconnect source"),
                )
                .into_any_element()
        }
    };
    let picker = state.source_menu.then(|| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(source_choices().into_iter().map(|(label, choice)| {
                let chosen = same_source(Some(&choice), source);
                let (app, target) = (app.clone(), target.clone());
                let key = format!("perform-source-{label}");
                float::nav_row(float::RowState::of(chosen, false), key.clone())
                    .id(SharedString::from(key))
                    .w_full()
                    .on_click(move |_, _, cx| {
                        let choice = choice.clone();
                        app.update(cx, |this, cx| this.follow_source(&target, Some(choice), cx));
                    })
                    .child(label)
                    .agent_node(Role::Toggle, label)
                    .agent_focused(chosen)
            }))
            .agent_node(Role::Card, "Source picker")
    });
    // The latency is the source's: shown while choosing one and while one is
    // followed.
    let latency = (state.source_menu || source.is_some()).then(|| {
        let latency = follow.map_or(0., |follow| follow.latency_ms);
        let app = app.clone();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(luma_ui::caption("Latency (ms)"))
            .child(div().flex_1())
            .child(
                luma_ui::luma_slider(
                    "perform-latency",
                    latency,
                    -200.,
                    200.,
                    140.,
                    move |value, _, cx| {
                        app.update(cx, |this, cx| this.set_perform_latency(value, cx));
                    },
                )
                .agent_node(Role::Slider, "Latency"),
            )
    });
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(head)
        .children(picker)
        .children(latency)
        .child(status_line(follow, state.inputs.as_ref()))
        .agent_node(Role::Card, "Source")
}

/// What the source and the inputs are doing, in one line of small type.
fn status_line(follow: Option<&PerformState>, inputs: Option<&InputState>) -> impl IntoElement {
    let mut parts: Vec<String> = Vec::new();
    match follow {
        Some(follow) if follow.source.is_some() => {
            parts.push(if follow.detail.is_empty() {
                if follow.connected {
                    "Connected"
                } else {
                    "Connecting…"
                }
                .to_string()
            } else {
                follow.detail.clone()
            });
            if let Some(ms) = follow.last_heard_ms {
                parts.push(format!("heard {:.1} s ago", ms as f64 / 1000.));
            }
        }
        _ => parts.push("Not following a DJ: the set plays on the tapped tempo".into()),
    }
    if let Some(inputs) = inputs {
        parts.push(match inputs.midi_ports.as_slice() {
            [] => "No MIDI devices".into(),
            ports => format!("MIDI: {}", ports.join(", ")),
        });
        parts.push(format!("OSC: {}", inputs.osc));
        if let Some(last) = &inputs.last {
            if last.ms_ago < 5_000 {
                parts.push(format!(
                    "Last input {} = {:.2}",
                    address_label(&last.address),
                    last.value
                ));
            }
        }
    }
    let line = parts.join("  ·  ");
    div()
        .text_size(px(11.5))
        .text_color(ladder::foreground_alpha(0.5))
        .child(line.clone())
        .agent_node(Role::Text, line)
}

/// Live's deck panel, beside the stage: each deck followed, or the tapped
/// tempo when there is none, and under them where a DJ source is connected,
/// changed or disconnected. What is followed is also the title bar's line.
pub(crate) fn deck_panel(state: &Perform, app: &Entity<Luma>) -> AnyElement {
    let target = Target::Perform {
        venue: state.venue_id.clone(),
    };
    div()
        .id("perform-decks")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(12.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .child("Decks")
                .agent_node(Role::Text, "Decks"),
        )
        .child(decks(state, &target, app))
        .child(source_section(state, &target, app))
        .agent_node(Role::Card, "Deck panel")
        .into_any_element()
}

fn decks(state: &Perform, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let virtual_deck = matches!(state.source(), Some(PerformSource::Virtual));
    let transport = virtual_deck
        .then_some(state.player.as_ref())
        .flatten()
        .filter(|player| player.is_loaded);
    let decks = state
        .follow
        .as_ref()
        .map(|follow| follow.decks.as_slice())
        .unwrap_or_default();
    // The add-song dialog, opened to choose what the virtual deck plays.
    let pick = |label: &'static str| {
        let (app, target) = (app.clone(), target.clone());
        float::btn(label, "virtual-pick")
            .id("virtual-pick")
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| this.show_track_to_play(target.clone(), cx));
            })
            .agent_node(Role::Button, label)
    };
    let tempo = match state
        .follow
        .as_ref()
        .and_then(|follow| follow.clock.as_ref())
    {
        Some(PerformClock::Tap(tempo)) => Some(*tempo),
        _ => None,
    };
    let tap = tempo.map(|tempo| {
        let press = |down: bool| {
            let (app, target) = (app.clone(), target.clone());
            move |app_cx: &mut gpui::App| {
                app.update(app_cx, |this, cx| {
                    this.press_part(&target, names::TAP, PartKind::Tap, down, cx)
                });
            }
        };
        let (down, up, out) = (press(true), press(false), press(false));
        let nudge = |label: &'static str, key: &'static str, bpm: f64| {
            let (app, target) = (app.clone(), target.clone());
            float::btn(label, key)
                .id(key)
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| this.set_tap_bpm(&target, bpm, cx));
                })
                .agent_node(Role::Button, format!("BPM {label}"))
        };
        deck_strip::tempo_strip(
            tempo,
            state.polled_at,
            div()
                .flex()
                .gap(px(4.))
                .child(
                    float::btn_primary("Tap")
                        .id("perform-tap")
                        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| down(cx))
                        .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| up(cx))
                        .on_mouse_up_out(gpui::MouseButton::Left, move |_, _, cx| out(cx))
                        .agent_node(Role::Button, "Tap"),
                )
                .child(nudge("−1", "perform-bpm-down", (tempo.bpm - 1.).round()))
                .child(nudge("+1", "perform-bpm-up", (tempo.bpm + 1.).round()))
                .child(nudge("½×", "perform-bpm-half", tempo.bpm / 2.))
                .child(nudge("2×", "perform-bpm-double", tempo.bpm * 2.))
                .into_any_element(),
        )
    });
    let strips = (!decks.is_empty()).then(|| {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(decks.iter().map(|one| {
                let view = deck_strip::track_id(one).and_then(|id| state.tracks.get(id).cloned());
                let controls = transport.map(|player| {
                    div()
                        .flex()
                        .gap(px(4.))
                        .pt(px(4.))
                        .child(deck_transport(player, target, app))
                        .child(pick("Change song"))
                        .into_any_element()
                });
                deck_strip::strip(one, state.polled_at, view, controls)
            }))
    });
    // A virtual deck with nothing on it says how to put something on.
    let empty_virtual = (virtual_deck && decks.is_empty()).then(|| {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(px(12.))
            .text_color(ladder::foreground_alpha(0.45))
            .child("No song on the virtual deck.")
            .child(pick("Choose song"))
    });
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(8.))
        .children(tap)
        .children(strips)
        .children(empty_virtual)
}

/// Play or pause, and back to the top, for the virtual deck.
fn deck_transport(
    player: &HostAudioSnapshot,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let session = player.session;
    let at = player.current_time;
    let playing = player.is_playing;
    let (toggle_app, top_app) = (app.clone(), app.clone());
    let (toggle_target, top_target) = (target.clone(), target.clone());
    div()
        .flex()
        .gap(px(4.))
        .child(
            float::btn(if playing { "Pause" } else { "Play" }, "virtual-play")
                .id("virtual-play")
                .on_click(move |_, _, cx| {
                    toggle_app.update(cx, |this, cx| {
                        let transport = if playing {
                            Transport::Pause
                        } else {
                            Transport::PlayFrom(at)
                        };
                        this.virtual_transport(&toggle_target, session, transport, cx);
                    });
                })
                .agent_node(Role::Button, if playing { "Pause" } else { "Play" }),
        )
        .child(
            float::btn("From the top", "virtual-top")
                .id("virtual-top")
                .on_click(move |_, _, cx| {
                    top_app.update(cx, |this, cx| {
                        let transport = if playing {
                            Transport::PlayFrom(0.)
                        } else {
                            Transport::SeekTo(0.)
                        };
                        this.virtual_transport(&top_target, session, transport, cx);
                    });
                })
                .agent_node(Role::Button, "From the top"),
        )
}

/// The field a name is typed in, in place of the name. Presses in it stay
/// there, so what it sits on does not take them.
fn draft_field(draft: &Draft, label: &'static str, width: gpui::Length) -> impl IntoElement {
    float::field()
        .id(label)
        .w(width)
        .key_context(text_input::DRAFT_CONTEXT)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(div().w_full().child(draft.field.clone()))
        .agent_node(Role::Input, label)
}

/// A binding: its input, its mode (click to cycle it), invert, remove.
fn binding_chip(binding: &Binding, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let label = address_label(&binding.address);
    let on = |change: BindingChange| {
        let (app, target, binding) = (app.clone(), target.clone(), binding.clone());
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                this.change_binding(&target, &binding, change, cx)
            });
        }
    };
    let id = &binding.id;
    let word = mode_word(binding.mode);
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(10.))
        .pr(px(4.))
        .py(px(4.))
        .rounded(px(luma_ui::radius::ROW))
        .bg(glass::scrim(0.18))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.85))
                .child(label.clone()),
        )
        .child(
            float::chip()
                .id(SharedString::from(format!("binding-mode-{id}")))
                .child(word)
                .on_click(on(BindingChange::NextMode))
                .agent_node(Role::Button, format!("{label} mode {word}")),
        )
        .child(
            float::chip()
                .id(SharedString::from(format!("binding-invert-{id}")))
                .when(binding.invert, |chip| {
                    chip.bg(glass::card_selected_bg())
                        .shadow(glass::card_selected_shadows())
                })
                .when(!binding.invert, |chip| {
                    chip.text_color(ladder::foreground_alpha(0.45))
                })
                .child("Invert")
                .on_click(on(BindingChange::Invert))
                .agent_node(Role::Toggle, format!("{label} invert"))
                .agent_focused(binding.invert),
        )
        .child(
            float::chip()
                .id(SharedString::from(format!("binding-remove-{id}")))
                .text_color(Hsla::from(ladder::danger()))
                .child("Remove")
                .on_click(on(BindingChange::Remove))
                .agent_node(Role::Button, format!("Remove {label}")),
        )
}
