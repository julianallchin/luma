//! The composer's model picker, after Zeron's (`zeronsh/zeron`,
//! `apps/zgx/ui/ds/model_picker.luau` at 68c7ea2): one card that opens
//! folded on the chosen model over its reasoning, and opens out in place —
//! a tab per service along the top, a search scoped to the tab, the tab's
//! models — around the chosen row. A pick folds it back onto the pick;
//! Escape folds an opened-out card, and closes a folded one.
//!
//! The card is a view of its own, mounted by the shell beside the chat panel
//! rather than inside it, so scrolling the list or typing in its search
//! redraws the card alone: a view nested in the panel would mark the panel
//! dirty with it. The panel draws only the chip ([`chip`]), and tells the
//! card what it shows of the panel ([`Seen`]) as that changes.
use crate::{Agent, AgentChat};
use gpui::{
    div, prelude::*, px, rgb, AnyElement, Bounds, Entity, FocusHandle, Focusable, Pixels,
    ScrollHandle, SharedString, Subscription, WeakEntity,
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

/// Most rows the list mounts. A gateway lists hundreds; past this many the
/// search is the way to reach the rest.
const LIST_LIMIT: usize = 60;
/// The tallest the scrolling list grows before it scrolls.
const LIST_MAX_HEIGHT: f32 = 264.;
/// The card's width: room for a gateway's long names beside their logo.
const CARD_WIDTH: f32 = 280.;
/// The card's inset, and the inset of the reasoning block inside it, which
/// lines the slider up with the rows' text.
const CARD_PAD: f32 = 6.;
const FOOT_PAD: f32 = 8.;
const SLIDER_WIDTH: f32 = CARD_WIDTH - 2. * (CARD_PAD + FOOT_PAD);
/// What the reasoning says for a model with no levels to choose.
const OWN_EFFORT: &str = "This model manages its own effort";
/// Skeleton rows while a list is first read.
const LOADING_ROWS: usize = 4;

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

pub struct ModelPicker {
    chat: WeakEntity<AgentChat>,
    agent: Agent,
    pub(crate) seen: Seen,
    focus: FocusHandle,
    pub(crate) open: bool,
    /// Opened out (the tabs, the search, the list) rather than folded on the
    /// chosen model.
    expanded: bool,
    /// The slider's level while it is dragged or stepped, ahead of the save.
    preview: Option<f32>,
    dragging: bool,
    thumb: RefCell<Tween>,
    height: RefCell<Tween>,
    /// The tab shown.
    service: Service,
    catalogs: HashMap<Service, Result<Vec<ModelChoice>, String>>,
    loading: HashSet<Service>,
    /// The services whose list saved on disk has been looked for, found or
    /// not: past that, the chip's label changes only when a fresh read lands.
    looked_up: HashSet<Service>,
    /// The search over the shown tab's list.
    search: Entity<TextInput>,
    _search: Subscription,
    /// The shown tab's rows, filtered by the search.
    list: float::Picker<ModelChoice>,
    scroll: ScrollHandle,
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
                let mut field = TextInput::search(placeholder(Service::Claude), cx);
                field.set_text_size(12.5, cx);
                field
            });
            let subscription = cx.subscribe(&search, |picker: &mut Self, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let query = field.read(cx).text().to_string();
                    picker.list.set_query(query);
                    picker.scroll.scroll_to_item(0);
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
                service: Service::Claude,
                catalogs: HashMap::new(),
                loading: HashSet::new(),
                looked_up: HashSet::new(),
                search,
                _search: subscription,
                list: float::Picker::new(matches).with_limit(LIST_LIMIT),
                scroll: ScrollHandle::new(),
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

    /// Show `service`'s models, reading them if needed. A gateway's list is
    /// read again every time — the card calls this each time it opens — and
    /// the rows already held, or saved on disk, show while it is on the wire,
    /// so the list is never blank while it refreshes. A CLI's list is read
    /// once per session: asking means starting the CLI.
    pub(crate) fn browse(&mut self, service: Service, cx: &mut gpui::Context<Self>) {
        self.show(service, cx);
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

    /// Show `service`'s tab, its search naming it.
    fn show(&mut self, service: Service, cx: &mut gpui::Context<Self>) {
        self.service = service;
        self.search.update(cx, |field, cx| {
            field.set_placeholder(placeholder(service), cx)
        });
        self.sync(cx);
    }

    /// Take a list read for `service`. A failed refresh keeps the rows
    /// already shown: a stale list is more use than an error in its place.
    fn receive(
        &mut self,
        service: Service,
        result: Result<Vec<ModelChoice>, String>,
        cx: &gpui::App,
    ) {
        if result.is_ok() || !matches!(self.catalogs.get(&service), Some(Ok(_))) {
            self.catalogs.insert(service, result);
        }
        if service == self.service {
            self.sync(cx);
        }
    }

    fn sync(&mut self, cx: &gpui::App) {
        let rows = match self.catalogs.get(&self.service) {
            Some(Ok(models)) => models.clone(),
            _ => Vec::new(),
        };
        self.list.set_rows(rows);
        self.list.set_query(self.search.read(cx).text().to_string());
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

    /// Escape: an opened-out card folds back onto the chosen model; a folded
    /// one closes.
    fn escape(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if self.expanded {
            self.fold(window, cx);
        } else {
            self.dismiss(window, cx);
        }
    }

    /// Open the card out: the tabs, the search and the list, on the chosen
    /// model's tab with the keyboard cursor on it.
    fn open_out(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let Some(service) = self.seen.selection.as_ref().map(|s| s.service) else {
            return;
        };
        self.expanded = true;
        self.browse(service, cx);
        self.seek();
        window.focus(&self.search.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    /// Fold the card back onto the chosen model, its search cleared.
    fn fold(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        self.expanded = false;
        self.clear_search(cx);
        if let Some(service) = self.seen.selection.as_ref().map(|s| s.service) {
            self.show(service, cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Show another service's tab, keeping what is searched for.
    fn show_tab(
        &mut self,
        service: Service,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.browse(service, cx);
        self.seek();
        window.focus(&self.search.read(cx).focus_handle(cx), cx);
    }

    /// Put the keyboard cursor on the chosen model, when the shown tab lists
    /// it: the list opens around it.
    fn seek(&mut self) {
        let Some(selection) = self.seen.selection.as_ref() else {
            return;
        };
        if selection.service != self.service {
            return;
        }
        if let Some(at) = self.list.seek(|model| model.matches(&selection.model)) {
            self.scroll.scroll_to_item(at);
        }
    }

    /// Move the list's keyboard cursor.
    fn step(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        if let Some(at) = self.list.step(delta) {
            self.scroll.scroll_to_item(at);
            cx.notify();
        }
    }

    /// Save `selection` and fold the card back onto it.
    fn pick(
        &mut self,
        selection: Selection,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(chat) = self.chat.upgrade() {
            chat.update(cx, |chat, cx| chat.select_model(selection, cx));
        }
        self.fold(window, cx);
    }

    /// Pick the model under the list's keyboard cursor.
    fn choose_current(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let (Some(selection), Some(model)) = (self.seen.selection.as_ref(), self.list.current())
        else {
            return;
        };
        let chosen = model.selection(self.service, selection);
        self.pick(chosen, window, cx);
    }

    /// Save the level the slider shows.
    fn commit_effort(&mut self, cx: &mut gpui::Context<Self>) {
        self.dragging = false;
        let (Some(fraction), Some(mut selection)) = (self.preview, self.seen.selection.clone())
        else {
            return;
        };
        let levels = self.efforts(&selection);
        selection.effort = levels[effort_index(fraction, levels.len())].clone();
        if let Some(chat) = self.chat.upgrade() {
            chat.update(cx, |chat, cx| chat.select_model(selection, cx));
        }
        cx.notify();
    }

    /// ← → Home End on the folded card move the reasoning a level.
    fn step_effort(&mut self, key: &str, cx: &mut gpui::Context<Self>) {
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
        let index = levels
            .iter()
            .position(|effort| effort == &selection.effort)
            .unwrap_or(0);
        let index = self
            .preview
            .map(|fraction| effort_index(fraction, levels.len()))
            .unwrap_or(index);
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

    fn efforts(&self, selection: &Selection) -> Vec<Option<String>> {
        std::iter::once(None)
            .chain(
                self.model(selection)
                    .into_iter()
                    .flat_map(|model| model.effort_levels.iter().cloned().map(Some)),
            )
            .collect()
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
/// and its effort muted after it. A chip opens a card; it carries no
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
                .child(effort_label(Some(effort)))
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
        let target_height = self.content_height.get().unwrap_or(120.);
        let height = self.height.borrow_mut().sample(
            target_height,
            motion::reduced_motion(cx),
            &motion::COLLAPSE,
            window,
        );
        let expanded = self.expanded;
        let content = self.card(&selection, disabled, &this, window, cx);
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
                            // Folded, the arrows and Enter open the list out
                            // around the chosen model, and typing opens it
                            // out already searching.
                            "up" | "down" | "enter" => picker.open_out(window, cx),
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
                    float::popover_card()
                        .p_0()
                        .h(px(height))
                        .overflow_hidden()
                        // Bottom-anchored, so as the card grows the
                        // reasoning by the chip stays put and the list opens
                        // out above it.
                        .justify_end()
                        .child(motion::fade_quick(
                            if expanded {
                                "picker-open"
                            } else {
                                "picker-folded"
                            },
                            div().flex_none().child(content),
                        )),
                )
                .into_any_element(),
        ))
    }
}

impl ModelPicker {
    /// The card: opened out, the tabs, the search and the list; folded, the
    /// chosen model alone. The reasoning under either.
    fn card(
        &self,
        selection: &Selection,
        disabled: bool,
        this: &Entity<Self>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> AnyElement {
        let measured = Rc::clone(&self.content_height);
        div()
            .relative()
            .flex()
            .flex_col()
            .w(px(CARD_WIDTH))
            .p(px(CARD_PAD))
            .text_color(ladder::foreground())
            .text_size(px(13.))
            .map(|el| {
                if self.expanded {
                    el.child(self.tabs(this))
                        .child(self.search_field(cx))
                        .child(self.list_body(selection, this, cx))
                } else {
                    el.child(self.chosen_row(selection, this))
                }
            })
            .child(float::divider().mx(px(-CARD_PAD)).my(px(CARD_PAD)))
            .child(self.reasoning(selection, disabled, this, window, cx))
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

    /// Folded, the chosen model as a row of its own: a press opens the list
    /// out around it.
    fn chosen_row(&self, selection: &Selection, this: &Entity<Self>) -> AnyElement {
        let open = this.clone();
        float::menu_row(RowState::Rest, "choose-model")
            .id("choose-model")
            .child(model_logo(selection.service, selection.model.as_deref()).size(px(16.)))
            .child(float::row_text(
                self.label(selection),
                Some(provider_label(selection.service).into()),
            ))
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(px(14.))
                    .text_color(ladder::muted_foreground()),
            )
            .on_click(move |_, window, cx| {
                open.update(cx, |picker, cx| picker.open_out(window, cx))
            })
            .agent_node(Role::Button, "Choose model")
            .into_any_element()
    }

    /// A tab per service, each its logo alone (its name on hover); the shown
    /// one in full ink with the accent's bar on the rule under the tabs.
    fn tabs(&self, this: &Entity<Self>) -> AnyElement {
        div()
            .flex()
            .gap(px(2.))
            .pb(px(4.))
            .mb(px(6.))
            .mx(px(-CARD_PAD))
            .px(px(CARD_PAD))
            .border_b_1()
            .border_color(glass::hairline(0.07))
            .children(Service::ALL.into_iter().map(|service| {
                let shown = service == self.service;
                let target = this.clone();
                let name = SharedString::from(service.label());
                div()
                    .id(SharedString::from(format!("service-{service:?}")))
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(32.))
                    .h(px(28.))
                    .rounded(px(radius::CONTROL))
                    .cursor_pointer()
                    .hover(|style| style.bg(glass::glass_hover()))
                    .child(service_logo(service).text_color(if shown {
                        ladder::foreground().into()
                    } else {
                        glass::ink(glass::TEXT_TERTIARY)
                    }))
                    .when(shown, |el| {
                        el.child(
                            div()
                                .absolute()
                                .left(px(8.))
                                .right(px(8.))
                                .bottom(px(-5.))
                                .h(px(2.))
                                .rounded_full()
                                .bg(ladder::primary()),
                        )
                    })
                    .tooltip({
                        let name = name.clone();
                        move |window, cx| Tooltip::new(name.clone()).build(window, cx)
                    })
                    .on_click(move |_, window, cx| {
                        target.update(cx, |picker, cx| picker.show_tab(service, window, cx))
                    })
                    .agent_node(Role::Button, name)
            }))
            .into_any_element()
    }

    fn search_field(&self, cx: &gpui::App) -> AnyElement {
        let query = self.search.read(cx).text().to_string();
        float::field()
            .w_full()
            .h(px(28.))
            .gap(px(6.))
            .child(
                Icon::new(IconName::Search)
                    .size(px(14.))
                    .text_color(glass::ink(glass::TEXT_TERTIARY)),
            )
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .agent_node(
                Role::Input,
                if query.is_empty() {
                    placeholder(self.service)
                } else {
                    query
                },
            )
            .into_any_element()
    }

    /// The shown tab's rows; while its list is first read, skeleton rows; a
    /// list that failed, why, and a retry; none to show, what is missing.
    fn list_body(
        &self,
        selection: &Selection,
        this: &Entity<Self>,
        cx: &mut gpui::App,
    ) -> AnyElement {
        let body = match self.catalogs.get(&self.service) {
            Some(Ok(_)) if self.list.is_empty() => {
                let query = self.list.query().trim();
                let service = self.service.label();
                if query.is_empty() {
                    empty("No models", format!("{service} has none to offer"))
                } else {
                    empty(
                        "No models found",
                        format!("Nothing in {service} matches “{query}”"),
                    )
                }
            }
            Some(Ok(_)) => self.model_rows(selection, this),
            Some(Err(error)) => {
                let target = this.clone();
                let service = self.service;
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(6.))
                    .p(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(ladder::muted_foreground())
                            .child(error.clone()),
                    )
                    .child(
                        luma_ui::button("Retry", luma_ui::Enabled::Yes)
                            .id("retry-models")
                            .on_click(move |_, _, cx| {
                                target.update(cx, |picker, cx| picker.browse(service, cx))
                            })
                            .agent_node(Role::Button, "Retry models"),
                    )
                    .into_any_element()
            }
            None => float::skeleton_rows(LOADING_ROWS, this.entity_id(), cx)
                .agent_node(Role::Text, "Loading models")
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .pt(px(4.))
            .child(body)
            .when(self.list.shown().len() == LIST_LIMIT, |el| {
                el.child(
                    div()
                        .px(px(float::ROW_INSET))
                        .pt(px(4.))
                        .text_size(px(11.))
                        .text_color(ladder::muted_foreground())
                        .child(format!(
                            "Only the first {LIST_LIMIT} show. Type to find others."
                        )),
                )
            })
            .into_any_element()
    }

    fn model_rows(&self, selection: &Selection, this: &Entity<Self>) -> AnyElement {
        let cursor = self.list.cursor();
        div()
            .id("model-list")
            .flex()
            .flex_col()
            .gap(px(2.))
            .max_h(px(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(self.list.shown().enumerate().map(|(index, model)| {
                let target = this.clone();
                let chosen = model.selection(self.service, selection);
                let active = selection.service == self.service && model.matches(&selection.model);
                let key = format!(
                    "model-{:?}-{}",
                    self.service,
                    model.id.as_deref().unwrap_or("default")
                );
                let detail = detail(model);
                let wire = model.resolved_model.as_deref().or(model.id.as_deref());
                float::menu_row(RowState::of(active, index == cursor), key.clone())
                    .id(SharedString::from(key))
                    .flex_none()
                    .child(model_logo(self.service, wire).size(px(16.)))
                    .child(float::row_text(
                        model.label.clone(),
                        detail.clone().map(SharedString::from),
                    ))
                    .on_click(move |_, window, cx| {
                        target.update(cx, |picker, cx| picker.pick(chosen.clone(), window, cx))
                    })
                    .agent_node(
                        Role::Button,
                        match detail {
                            Some(detail) => format!("{} · {detail}", model.label),
                            None => model.label.clone(),
                        },
                    )
            }))
            .into_any_element()
    }

    /// The reasoning: its name and level over the slider.
    fn reasoning(
        &self,
        selection: &Selection,
        disabled: bool,
        this: &Entity<Self>,
        window: &mut gpui::Window,
        cx: &gpui::App,
    ) -> AnyElement {
        let levels = self.efforts(selection);
        let selected = levels
            .iter()
            .position(|effort| effort == &selection.effort)
            .unwrap_or(0);
        let fraction = self
            .preview
            .unwrap_or(selected as f32 / (levels.len().saturating_sub(1).max(1)) as f32);
        let index = effort_index(fraction, levels.len());
        let label = effort_label(levels[index].as_deref());
        let animated = self.thumb.borrow_mut().sample(
            fraction,
            motion::reduced_motion(cx),
            &motion::TAB_SLIDE,
            window,
        );
        let adjustable = levels.len() > 1;
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(FOOT_PAD))
            .pb(px(FOOT_PAD - CARD_PAD + 2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(12.))
                    .child(
                        div()
                            .text_color(ladder::muted_foreground())
                            .child("Reasoning"),
                    )
                    .when(adjustable, |el| {
                        el.child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(effort_color(fraction))
                                .child(label.clone())
                                .agent_node(Role::Text, format!("Effort · {label}")),
                        )
                    }),
            )
            .child(if adjustable {
                slider(animated, levels.len(), disabled, this)
            } else {
                div()
                    .text_size(px(12.))
                    .text_color(glass::ink(glass::TEXT_TERTIARY))
                    .child(OWN_EFFORT)
                    .agent_node(Role::Text, OWN_EFFORT)
                    .into_any_element()
            })
            .into_any_element()
    }
}

fn slider(fraction: f32, count: usize, disabled: bool, this: &Entity<ModelPicker>) -> AnyElement {
    const THUMB: f32 = 24.;
    let travel = SLIDER_WIDTH - THUMB;
    let left = fraction * travel;
    let (bounds, probe) = luma_ui::arg::bounds_probe();
    let down = this.clone();
    let up = this.clone();
    let outside = this.clone();
    let moving = this.clone();
    let color = effort_color(fraction);
    let fill = if fraction > 0.75 {
        gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(rgb(0x4244df), 0.),
            gpui::linear_color_stop(rgb(0xab79ef), 1.),
        )
    } else {
        gpui::Background::from(color)
    };
    div()
        .id("effort-slider")
        .relative()
        .w(px(SLIDER_WIDTH))
        .h(px(24.))
        .cursor_pointer()
        .child(probe)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(3.))
                .h(px(18.))
                .rounded_full()
                .bg(ladder::foreground_alpha(0.12)),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(3.))
                .w(px(left + THUMB / 2.))
                .h(px(18.))
                .rounded_full()
                .bg(fill),
        )
        .children((0..count).map(|index| {
            div()
                .absolute()
                .left(px(
                    THUMB / 2. + travel * index as f32 / (count - 1) as f32 - 2.
                ))
                .top(px(10.))
                .size(px(4.))
                .rounded_full()
                .bg(ladder::foreground_alpha(0.3))
        }))
        .child(
            div()
                .absolute()
                .left(px(left))
                .top_0()
                .size(px(THUMB))
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

/// The search's placeholder, naming the tab it searches.
fn placeholder(service: Service) -> String {
    format!("Search {} models…", service.label())
}

/// What an empty list shows: what is missing, and where.
fn empty(title: &str, hint: String) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .px(px(12.))
        .py(px(20.))
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

/// Whether `model` matches every word of `query`, by name or wire id.
fn matches(model: &ModelChoice, query: &str) -> bool {
    let haystack = format!(
        "{} {}",
        model.label,
        model.resolved_model.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

/// The line under a model's name: its window and its price per million
/// tokens, as far as its list says.
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

fn snapped_fraction(x: f32, width: f32, count: usize) -> f32 {
    let fraction = ((x - 12.) / (width - 24.).max(1.)).clamp(0., 1.);
    effort_index(fraction, count) as f32 / count.saturating_sub(1).max(1) as f32
}

fn provider_label(service: Service) -> String {
    if service.provider().is_none() {
        format!("{} · Subscription", service.label())
    } else {
        service.label().into()
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
    gpui::svg().data(data).size(px(16.)).flex_none()
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
fn effort_color(fraction: f32) -> gpui::Hsla {
    if fraction > 0.75 {
        rgb(0xb57bff).into()
    } else {
        rgb(0x3784fb).into()
    }
}
fn effort_label(effort: Option<&str>) -> String {
    match effort {
        None => "Auto".into(),
        Some("xhigh") => "Extra high".into(),
        Some(value) => {
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
        assert!(matches(&model(None), "gpt SOL"));
        assert!(matches(&model(None), "openai/"));
        assert!(!matches(&model(None), "kimi"));
    }

    #[test]
    fn pointer_selects_discrete_ticks_and_clamps_outside_track() {
        for count in 2..8 {
            for x in -20..260 {
                let value = snapped_fraction(x as f32, 226., count);
                let tick = value * (count - 1) as f32;
                assert!((tick - tick.round()).abs() < 0.0001);
                assert!((0.0..=1.0).contains(&value));
            }
        }
        assert_eq!(snapped_fraction(-20., 226., 4), 0.);
        assert_eq!(snapped_fraction(260., 226., 4), 1.);
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
