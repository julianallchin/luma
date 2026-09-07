use gpui::{div, prelude::*, px, AnyElement, Entity, SharedString, Svg};
use gpui_component::{Icon, IconName};
use luma_lib::agent::{
    engine::catalog::{ModelChoice, Selection, Service},
    model::MODELS,
};
use luma_ui::{
    float, ladder,
    node::{AgentNode, Instrument, Role},
};
use std::collections::{HashMap, HashSet};

use crate::AgentChat;

pub(crate) struct Picker {
    pub open: bool,
    pub effort_open: bool,
    pub service: Service,
    pub catalogs: HashMap<Service, Result<Vec<ModelChoice>, String>>,
    pub loading: HashSet<Service>,
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            open: false,
            effort_open: false,
            service: Service::Claude,
            catalogs: HashMap::new(),
            loading: HashSet::new(),
        }
    }
}

fn logo(service: Service) -> Svg {
    gpui::svg()
        .data(match service {
            Service::Claude | Service::Anthropic => {
                include_bytes!("../assets/services/claude.svg").as_slice()
            }
            Service::Codex => include_bytes!("../assets/services/openai.svg").as_slice(),
            Service::OpenRouter => include_bytes!("../assets/services/openrouter.svg").as_slice(),
            Service::Vercel => include_bytes!("../assets/services/vercel.svg").as_slice(),
        })
        .size(px(17.))
        .text_color(ladder::foreground())
}

fn model_logo(service: Service, model: Option<&str>) -> Svg {
    match model {
        Some(model) if model.starts_with("kimi-") => gpui::svg()
            .data(include_bytes!("../assets/services/kimi.svg"))
            .size(px(17.))
            .text_color(ladder::foreground()),
        Some(model) if model.starts_with("grok-") => gpui::svg()
            .data(include_bytes!("../assets/services/grok.svg"))
            .size(px(17.))
            .text_color(ladder::foreground()),
        Some(model) if model.starts_with("claude-") => logo(Service::Claude),
        _ => logo(service),
    }
}

