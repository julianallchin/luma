//! The composer's model picker, after Zeron's compact picker (`zeronsh/zeron`
//! at 68c7ea2, `crates/ui/src/pickers/compact.rs`): a card that opens on its
//! panel — the reasoning level as the title, the model's name under it (a
//! press there opens the models), the level's slider below — and turns to a
//! page of every service's models: back, a tab per service that jumps to its
//! models, a search, one list. A pick turns back to the panel; Escape turns
//! back from the models, and closes the panel.
//!
//! The card is a view of its own, mounted by the shell beside the chat panel
//! rather than inside it, so scrolling the list or typing in its search
//! redraws the card alone: a view nested in the panel would mark the panel
//! dirty with it. The panel draws only the chip ([`chip`]), and tells the
//! card what it shows of the panel ([`Seen`]) as that changes.
use crate::{Agent, AgentChat};
use gpui::{
    div, prelude::*, px, rgb, AnyElement, Bounds, Entity, FocusHandle, Focusable, Pixels,
    ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, WeakEntity,
};
use gpui_component::{tooltip::Tooltip, Icon};
use luma_lib::agent::{
    engine::catalog::{ModelChoice, Selection, Service},
    model::MODELS,
};
use luma_ui::{
    float::{self, RowState},
    glass,
    icons::IconName,
    ladder, motion,
    node::{AgentNode, Instrument, Role},
    radius,
    text_input::{self, TextInput},
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Instant,
};

gpui::actions!(model_picker, [DismissPicker]);

/// The card's width, as Zeron's.
const CARD_WIDTH: f32 = 256.;
/// The card's inset: rows and buttons sit this far inside its edge, so their
/// corners ([`radius::ROW`]) are concentric with its own ([`radius::CARD`]).
const INSET: f32 = 4.;
/// The panel's text and the slider's rail start this far inside the inset,
/// on one edge with the rows' text.
const TEXT_INSET: f32 = 8.;
const SLIDER_WIDTH: f32 = CARD_WIDTH - 2. * (INSET + TEXT_INSET);
/// The slider's knob, and the thinner track it rides.
const KNOB: f32 = 24.;
const TRACK: f32 = 18.;
const TRACK_TOP: f32 = (KNOB - TRACK) / 2.;
/// The panel's title block: the level over the model, or the model alone.
const TITLE_HEIGHT: f32 = 48.;
const TITLE_HEIGHT_SINGLE: f32 = 36.;
/// A model row, and the gap under it: one pitch, so the list can be
/// virtualized and the scroll offset names the top row.
const ROW_HEIGHT: f32 = 32.;
const ROW_PITCH: f32 = ROW_HEIGHT + 2.;
/// Rows the list shows before it scrolls.
const LIST_ROWS: usize = 7;
/// The back button and the tabs, and the search under them.
const BAR_HEIGHT: f32 = ROW_HEIGHT + 2. * INSET;
const SEARCH_HEIGHT: f32 = 40.;
/// Type: the level as the title, the model's name and the rows under it.
const TITLE_SIZE: f32 = 15.;
const NAME_SIZE: f32 = 13.;
const ROW_SIZE: f32 = 13.;
const SEARCH_SIZE: f32 = 13.5;
const SEARCH_PLACEHOLDER: &str = "Search models…";

/// What the card shows of the chat panel: the thread's selection, and
/// whether a change is taken now.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Seen {
    pub selection: Option<Selection>,
    pub streaming: bool,
    pub saving: bool,
}

impl Seen {
    /// A turn running or a choice saving: the chip and the slider rest.
    fn locked(&self) -> bool {
        self.streaming || self.saving
    }
}

/// One row of the models page: a service's model, and whether another
/// service lists a model of the same name (its row then names its service).
#[derive(Clone)]
struct Row {
    service: Service,
    model: ModelChoice,
    shared_name: bool,
}

impl Row {
    fn is(&self, selection: &Selection) -> bool {
        self.service == selection.service && self.model.matches(&selection.model)
    }

    fn key(&self) -> String {
        format!(
            "model-{:?}-{}",
            self.service,
            self.model.id.as_deref().unwrap_or("default")
        )
    }
}

pub struct ModelPicker {
    chat: WeakEntity<AgentChat>,
    agent: Agent,
    pub(crate) seen: Seen,
    focus: FocusHandle,
    pub(crate) open: bool,
    /// On the models page rather than the panel.
    expanded: bool,
    /// The slider's level while it is dragged or stepped, ahead of the save.
    preview: Option<f32>,
    dragging: bool,
    thumb: RefCell<Tween>,
    height: RefCell<Tween>,
    catalogs: HashMap<Service, Result<Vec<ModelChoice>, String>>,
    loading: HashSet<Service>,
    /// The services whose list saved on disk has been looked for, found or
    /// not: past that, the chip's label changes only when a fresh read lands.
    looked_up: HashSet<Service>,
    search: Entity<TextInput>,
    _search: Subscription,
    /// Every service's models, in [`Service::ALL`] order, filtered by the
    /// search.
    list: float::Picker<Row>,
    scroll: UniformListScrollHandle,
    /// The card's natural height, written as it paints. The card's height is
    /// tweened, so it needs a number, and the height depends on how many rows
    /// match — a fact only layout knows.
    content_height: Rc<Cell<Option<f32>>>,
    /// The chip's box in window space, written as the panel paints it: what
    /// the card hangs from.
    anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    renders: u32,
}

impl ModelPicker {
    pub(crate) fn new(agent: Agent, cx: &mut gpui::Context<AgentChat>) -> Entity<Self> {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            DismissPicker,
            Some("ModelEffortPicker"),
        )]);
        let chat = cx.weak_entity();
        cx.new(|cx| {
            let search = cx.new(|cx| {
                let mut field = TextInput::search(SEARCH_PLACEHOLDER, cx);
                field.set_text_size(SEARCH_SIZE, cx);
                field
            });
            let subscription = cx.subscribe(&search, |picker: &mut Self, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let query = field.read(cx).text().to_string();
                    picker.list.set_query(query);
                    picker.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
            });
            Self {
                chat,
                agent,
                seen: Seen::default(),
                focus: cx.focus_handle(),
                open: false,
                expanded: false,
                preview: None,
                dragging: false,
                thumb: RefCell::new(Tween::default()),
                height: RefCell::new(Tween::default()),
                catalogs: HashMap::new(),
                loading: HashSet::new(),
                looked_up: HashSet::new(),
                search,
                _search: subscription,
                list: float::Picker::new(matches),
                scroll: UniformListScrollHandle::new(),
                content_height: Rc::default(),
                anchor: Rc::default(),
                renders: 0,
            }
        })
    }

    // ---- what the chat panel asks of it ----

    /// Open or close the card from its chip.
    fn toggle(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if self.open {
            self.dismiss(window, cx);
            return;
        }
        let Some(service) = self.seen.selection.as_ref().map(|s| s.service) else {
            return;
        };
        if self.seen.locked() {
            return;
        }
        self.open = true;
        self.expanded = false;
        self.clear_search(cx);
        window.focus(&self.focus, cx);
        self.browse(service, cx);
        self.clamp_effort(cx);
        cx.notify();
    }

    /// Close the card, dropping whatever the slider was doing.
    pub(crate) fn close(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.open && self.preview.is_none() && !self.dragging {
            return;
        }
        self.open = false;
        self.preview = None;
        self.dragging = false;
        // A card that opens again starts at its own height, not this one's.
        self.height = RefCell::default();
        cx.notify();
    }

    /// The level the slider shows is the saved one again.
    pub(crate) fn settle_effort(&mut self, cx: &mut gpui::Context<Self>) {
        self.preview = None;
        cx.notify();
    }

    /// Read `service`'s models if needed. A gateway's list is read again
    /// every time — the models page asks each time it opens — and the rows
    /// already held, or saved on disk, show while it is on the wire, so the
    /// list is never blank while it refreshes. A CLI's list is read once per
    /// session: asking means starting the CLI.
    pub(crate) fn browse(&mut self, service: Service, cx: &mut gpui::Context<Self>) {
        let held = matches!(self.catalogs.get(&service), Some(Ok(_)));
        if (held && !service.lists_models()) || !self.loading.insert(service) {
            cx.notify();
            return;
        }
        if !held {
            // A previous error, cleared so the retry reads as loading.
            self.catalogs.remove(&service);
        }
        let saved = (!held).then(|| self.agent.cached_models(service));
        let pending = self.agent.models(service);
        cx.notify();
        cx.spawn(async move |this, cx| {
            // Applied before the fresh read, so the fresh read always wins.
            let saved = match saved {
                Some(saved) => saved.await,
                None => None,
            };
            this.update(cx, |this, cx| {
                let chip = this.chip_key();
                this.looked_up.insert(service);
                if let Some(models) = saved {
                    if !matches!(this.catalogs.get(&service), Some(Ok(_))) {
                        this.receive(service, Ok(models), cx);
                    }
                }
                this.changed(chip, cx);
            })
            .ok();
            let result = pending.await;
            this.update(cx, |this, cx| {
                let chip = this.chip_key();
                this.loading.remove(&service);
                this.receive(service, result, cx);
                this.changed(chip, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Whether the chip's label for `selection` is the one it shows until a
    /// fresh read of the service's list lands: a list is held, none is being
    /// read, or the one saved on disk has been looked for. Before then the
    /// chip shows a stand-in name of another width.
    pub(crate) fn label_settled(&self, selection: &Selection) -> bool {
        let service = &selection.service;
        self.catalogs.contains_key(service)
            || !self.loading.contains(service)
            || self.looked_up.contains(service)
    }

    // ---- its own state ----

    /// What the chip, drawn by the panel, reads of this card: the chosen
    /// model's name and whether it is settled.
    fn chip_key(&self) -> Option<(String, bool)> {
        let selection = self.seen.selection.as_ref()?;
        Some((self.label(selection), self.label_settled(selection)))
    }

    /// Redraw after a list landed; the panel too when that changed its chip
    /// (`before`, its [`Self::chip_key`] ahead of the change).
    fn changed(&self, before: Option<(String, bool)>, cx: &mut gpui::Context<Self>) {
        cx.notify();
        if self.chip_key() != before {
            if let Some(chat) = self.chat.upgrade() {
                chat.update(cx, |_, cx| cx.notify());
            }
        }
    }

    /// Take a list read for `service`. A failed refresh keeps the rows
    /// already shown: a stale list is more use than an error in its place.
    fn receive(
        &mut self,
        service: Service,
        result: Result<Vec<ModelChoice>, String>,
        cx: &mut gpui::Context<Self>,
    ) {
        if result.is_ok() || !matches!(self.catalogs.get(&service), Some(Ok(_))) {
            self.catalogs.insert(service, result);
        }
        self.sync(cx);
        if self.seen.selection.as_ref().map(|s| s.service) == Some(service) {
            self.clamp_effort(cx);
        }
    }

    /// Rebuild the rows from the lists held, keeping the cursor on the row it
    /// was on: a list that lands mid-browse must not move it.
    fn sync(&mut self, cx: &gpui::App) {
        let on = self.list.current().map(Row::key);
        // A cursor still where the models opened it — on the chosen model, or
        // on nothing yet — follows the chosen model as lists land above it.
        let anchored = self.list.query().trim().is_empty()
            && match (self.list.current(), &self.seen.selection) {
                (None, _) => true,
                (Some(row), Some(selection)) => row.is(selection),
                (Some(_), None) => false,
            };
        let mut names: HashMap<&str, HashSet<Service>> = HashMap::new();
        for (service, models) in self.held() {
            for model in models {
                names.entry(&model.label).or_default().insert(service);
            }
        }
        let names = &names;
        let rows = self
            .held()
            .flat_map(|(service, models)| {
                models.iter().map(move |model| Row {
                    service,
                    model: model.clone(),
                    shared_name: names.get(model.label.as_str()).is_some_and(|s| s.len() > 1),
                })
            })
            .collect();
        self.list.set_rows(rows);
        self.list.set_query(self.search.read(cx).text().to_string());
        if anchored && self.expanded {
            self.seek();
        } else if let Some(on) = on {
            self.list.seek(|row| row.key() == on);
        }
    }

    /// The lists held, in [`Service::ALL`] order.
    fn held(&self) -> impl Iterator<Item = (Service, &Vec<ModelChoice>)> + '_ {
        Service::ALL
            .into_iter()
            .filter_map(|service| match self.catalogs.get(&service) {
                Some(Ok(models)) => Some((service, models)),
                _ => None,
            })
    }

    fn clear_search(&self, cx: &mut gpui::App) {
        self.search.update(cx, |field, cx| field.set_text("", cx));
    }

    /// Close the card and give the keys back to the composer.
    fn dismiss(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        self.close(cx);
        if let Some(chat) = self.chat.upgrade() {
            window.focus(&chat.read(cx).composer.focus_handle(cx), cx);
        }
    }

    /// Escape: the models page turns back to the panel; the panel closes.
    fn escape(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if self.expanded {
            self.fold(window, cx);
        } else {
            self.dismiss(window, cx);
        }
    }

    /// Turn to the models page, opened on the chosen model: its service's
    /// models at the top when it is near their start, else centred on it.
    fn open_out(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if self.seen.selection.is_none() {
            return;
        }
        self.expanded = true;
        for service in Service::ALL {
            self.browse(service, cx);
        }
        self.sync(cx);
        self.seek();
        window.focus(&self.search.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    /// Back to the panel, the search cleared.
    fn fold(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        self.expanded = false;
        self.clear_search(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Put the cursor on the chosen model and scroll to it (Zeron's
    /// `show_compact_list`): its service's first rows at the top, one row of
    /// the service before it showing above, when the model is near its start;
    /// else the model in the middle.
    fn seek(&mut self) {
        let Some(selection) = self.seen.selection.clone() else {
            return;
        };
        let Some(at) = self.list.seek(|row| row.is(&selection)) else {
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            return;
        };
        let start = self
            .groups()
            .into_iter()
            .rev()
            .find(|(_, start)| *start <= at)
            .map_or(0, |(_, start)| start);
        if at - start < LIST_ROWS - 2 {
            self.scroll.scroll_to_item_strict_with_offset(
                start,
                ScrollStrategy::Top,
                usize::from(start > 0),
            );
        } else {
            self.scroll
                .scroll_to_item_strict(at, ScrollStrategy::Center);
        }
    }

    /// Where each service's rows start in the unsearched list.
    fn groups(&self) -> Vec<(Service, usize)> {
        if !self.list.query().trim().is_empty() {
            return Vec::new();
        }
        let mut groups: Vec<(Service, usize)> = Vec::new();
        for (at, row) in self.list.shown().enumerate() {
            if groups.last().is_none_or(|(last, _)| *last != row.service) {
                groups.push((row.service, at));
            }
        }
        groups
    }

    /// A tab: the list jumps to `start`, its service's first row.
    fn jump(&mut self, start: usize, cx: &mut gpui::Context<Self>) {
        let key = self.list.get(start).map(Row::key);
        self.list.seek(|row| Some(row.key()) == key);
        self.scroll
            .scroll_to_item_strict(start, ScrollStrategy::Top);
        cx.notify();
    }

    /// Move the list's keyboard cursor.
    fn step(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        if let Some(at) = self.list.step(delta) {
            self.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// Save `row`'s model, at the level asked for when it offers it, and turn
    /// back to the panel.
    fn pick(&mut self, row: &Row, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let Some(selection) = self.seen.selection.as_ref() else {
            return;
        };
        let chosen = row.model.selection(row.service, selection);
        if let Some(chat) = self.chat.upgrade() {
            chat.update(cx, |chat, cx| chat.select_model(chosen, cx));
        }
        self.fold(window, cx);
    }

    /// Pick the model under the list's keyboard cursor.
    fn choose_current(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if let Some(row) = self.list.current().cloned() {
            self.pick(&row, window, cx);
        }
    }

    /// A model runs at one of its own levels: a saved level it does not offer
    /// (or none) becomes its default, as Zeron's picker clamps a model moved
    /// to. Once the model's list is held.
    fn clamp_effort(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(selection) = self.seen.selection.clone() else {
            return;
        };
        if !self.open || self.seen.locked() {
            return;
        }
        let Some(model) = self.model(&selection) else {
            return;
        };
        let effort = model.effort(selection.effort.as_deref());
        if effort != selection.effort {
            self.save(
                Selection {
                    effort,
                    ..selection
                },
                cx,
            );
        }
    }

    fn save(&self, selection: Selection, cx: &mut gpui::Context<Self>) {
        if let Some(chat) = self.chat.upgrade() {
            chat.update(cx, |chat, cx| chat.select_model(selection, cx));
        }
    }

    /// Save the level the slider shows.
    fn commit_effort(&mut self, cx: &mut gpui::Context<Self>) {
        self.dragging = false;
        let (Some(fraction), Some(mut selection)) = (self.preview, self.seen.selection.clone())
        else {
            return;
        };
        let levels = self.efforts(&selection);
        let Some(level) = levels.get(effort_index(fraction, levels.len())) else {
            return;
        };
        selection.effort = Some(level.clone());
        self.save(selection, cx);
        cx.notify();
    }

    /// ← → Home End on the panel move the reasoning a level.
    fn step_effort(&mut self, key: &str, cx: &mut gpui::Context<Self>) {
        // A level picked while the last one saves is queued behind it.
        if self.expanded || self.seen.streaming {
            return;
        }
        let Some(selection) = &self.seen.selection else {
            return;
        };
        let levels = self.efforts(selection);
        if levels.len() < 2 {
            return;
        }
        let index = self
            .preview
            .map(|fraction| effort_index(fraction, levels.len()))
            .unwrap_or_else(|| self.level_index(selection, &levels));
        let next = match key {
            "home" => 0,
            "end" => levels.len() - 1,
            "left" => index.saturating_sub(1),
            _ => (index + 1).min(levels.len() - 1),
        };
        self.preview = Some(next as f32 / (levels.len() - 1) as f32);
        self.commit_effort(cx);
    }

    fn model(&self, selection: &Selection) -> Option<&ModelChoice> {
        self.catalogs
            .get(&selection.service)?
            .as_ref()
            .ok()?
            .iter()
            .find(|model| model.matches(&selection.model))
    }

    /// The chosen model's levels, lowest first: the slider's steps.
    fn efforts(&self, selection: &Selection) -> Vec<String> {
        self.model(selection)
            .map(|model| model.effort_levels.clone())
            .unwrap_or_default()
    }

    /// Where the model's level sits among `levels`: the saved one, or the one
    /// it runs at when the saved one is not among them.
    fn level_index(&self, selection: &Selection, levels: &[String]) -> usize {
        let effort = self
            .model(selection)
            .and_then(|model| model.effort(selection.effort.as_deref()));
        levels
            .iter()
            .position(|level| Some(level) == effort.as_ref())
            .unwrap_or(0)
    }

    fn label(&self, selection: &Selection) -> String {
        self.model(selection)
            .map(|model| model.label.clone())
            .or_else(|| {
                MODELS
                    .iter()
                    .find(|model| Some(model.key) == selection.model.as_deref())
                    .map(|model| model.display.into())
            })
            .unwrap_or_else(|| selection.model.clone().unwrap_or_else(|| "Default".into()))
    }
}

/// The chip in the composer's row, as Zeron's: the model's logo, its name,
/// and its level muted after it. A chip opens a card; it carries no
/// chevron. Drawn by the chat panel; its box is where the card hangs.
pub(crate) fn chip(
    picker: &Entity<ModelPicker>,
    selection: &Selection,
    disabled: bool,
    cx: &gpui::App,
) -> AnyElement {
    let state = picker.read(cx);
    let label = state.label(selection);
    let toggle = picker.clone();
    let moved = picker.clone();
    let anchor = Rc::clone(&state.anchor);
    float::chip()
        .id("thread-model")
        .relative()
        .max_w(px(220.))
        .child(model_logo(selection.service, selection.model.as_deref()))
        .child(div().min_w_0().truncate().child(label.clone()))
        .children(selection.effort.as_deref().map(|effort| {
            div()
                .flex_none()
                .text_color(ladder::muted_foreground())
                .child(effort_label(effort))
        }))
        .child(
            gpui::canvas(
                move |bounds, _, cx| {
                    // The card hangs from here; one already open follows
                    // the chip when the row moves it.
                    if anchor.get() != Some(bounds) {
                        anchor.set(Some(bounds));
                        if moved.read(cx).open {
                            cx.notify(moved.entity_id());
                        }
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .when(disabled, |el| el.opacity(0.5))
        .on_click(move |_, window, cx| toggle.update(cx, |picker, cx| picker.toggle(window, cx)))
        .agent_node(
            Role::Select,
            format!("{} · {label}", selection.service.label()),
        )
        .agent_disabled(disabled)
        .into_any_element()
}

impl Render for ModelPicker {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        self.renders += 1;
        let root = div()
            .size_0()
            .child(luma_ui::node::render_count("Picker", self.renders));
        let (true, Some(selection), Some(anchor)) =
            (self.open, self.seen.selection.clone(), self.anchor.get())
        else {
            return root;
        };
        let disabled = self.seen.locked();
        let this = cx.entity();
        let dismiss = this.clone();
        let keys = this.clone();
        let escape = this.clone();
        // Until the card has painted once, a guess; the tween turns toward
        // the measured height from wherever the guess got to.
        let target_height = self.content_height.get().unwrap_or(100.);
        let height = self.height.borrow_mut().sample(
            target_height,
            motion::reduced_motion(cx),
            &motion::COLLAPSE,
            window,
        );
        let expanded = self.expanded;
        let content = self.card(&selection, disabled, window, cx);
        root.child(float::anchored_above_box(
            "thread-model-menu",
            anchor,
            float::Dismiss::on_press_out(move |window, cx| {
                dismiss.update(cx, |picker, cx| picker.dismiss(window, cx));
            }),
            div()
                .id("model-picker-focus")
                .flex()
                .flex_col()
                .w(px(CARD_WIDTH))
                .track_focus(&self.focus)
                .key_context("ModelEffortPicker")
                .on_action(move |_: &DismissPicker, window, cx| {
                    escape.update(cx, |picker, cx| picker.escape(window, cx));
                })
                .on_key_down(move |event, window, cx| {
                    let keystroke = &event.keystroke;
                    keys.update(cx, |picker, cx| {
                        match keystroke.key.as_str() {
                            "left" | "right" | "home" | "end" if !expanded => {
                                picker.step_effort(&keystroke.key, cx)
                            }
                            "up" if expanded => picker.step(-1, cx),
                            "down" if expanded => picker.step(1, cx),
                            "enter" if expanded => picker.choose_current(window, cx),
                            // On the panel, ↑ ↓ open the models a row from
                            // the chosen one (Zeron's), Enter opens them on
                            // it, and typing opens them already searching.
                            "up" | "down" => {
                                picker.open_out(window, cx);
                                picker.step(if keystroke.key == "up" { -1 } else { 1 }, cx);
                            }
                            "enter" => picker.open_out(window, cx),
                            _ => match typed(keystroke).filter(|_| !expanded) {
                                Some(text) => {
                                    picker.open_out(window, cx);
                                    picker.search.update(cx, |field, cx| {
                                        let typed = format!("{}{text}", field.text());
                                        field.set_text(typed, cx);
                                    });
                                }
                                None => return,
                            },
                        }
                        cx.stop_propagation();
                    });
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .h(px(height))
                        .overflow_hidden()
                        // Bottom-anchored, so as the card grows toward the
                        // models its foot by the chip stays put.
                        .justify_end()
                        .child(motion::fade_quick(
                            if expanded {
                                "picker-models"
                            } else {
                                "picker-panel"
                            },
                            div().flex_none().child(content),
                        )),
                )
                .into_any_element(),
        ))
    }
}

impl ModelPicker {
    /// The card's page: the models, or the panel.
    fn card(
        &mut self,
        selection: &Selection,
        disabled: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let measured = Rc::clone(&self.content_height);
        div()
            .relative()
            .flex()
            .flex_col()
            .w(px(CARD_WIDTH))
            .text_size(px(13.))
            .text_color(ladder::foreground())
            .map(|el| {
                if self.expanded {
                    el.child(self.bar(cx))
                        .child(self.search_row(cx))
                        .children(self.failures(cx))
                        .child(self.list_body(selection, cx))
                } else {
                    el.child(self.panel(selection, disabled, window, cx))
                }
            })
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        let height = f32::from(bounds.size.height);
                        if measured.get() != Some(height) {
                            measured.set(Some(height));
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    /// The panel: the level as the title over the model's name (a press opens
    /// the models), and the level's slider; a model with no levels titles the
    /// panel with its name alone.
    fn panel(
        &self,
        selection: &Selection,
        disabled: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let levels = self.efforts(selection);
        let saved = self.level_index(selection, &levels);
        let steps = levels.len().saturating_sub(1).max(1) as f32;
        let fraction = self.preview.unwrap_or(saved as f32 / steps);
        let level = levels
            .get(effort_index(fraction, levels.len()))
            .map(|level| effort_label(level));
        let name = self.label(selection);
        let open = cx.entity();
        let title = div()
            .id("choose-model")
            .flex_1()
            .min_w_0()
            .h_full()
            .px(px(TEXT_INSET))
            .rounded(px(radius::ROW))
            .flex()
            .flex_col()
            .items_start()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(glass::card_cursor_bg()))
            .children(level.clone().map(|level| {
                div()
                    .text_size(px(TITLE_SIZE))
                    .line_height(px(18.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(level.clone())
                    .agent_node(Role::Text, format!("Effort · {level}"))
            }))
            .child(
                div()
                    .max_w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .map(|el| match level {
                        Some(_) => el
                            .text_size(px(NAME_SIZE))
                            .line_height(px(16.))
                            .text_color(ladder::muted_foreground()),
                        // The name is the whole title.
                        None => el
                            .text_size(px(TITLE_SIZE))
                            .line_height(px(18.))
                            .font_weight(gpui::FontWeight::SEMIBOLD),
                    })
                    .child(div().min_w_0().truncate().child(name))
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(10.))
                            .flex_none()
                            .text_color(ladder::muted_foreground()),
                    ),
            )
            .on_click(move |_, window, cx| {
                open.update(cx, |picker, cx| picker.open_out(window, cx))
            })
            .agent_node(Role::Button, "Choose model");
        div()
            .flex()
            .flex_col()
            .p(px(INSET))
            .child(
                div()
                    .flex()
                    .h(px(if levels.is_empty() {
                        TITLE_HEIGHT_SINGLE
                    } else {
                        TITLE_HEIGHT
                    }))
                    .child(title),
            )
            .when(!levels.is_empty(), |el| {
                let shown = self.thumb.borrow_mut().sample(
                    fraction,
                    motion::reduced_motion(cx),
                    &motion::TAB_SLIDE,
                    window,
                );
                el.child(
                    div()
                        .px(px(TEXT_INSET))
                        .pt(px(8.))
                        .pb(px(10.))
                        .child(slider(shown, levels.len(), disabled, &cx.entity())),
                )
            })
            .into_any_element()
    }

    /// The models page's top row: back to the panel, then a tab per service
    /// whose models are listed, each jumping the list to them. The service at
    /// the top of the scroll lights its tab. No tabs while searching: the
    /// search spans every service.
    fn bar(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let groups = self.groups();
        // Rows share one pitch, so the scroll offset names the top row.
        let scrolled = -f32::from(self.scroll.0.borrow().base_handle.offset().y);
        let top = (scrolled / ROW_PITCH).round().max(0.) as usize;
        let viewed = groups
            .iter()
            .rev()
            .find(|(_, start)| *start <= top)
            .map(|(service, _)| *service);
        let back = cx.entity();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(BAR_HEIGHT))
            .px(px(INSET))
            .child(
                square("models-back")
                    .hover(|style| style.bg(glass::card_cursor_bg()))
                    .child(
                        Icon::new(IconName::ChevronLeft)
                            .size(px(14.))
                            .text_color(ladder::muted_foreground()),
                    )
                    .tooltip(|window, cx| Tooltip::new("Back · Esc").build(window, cx))
                    .on_click(move |_, window, cx| {
                        back.update(cx, |picker, cx| picker.fold(window, cx))
                    })
                    .agent_node(Role::Button, "Back"),
            )
            .children(groups.into_iter().map(|(service, start)| {
                let shown = viewed == Some(service);
                let target = cx.entity();
                let name = SharedString::from(service.label());
                square(SharedString::from(format!("models-tab-{service:?}")))
                    .when(shown, |el| el.bg(glass::ink(0.08)))
                    .when(!shown, |el| {
                        el.opacity(0.7)
                            .hover(|style| style.bg(glass::card_cursor_bg()).opacity(1.))
                    })
                    .child(service_logo(service).text_color(if shown {
                        ladder::foreground()
                    } else {
                        ladder::muted_foreground()
                    }))
                    .tooltip({
                        let name = name.clone();
                        move |window, cx| Tooltip::new(name.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| {
                        target.update(cx, |picker, cx| picker.jump(start, cx))
                    })
                    .agent_node(Role::Button, name)
            }))
            .into_any_element()
    }

    /// The search: a glass, a borderless field, a hairline the width of the
    /// card under it.
    fn search_row(&self, cx: &gpui::App) -> AnyElement {
        let query = self.search.read(cx).text().to_string();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(SEARCH_HEIGHT))
            .px(px(INSET + TEXT_INSET))
            .border_b_1()
            .border_color(glass::hairline(0.08))
            .child(
                Icon::new(IconName::Search)
                    .size(px(14.))
                    .flex_none()
                    .text_color(ladder::muted_foreground()),
            )
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .agent_node(
                Role::Input,
                if query.is_empty() {
                    SEARCH_PLACEHOLDER.to_string()
                } else {
                    query
                },
            )
            .into_any_element()
    }

    /// A service whose list could not be read and has no rows to show: why,
    /// and a retry.
    fn failures(&self, cx: &gpui::Context<Self>) -> Vec<AnyElement> {
        Service::ALL
            .into_iter()
            .filter_map(|service| match self.catalogs.get(&service) {
                Some(Err(error)) => Some((service, error.clone())),
                _ => None,
            })
            .map(|(service, error)| {
                let target = cx.entity();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(INSET + TEXT_INSET))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(glass::hairline(0.08))
                    .child(service_logo(service).text_color(ladder::muted_foreground()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(ladder::muted_foreground())
                            .child(error.clone()),
                    )
                    .child(
                        luma_ui::button("Retry", luma_ui::Enabled::Yes)
                            .id(SharedString::from(format!("retry-{service:?}")))
                            .on_click(move |_, _, cx| {
                                target.update(cx, |picker, cx| picker.browse(service, cx))
                            })
                            .agent_node(Role::Button, "Retry models"),
                    )
                    .agent_node(Role::Text, format!("{}: {error}", service.label()))
                    .into_any_element()
            })
            .collect()
    }

    /// The rows, virtualized: a gateway lists hundreds. While nothing is
    /// held yet, skeleton rows; a search that finds nothing says so.
    fn list_body(&self, selection: &Selection, cx: &mut gpui::Context<Self>) -> AnyElement {
        let count = self.list.shown().len();
        let height = count.clamp(1, LIST_ROWS) as f32 * ROW_PITCH + 2. * INSET;
        let host = div()
            .relative()
            .flex_none()
            .py(px(INSET))
            .bg(glass::ink(0.02));
        if count > 0 {
            let selection = selection.clone();
            return host
                .h(px(height))
                .child(
                    gpui::uniform_list(
                        "model-list",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .filter_map(|index| this.row(index, &selection, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full()
                    .px(px(INSET)),
                )
                .into_any_element();
        }
        let query = self.list.query().trim().to_string();
        let empty_height = 4. * ROW_PITCH + 2. * INSET;
        let body = if !query.is_empty() {
            empty("No models found", format!("Nothing matches “{query}”"))
        } else if self.loading.is_empty() {
            empty("No models", "No service has any to offer".into())
        } else {
            float::skeleton_rows(4, cx.entity_id(), cx)
                .px(px(INSET))
                .agent_node(Role::Text, "Loading models")
                .into_any_element()
        };
        host.h(px(empty_height)).child(body).into_any_element()
    }

    /// One model row: the lab's logo, the name (and its service, when another
    /// service lists the same name), a check on the chosen one.
    fn row(
        &self,
        index: usize,
        selection: &Selection,
        cx: &mut gpui::Context<Self>,
    ) -> Option<AnyElement> {
        let row = self.list.get(index)?.clone();
        let chosen = row.is(selection);
        let key = row.key();
        let wire = row
            .model
            .resolved_model
            .as_deref()
            .or(row.model.id.as_deref());
        let detail = detail(&row.model);
        let tip = match &detail {
            Some(detail) => format!("{} · {}\n{detail}", row.model.label, row.service.label()),
            None => format!("{} · {}", row.model.label, row.service.label()),
        };
        let name = format!("{} · {}", row.model.label, row.service.label());
        let target = cx.entity();
        let plate = float::menu_row(
            RowState::of(chosen, index == self.list.cursor()),
            key.clone(),
        )
        .id(SharedString::from(key))
        .h(px(ROW_HEIGHT))
        .py_0()
        .pl(px(TEXT_INSET))
        .pr(px(TEXT_INSET))
        .gap(px(8.))
        .text_size(px(ROW_SIZE))
        .child(model_logo(row.service, wire))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(6.))
                .child(
                    div()
                        .flex_none()
                        .max_w_full()
                        .truncate()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(row.model.label.clone()),
                )
                .when(row.shared_name, |el| {
                    el.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(ladder::muted_foreground())
                            .child(row.service.label()),
                    )
                }),
        )
        .when(chosen, |el| {
            el.child(
                Icon::new(IconName::Check)
                    .size(px(14.))
                    .flex_none()
                    .text_color(ladder::primary()),
            )
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(move |_, window, cx| {
            target.update(cx, |picker, cx| picker.pick(&row, window, cx))
        })
        .agent_node(Role::Button, name);
        Some(
            div()
                .h(px(ROW_PITCH))
                .pb(px(2.))
                .child(plate)
                .into_any_element(),
        )
    }
}

/// A square the size of a row, for the models page's back button and tabs.
fn square(id: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(ROW_HEIGHT))
        .rounded(px(radius::ROW))
        .cursor_pointer()
}

/// The reasoning slider, as it was before the picker followed Zeron: a thin
/// track, a notch per level, a round white knob, and the fill in the effort's
/// colour (blue, turning to a violet gradient at the top levels).
fn slider(fraction: f32, count: usize, disabled: bool, this: &Entity<ModelPicker>) -> AnyElement {
    let travel = SLIDER_WIDTH - KNOB;
    let left = fraction * travel;
    let (bounds, probe) = luma_ui::arg::bounds_probe();
    let down = this.clone();
    let up = this.clone();
    let outside = this.clone();
    let moving = this.clone();
    let fill = if fraction > 0.75 {
        gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(rgb(0x4244df), 0.),
            gpui::linear_color_stop(rgb(0xab79ef), 1.),
        )
    } else {
        gpui::Background::from(effort_color(fraction))
    };
    div()
        .id("effort-slider")
        .relative()
        .w(px(SLIDER_WIDTH))
        .h(px(KNOB))
        .cursor_pointer()
        .child(probe)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(TRACK_TOP))
                .h(px(TRACK))
                .rounded_full()
                .bg(ladder::foreground_alpha(0.12)),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(TRACK_TOP))
                // At least the track's height, so the fill's rounded end is
                // the track's own and stays inside the knob. Narrower, its
                // corners tightened and peeked out past the knob's curve at
                // the lowest level.
                .w(px((left + KNOB / 2.).max(TRACK)))
                .h(px(TRACK))
                .rounded_full()
                .bg(fill),
        )
        .children((0..count).map(|index| {
            div()
                .absolute()
                .left(px(
                    KNOB / 2. + travel * index as f32 / (count - 1) as f32 - 2.
                ))
                .top(px(KNOB / 2. - 2.))
                .size(px(4.))
                .rounded_full()
                .bg(ladder::foreground_alpha(0.3))
        }))
        .child(
            div()
                .absolute()
                .left(px(left))
                .top_0()
                .size(px(KNOB))
                .rounded_full()
                .bg(rgb(0xffffff)),
        )
        .child(
            gpui::canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let moving = moving.clone();
                    window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Bubble {
                            moving.update(cx, |picker, cx| {
                                if picker.dragging {
                                    picker.preview = Some(snapped_fraction(
                                        f32::from(event.position.x - bounds.left()),
                                        f32::from(bounds.size.width),
                                        count,
                                    ));
                                    cx.notify();
                                }
                            });
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
            if disabled {
                return;
            }
            if let Some(bounds) = bounds.get() {
                down.update(cx, |picker, cx| {
                    picker.dragging = true;
                    picker.preview = Some(snapped_fraction(
                        f32::from(event.position.x - bounds.left()),
                        f32::from(bounds.size.width),
                        count,
                    ));
                    cx.notify();
                });
            }
            cx.stop_propagation();
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
            up.update(cx, |picker, cx| picker.commit_effort(cx))
        })
        .on_mouse_up_out(gpui::MouseButton::Left, move |_, _, cx| {
            outside.update(cx, |picker, cx| picker.commit_effort(cx))
        })
        .agent_node(Role::Slider, "Reasoning effort")
        .agent_disabled(disabled)
        .into_any_element()
}

/// The level a pointer `x` across the slider's `width` asks for, as the
/// fraction that level sits at: the notches and the pointer share the knob's
/// centre inset, so a press on a notch takes that notch.
fn snapped_fraction(x: f32, width: f32, count: usize) -> f32 {
    let fraction = ((x - KNOB / 2.) / (width - KNOB).max(1.)).clamp(0., 1.);
    effort_index(fraction, count) as f32 / count.saturating_sub(1).max(1) as f32
}

fn effort_color(fraction: f32) -> gpui::Hsla {
    if fraction > 0.75 {
        rgb(0xb57bff).into()
    } else {
        rgb(0x3784fb).into()
    }
}

/// The text a key types, when it types any: no command or control chord, and
/// not a key that names itself (`space` types, `tab` does not).
fn typed(keystroke: &gpui::Keystroke) -> Option<&str> {
    if keystroke.modifiers.platform || keystroke.modifiers.control {
        return None;
    }
    keystroke
        .key_char
        .as_deref()
        .filter(|text| !text.is_empty() && text.chars().all(|c| !c.is_control()))
}

/// What an empty list shows: what is missing, and where.
fn empty(title: &str, hint: String) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap(px(4.))
        .px(px(12.))
        .text_center()
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(title.to_string())
                .agent_node(Role::Text, title.to_string()),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(ladder::muted_foreground())
                .child(hint.clone())
                .agent_node(Role::Text, hint),
        )
        .into_any_element()
}

/// Whether `row` matches every word of `query`, by name, wire id or service.
fn matches(row: &Row, query: &str) -> bool {
    let haystack = format!(
        "{} {} {}",
        row.model.label,
        row.model.resolved_model.as_deref().unwrap_or_default(),
        row.service.label()
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

/// A row's tooltip line: its window and its price per million tokens, as
/// far as its list says.
fn detail(model: &ModelChoice) -> Option<String> {
    let window = model
        .context_window
        .map(|window| format!("{} context", tokens(window)));
    let price = model.price.map(|(input, output)| {
        if input == 0. && output == 0. {
            "Free".to_string()
        } else {
            format!("{} in · {} out", dollars(input), dollars(output))
        }
    });
    match (window, price) {
        (Some(window), Some(price)) => Some(format!("{window} · {price}")),
        (window, price) => window.or(price),
    }
}

/// `1048576` → `1M`, `262144` → `262K`.
fn tokens(count: u32) -> String {
    let count = f64::from(count);
    if count >= 1_000_000. {
        format!("{}M", trimmed(count / 1_000_000., 1))
    } else {
        format!("{}K", (count / 1_000.).round())
    }
}

/// USD per million tokens: cents above a dollar, two significant figures
/// below — `$1.89`, `$0.15`, `$0.037`.
fn dollars(per_million: f64) -> String {
    let decimals = if per_million >= 1. || per_million <= 0. {
        2
    } else {
        (1 - per_million.log10().floor() as i32).clamp(2, 6) as usize
    };
    format!("${}", trimmed(per_million, decimals))
}

fn trimmed(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

const CLAUDE: &[u8] = include_bytes!("../assets/services/claude.svg");
const OPENAI: &[u8] = include_bytes!("../assets/services/openai.svg");
const KIMI: &[u8] = include_bytes!("../assets/services/kimi.svg");
const GROK: &[u8] = include_bytes!("../assets/services/grok.svg");
const OPENROUTER: &[u8] = include_bytes!("../assets/services/openrouter.svg");
const VERCEL: &[u8] = include_bytes!("../assets/services/vercel.svg");

/// A service's own mark, one ink: a tab's glyph.
fn service_logo(service: Service) -> gpui::Svg {
    let data = match service {
        Service::Claude => CLAUDE,
        Service::Codex => OPENAI,
        Service::OpenRouter => OPENROUTER,
        Service::Vercel => VERCEL,
    };
    gpui::svg().data(data).size(px(14.)).flex_none()
}

/// The logo of the lab behind `model` (by its wire id), in the lab's colour;
/// a gateway's own mark when the id names no lab it knows.
fn model_logo(service: Service, model: Option<&str>) -> gpui::Svg {
    let model = model.unwrap_or("").to_lowercase();
    let (data, color): (&'static [u8], gpui::Hsla) =
        if model.contains("claude") || service == Service::Claude {
            (CLAUDE, rgb(0xd97757).into())
        } else if model.contains("kimi") {
            (KIMI, rgb(0x6485ff).into())
        } else if model.contains("grok") {
            (GROK, ladder::foreground().into())
        } else if model.contains("gpt") || service == Service::Codex {
            (OPENAI, rgb(0x10a37f).into())
        } else if service == Service::Vercel {
            (VERCEL, ladder::foreground().into())
        } else {
            (OPENROUTER, rgb(0x8c7cf0).into())
        };
    gpui::svg()
        .data(data)
        .size(px(14.))
        .flex_none()
        .text_color(color)
}

#[derive(Default)]
struct Tween {
    state: Option<(f32, f32, Instant)>,
}
impl Tween {
    fn sample(
        &mut self,
        target: f32,
        reduced: bool,
        spec: &motion::MotionSpec,
        window: &mut gpui::Window,
    ) -> f32 {
        let (value, active) = self.advance(target, reduced, spec, Instant::now());
        if active {
            window.request_animation_frame();
        }
        value
    }

    fn advance(
        &mut self,
        target: f32,
        reduced: bool,
        spec: &motion::MotionSpec,
        now: Instant,
    ) -> (f32, bool) {
        let Some((from, to, since)) = self.state else {
            self.state = Some((target, target, now));
            return (target, false);
        };
        let raw = (now.duration_since(since).as_secs_f32()
            / motion::span(spec).as_secs_f32().max(0.001))
        .min(1.);
        let current = motion::lerp(from, to, spec.progress(raw));
        if reduced {
            self.state = Some((target, target, now));
            (target, false)
        } else if target != to {
            self.state = Some((current, target, now));
            (current, true)
        } else {
            (current, raw < 1. && from != to)
        }
    }
}

fn effort_index(fraction: f32, count: usize) -> usize {
    (fraction.clamp(0., 1.) * count.saturating_sub(1) as f32).round() as usize
}

/// A level as Zeron names it: `xhigh` is X-High.
fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "X-High".into(),
        value => {
            let mut chars = value.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn prices_and_windows_read_as_short_figures() {
        assert_eq!(dollars(1.89), "$1.89");
        assert_eq!(dollars(5.), "$5");
        assert_eq!(dollars(0.15), "$0.15");
        assert_eq!(dollars(0.037), "$0.037");
        assert_eq!(dollars(0.1), "$0.1");
        assert_eq!(tokens(1_048_576), "1M");
        assert_eq!(tokens(2_000_000), "2M");
        assert_eq!(tokens(262_144), "262K");
        let model = |price| ModelChoice {
            id: Some("openai/gpt-5.6-sol".into()),
            label: "GPT-5.6 Sol".into(),
            resolved_model: Some("openai/gpt-5.6-sol".into()),
            effort_levels: Vec::new(),
            context_window: Some(400_000),
            price,
        };
        assert_eq!(
            detail(&model(Some((1., 5.)))).as_deref(),
            Some("400K context · $1 in · $5 out")
        );
        assert_eq!(
            detail(&model(Some((0., 0.)))).as_deref(),
            Some("400K context · Free")
        );
        let row = |model| Row {
            service: Service::OpenRouter,
            model,
            shared_name: false,
        };
        assert!(matches(&row(model(None)), "gpt SOL"));
        assert!(matches(&row(model(None)), "openai/"));
        assert!(matches(&row(model(None)), "openrouter sol"));
        assert!(!matches(&row(model(None)), "kimi"));
    }

    #[test]
    fn pointer_selects_discrete_ticks_and_clamps_outside_track() {
        for count in 2..8 {
            for x in -20..260 {
                let value = snapped_fraction(x as f32, SLIDER_WIDTH, count);
                let tick = value * (count - 1) as f32;
                assert!((tick - tick.round()).abs() < 0.0001);
                assert!((0.0..=1.0).contains(&value));
            }
        }
        assert_eq!(snapped_fraction(-20., SLIDER_WIDTH, 4), 0.);
        assert_eq!(snapped_fraction(260., SLIDER_WIDTH, 4), 1.);
    }

    #[test]
    fn levels_read_as_zeron_names_them() {
        assert_eq!(effort_label("high"), "High");
        assert_eq!(effort_label("xhigh"), "X-High");
        assert_eq!(effort_label("minimal"), "Minimal");
    }

    #[test]
    fn morph_reverses_from_current_position_and_finishes_without_restart() {
        let mut tween = Tween::default();
        let now = Instant::now();
        let spec = &motion::COLLAPSE;
        assert_eq!(tween.advance(104., false, spec, now), (104., false));
        assert_eq!(tween.advance(300., false, spec, now), (104., true));
        let halfway = now + Duration::from_millis(50);
        let (height, active) = tween.advance(300., false, spec, halfway);
        assert!(active && height > 104. && height < 300.);
        assert_eq!(tween.advance(104., false, spec, halfway), (height, true));
        assert_eq!(
            tween.advance(104., false, spec, now + Duration::from_secs(2)),
            (104., false)
        );
        assert_eq!(
            tween.advance(300., true, spec, now + Duration::from_secs(3)),
            (300., false)
        );
    }
}