impl Picker {
    pub fn render(
        &self,
        selection: &Selection,
        disabled: bool,
        chat: &Entity<AgentChat>,
    ) -> AnyElement {
        let label = self
            .catalogs
            .get(&selection.service)
            .and_then(|result| result.as_ref().ok())
            .and_then(|models| models.iter().find(|model| model.matches(&selection.model)))
            .map(|model| model.label.clone())
            .or_else(|| {
                MODELS
                    .iter()
                    .find(|model| Some(model.key) == selection.model.as_deref())
                    .map(|model| model.display.into())
            })
            .unwrap_or_else(|| selection.model.clone().unwrap_or_else(|| "Default".into()));
        let toggle = chat.clone();
        let trigger = float::chip()
            .id("thread-model")
            .max_w(px(210.))
            .child(logo(selection.service))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(label.clone())),
            )
            .child(Icon::new(IconName::ChevronDown).size(px(12.)))
            .when(disabled, |el| el.opacity(0.5))
            .on_click(move |_, _, cx| toggle.update(cx, |chat, cx| chat.toggle_model_picker(cx)))
            .agent_node(
                Role::Select,
                format!("{} · {label}", selection.service.label()),
            )
            .agent_disabled(disabled);
        let dismiss = chat.clone();
        div()
            .flex()
            .gap(px(4.))
            .child(div().relative().child(trigger).when(self.open, |el| {
                el.child(float::anchored_below(
                    "thread-model-menu",
                    luma_ui::CONTROL_HEIGHT,
                    float::Dismiss::on_press_out(move |_, cx| {
                        dismiss.update(cx, |chat, cx| chat.toggle_model_picker(cx))
                    }),
                    self.menu(selection, chat),
                ))
            }))
            .child(self.effort_picker(selection, disabled, chat))
            .into_any_element()
    }

    fn effort_picker(
        &self,
        selection: &Selection,
        disabled: bool,
        chat: &Entity<AgentChat>,
    ) -> AnyElement {
        let model = self
            .catalogs
            .get(&selection.service)
            .and_then(|catalog| catalog.as_ref().ok())
            .and_then(|models| models.iter().find(|model| model.matches(&selection.model)));
        let Some(model) = model.filter(|model| !model.effort_levels.is_empty()) else {
            return div().into_any_element();
        };
        let label = effort_label(selection.effort.as_deref());
        let toggle = chat.clone();
        let dismiss = chat.clone();
        let trigger = float::chip()
            .id("thread-effort")
            .child(label.clone())
            .child(Icon::new(IconName::ChevronDown).size(px(12.)))
            .when(disabled, |el| el.opacity(0.5))
            .on_click(move |_, _, cx| toggle.update(cx, |chat, cx| chat.toggle_effort_picker(cx)))
            .agent_node(Role::Select, format!("Effort · {label}"))
            .agent_disabled(disabled);
        div()
            .relative()
            .child(trigger)
            .when(self.effort_open, |el| {
                let mut menu = float::popover_card()
                    .w(px(160.))
                    .child(div().px(px(10.)).py(px(6.)).child("Effort"));
                for effort in std::iter::once(None)
                    .chain(model.effort_levels.iter().map(|level| Some(level.as_str())))
                {
                    let mut chosen = selection.clone();
                    chosen.effort = effort.map(str::to_string);
                    let active = chosen.effort == selection.effort;
                    let label = effort_label(effort);
                    let chat = chat.clone();
                    let id = format!("effort-{}", effort.unwrap_or("default"));
                    menu = menu.child(
                        float::menu_row(float::RowState::of(active, false), id.clone())
                            .id(SharedString::from(id))
                            .px(px(10.))
                            .py(px(8.))
                            .child(div().flex_1().child(label.clone()))
                            .when(active, |row| {
                                row.child(Icon::new(IconName::Check).size(px(14.)))
                            })
                            .on_click(move |_, _, cx| {
                                chat.update(cx, |chat, cx| chat.select_model(chosen.clone(), cx))
                            })
                            .agent_node(Role::Button, format!("Effort {label}")),
                    );
                }
                el.child(float::anchored_below(
                    "thread-effort-menu",
                    luma_ui::CONTROL_HEIGHT,
                    float::Dismiss::on_press_out(move |_, cx| {
                        dismiss.update(cx, |chat, cx| chat.toggle_effort_picker(cx))
                    }),
                    menu.into_any_element(),
                ))
            })
            .into_any_element()
    }

    fn menu(&self, selection: &Selection, chat: &Entity<AgentChat>) -> AnyElement {
        let tabs =
            Service::ALL
                .into_iter()
                .fold(div().flex().gap(px(4.)).p(px(4.)), |tabs, service| {
                    let chat = chat.clone();
                    tabs.child(
                        float::chip()
                            .id(SharedString::from(format!("service-{service:?}")))
                            .w(px(42.))
                            .child(logo(service))
                            .when(service == self.service, |tab| {
                                tab.bg(ladder::foreground_alpha(0.12))
                            })
                            .on_click(move |_, _, cx| {
                                chat.update(cx, |chat, cx| chat.browse_models(service, cx))
                            })
                            .agent_node(Role::Button, service.label()),
                    )
                });
        let subtitle = if self.service.provider().is_some() {
            "API billing"
        } else {
            "Your subscription"
        };
        let mut menu = float::popover_card().w(px(300.)).child(tabs).child(
            div()
                .px(px(10.))
                .py(px(6.))
                .flex()
                .justify_between()
                .child(self.service.label())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(ladder::muted_foreground())
                        .child(subtitle),
                ),
        );
        match self.catalogs.get(&self.service) {
            None => {
                menu = menu.child(
                    float::empty_row("Loading models…").agent_node(Role::Text, "Loading models…"),
                )
            }
            Some(Err(error)) => {
                let chat = chat.clone();
                let service = self.service;
                menu = menu
                    .child(div().p(px(10.)).text_size(px(12.)).child(error.clone()))
                    .child(
                        float::chip()
                            .id("retry-models")
                            .child("Retry")
                            .on_click(move |_, _, cx| {
                                chat.update(cx, |chat, cx| chat.browse_models(service, cx))
                            })
                            .agent_node(Role::Button, "Retry models"),
                    );
            }
            Some(Ok(models)) => {
                let rows = models.iter().enumerate().fold(
                    div()
                        .id("thread-model-list")
                        .max_h(px(280.))
                        .overflow_y_scroll(),
                    |rows, (index, model)| {
                        let chosen = model.selection(self.service, selection);
                        let active =
                            self.service == selection.service && model.matches(&selection.model);
                        let chat = chat.clone();
                        rows.child(
                            float::menu_row(
                                float::RowState::of(active, false),
                                format!("model-{index}"),
                            )
                            .id(SharedString::from(format!("model-{index}")))
                            .px(px(10.))
                            .py(px(8.))
                            .gap(px(10.))
                            .child(model_logo(self.service, model.id.as_deref()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().child(model.label.clone()))
                                    .when_some(
                                        model.resolved_model.clone().or(model.id.clone()),
                                        |el, version| {
                                            el.child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(ladder::muted_foreground())
                                                    .child(version.clone())
                                                    .agent_node(Role::Text, version),
                                            )
                                        },
                                    ),
                            )
                            .when(active, |row| {
                                row.child(Icon::new(IconName::Check).size(px(14.)))
                            })
                            .on_click(move |_, _, cx| {
                                chat.update(cx, |chat, cx| chat.select_model(chosen.clone(), cx))
                            })
                            .agent_node(Role::Button, model.label.clone()),
                        )
                    },
                );
                menu = menu.child(rows);
            }
        }
        menu.into_any_element()
    }
}

fn effort_label(effort: Option<&str>) -> String {
    match effort {
        None => "Auto".into(),
        Some("xhigh") => "Extra high".into(),
        Some(effort) => {
            let mut chars = effort.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}
