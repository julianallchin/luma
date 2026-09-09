use crate::AgentChat;
use gpui::{div, prelude::*, px, rgb, AnyElement, Entity, FocusHandle, SharedString};
use gpui_component::Icon;
use luma_lib::agent::{
    engine::catalog::{ModelChoice, Selection, Service},
    model::MODELS,
};
use luma_ui::{
    float,
    icons::IconName,
    ladder, motion,
    node::{AgentNode, Instrument, Role},
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    time::Instant,
};

gpui::actions!(model_picker, [DismissPicker]);

pub(crate) struct Picker {
    pub focus: FocusHandle,
    pub open: bool,
    pub models_open: bool,
    pub preview: Option<f32>,
    pub dragging: bool,
    pub queued: Option<Selection>,
    thumb: RefCell<Tween>,
    height: RefCell<Tween>,
    pub service: Service,
    pub catalogs: HashMap<Service, Result<Vec<ModelChoice>, String>>,
    pub loading: HashSet<Service>,
}

impl Picker {
    pub fn new(cx: &mut gpui::Context<AgentChat>) -> Self {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            DismissPicker,
            Some("ModelEffortPicker"),
        )]);
        Self {
            focus: cx.focus_handle(),
            open: false,
            models_open: false,
            preview: None,
            dragging: false,
            queued: None,
            thumb: RefCell::new(Tween::default()),
            height: RefCell::new(Tween::default()),
            service: Service::Claude,
            catalogs: HashMap::new(),
            loading: HashSet::new(),
        }
    }

    fn model(&self, selection: &Selection) -> Option<&ModelChoice> {
        self.catalogs
            .get(&selection.service)?
            .as_ref()
            .ok()?
            .iter()
            .find(|model| model.matches(&selection.model))
    }

    pub fn efforts(&self, selection: &Selection) -> Vec<Option<String>> {
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

    pub fn render(
        &self,
        selection: &Selection,
        disabled: bool,
        chat: &Entity<AgentChat>,
        window: &mut gpui::Window,
        cx: &gpui::App,
    ) -> AnyElement {
        let toggle = chat.clone();
        let dismiss = chat.clone();
        let key_chat = chat.clone();
        let escape = chat.clone();
        let trigger = float::chip()
            .id("thread-model")
            .max_w(px(190.))
            .child(model_logo(selection))
            .child(div().min_w_0().truncate().child(self.label(selection)))
            .child(Icon::new(IconName::ChevronDown).size(px(12.)))
            .when(disabled, |el| el.opacity(0.5))
            .on_click(move |_, window, cx| {
                toggle.update(cx, |chat, cx| chat.toggle_model_picker(window, cx))
            })
            .agent_node(
                Role::Select,
                format!("{} · {}", selection.service.label(), self.label(selection)),
            )
            .agent_disabled(disabled);
        let target_height = if self.open && self.models_open {
            let rows = self
                .catalogs
                .get(&self.service)
                .and_then(|r| r.as_ref().ok())
                .map_or(2, |m| m.len());
            102. + (rows as f32 * 30.).min(240.)
        } else {
            104.
        };
        let height = self.height.borrow_mut().sample(
            target_height,
            !self.open || motion::reduced_motion(cx),
            &motion::COLLAPSE,
            window,
        );
        div()
            .flex()
            .relative()
            .child(trigger)
            .when(self.open, |el| {
                let content = if self.models_open {
                    self.model_menu(selection, chat)
                } else {
                    self.effort_card(selection, disabled, chat, window, cx)
                };
                el.child(float::anchored_above(
                    "thread-model-menu",
                    luma_ui::CONTROL_HEIGHT,
                    float::Dismiss::on_press_out(move |window, cx| {
                        dismiss.update(cx, |chat, cx| {
                            chat.escape(cx);
                            window.focus(&chat.composer.focus_handle(cx), cx);
                        });
                    }),
                    div()
                        .id("model-picker-focus")
                        .flex()
                        .flex_col()
                        .w(px(252.))
                        .track_focus(&self.focus)
                        .key_context("ModelEffortPicker")
                        .on_action(move |_: &DismissPicker, window, cx| {
                            escape.update(cx, |chat, cx| {
                                chat.escape(cx);
                                window.focus(&chat.composer.focus_handle(cx), cx);
                            });
                        })
                        .on_key_down(move |event, window, cx| {
                            match event.keystroke.key.as_str() {
                                "escape" => key_chat.update(cx, |chat, cx| {
                                    chat.escape(cx);
                                    window.focus(&chat.composer.focus_handle(cx), cx);
                                }),
                                "left" | "right" | "home" | "end" => key_chat
                                    .update(cx, |chat, cx| {
                                        chat.step_effort(&event.keystroke.key, cx)
                                    }),
                                _ => return,
                            }
                            cx.stop_propagation();
                        })
                        .child(
                            float::popover_card()
                                .p_0()
                                .h(px(height))
                                .child(motion::fade_quick(
                                    if self.models_open {
                                        "picker-models"
                                    } else {
                                        "picker-effort"
                                    },
                                    div().flex_none().child(content),
                                )),
                        )
                        .into_any_element(),
                ))
            })
            .into_any_element()
    }

    fn effort_card(
        &self,
        selection: &Selection,
        disabled: bool,
        chat: &Entity<AgentChat>,
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
        let tint = effort_color(fraction);
        let models = chat.clone();
        let animated = self.thumb.borrow_mut().sample(
            fraction,
            motion::reduced_motion(cx),
            &motion::TAB_SLIDE,
            window,
        );
        card()
            .h(px(104.))
            .p(px(12.))
            .gap(px(8.))
            .child(
                div().flex().items_center().gap(px(12.)).h(px(22.)).child(
                    div()
                        .id("choose-model")
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_start()
                        .gap(px(6.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .child(model_logo(selection))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(self.label(selection)),
                        )
                        .child(
                            div()
                                .text_color(tint)
                                .child(label.clone())
                                .agent_node(Role::Text, format!("Effort · {label}")),
                        )
                        .child(Icon::new(IconName::ChevronRight).size(px(15.)))
                        .on_click(move |_, _, cx| {
                            models.update(cx, |chat, cx| {
                                chat.model_picker.models_open = true;
                                cx.notify();
                            })
                        })
                        .agent_node(Role::Button, "Choose model"),
                ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(ladder::muted_foreground())
                    .child(provider_label(selection.service))
                    .agent_node(Role::Text, provider_label(selection.service)),
            )
            .child(if levels.len() > 1 {
                self.slider(animated, levels.len(), disabled, chat)
            } else {
                div()
                    .text_size(px(13.))
                    .text_color(ladder::muted_foreground())
                    .child("This model manages its own effort")
                    .into_any_element()
            })
            .into_any_element()
    }

    fn slider(
        &self,
        fraction: f32,
        count: usize,
        disabled: bool,
        chat: &Entity<AgentChat>,
    ) -> AnyElement {
        const WIDTH: f32 = 226.;
        const THUMB: f32 = 24.;
        let travel = WIDTH - THUMB;
        let left = fraction * travel;
        let (bounds, probe) = luma_ui::arg::bounds_probe();
        let down = chat.clone();
        let up = chat.clone();
        let outside = chat.clone();
        let moving = chat.clone();
        let color = effort_color(fraction);
        let fill = if fraction > 0.75 {
            gpui::linear_gradient(
                90.,
                gpui::linear_color_stop(rgb(0x4244df), 0.),
                gpui::linear_color_stop(rgb(0xab79ef), 1.),
            )
            .into()
        } else {
            gpui::Background::from(color)
        };
        div()
            .id("effort-slider")
            .relative()
            .w(px(WIDTH))
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
                                moving.update(cx, |chat, cx| {
                                    if chat.model_picker.dragging {
                                        chat.model_picker.preview = Some(snapped_fraction(
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
                    down.update(cx, |chat, cx| {
                        chat.model_picker.dragging = true;
                        chat.model_picker.preview = Some(snapped_fraction(
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
                up.update(cx, |chat, cx| chat.commit_effort(cx))
            })
            .on_mouse_up_out(gpui::MouseButton::Left, move |_, _, cx| {
                outside.update(cx, |chat, cx| chat.commit_effort(cx))
            })
            .agent_node(Role::Slider, "Reasoning effort")
            .agent_disabled(disabled)
            .into_any_element()
    }

    fn model_menu(&self, selection: &Selection, chat: &Entity<AgentChat>) -> AnyElement {
        let mut menu = card().p(px(8.)).gap(px(4.));
        match self.catalogs.get(&self.service) {
            Some(Ok(models)) => {
                let mut rows = div().id("model-list").max_h(px(240.)).overflow_y_scroll();
                for (index, model) in models.iter().enumerate() {
                    let target = chat.clone();
                    let chosen = model.selection(self.service, selection);
                    let active =
                        selection.service == self.service && model.matches(&selection.model);
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!("model-{index}")))
                            .flex()
                            .items_center()
                            .h(px(30.))
                            .px(px(2.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|el| el.bg(ladder::foreground_alpha(0.06)))
                            .child(div().flex_1().truncate().child(model.label.clone()))
                            .when(active, |el| {
                                el.child(
                                    Icon::new(IconName::Check)
                                        .size(px(14.))
                                        .text_color(ladder::muted_foreground()),
                                )
                            })
                            .on_click(move |_, _, cx| {
                                target.update(cx, |chat, cx| {
                                    chat.select_model(chosen.clone(), cx);
                                    chat.model_picker.models_open = false;
                                })
                            })
                            .agent_node(Role::Button, model.label.clone()),
                    );
                }
                menu = menu.child(rows);
            }
            Some(Err(error)) => {
                let target = chat.clone();
                let service = self.service;
                menu = menu
                    .child(div().text_size(px(13.)).child(error.clone()))
                    .child(
                        div()
                            .id("retry-models")
                            .cursor_pointer()
                            .child("Retry")
                            .on_click(move |_, _, cx| {
                                target.update(cx, |chat, cx| chat.browse_models(service, cx))
                            })
                            .agent_node(Role::Button, "Retry models"),
                    );
            }
            None => menu = menu.child("Loading models…"),
        }
        menu.child(float::divider())
            .child(div().flex().flex_wrap().gap(px(4.)).pt(px(8.)).children(
                Service::ALL.into_iter().map(|service| {
                    let target = chat.clone();
                    float::chip()
                        .id(SharedString::from(format!("service-{service:?}")))
                        .text_size(px(11.))
                        .child(service.label())
                        .when(service == self.service, |el| {
                            el.bg(ladder::foreground_alpha(0.14))
                        })
                        .on_click(move |_, _, cx| {
                            target.update(cx, |chat, cx| chat.browse_models(service, cx))
                        })
                        .agent_node(Role::Button, service.label())
                }),
            ))
            .into_any_element()
    }
}

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(250.))
        .text_color(ladder::foreground())
        .text_size(px(12.))
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

fn model_logo(selection: &Selection) -> gpui::Svg {
    let model = selection.model.as_deref().unwrap_or("").to_lowercase();
    let (data, color): (&'static [u8], gpui::Hsla) = if model.contains("claude")
        || matches!(selection.service, Service::Claude | Service::Anthropic)
    {
        (
            include_bytes!("../assets/services/claude.svg"),
            rgb(0xd97757).into(),
        )
    } else if model.contains("kimi") {
        (
            include_bytes!("../assets/services/kimi.svg"),
            rgb(0x6485ff).into(),
        )
    } else if model.contains("grok") {
        (
            include_bytes!("../assets/services/grok.svg"),
            ladder::foreground().into(),
        )
    } else if model.contains("gpt") || selection.service == Service::Codex {
        (
            include_bytes!("../assets/services/openai.svg"),
            rgb(0x10a37f).into(),
        )
    } else if selection.service == Service::Vercel {
        (
            include_bytes!("../assets/services/vercel.svg"),
            ladder::foreground().into(),
        )
    } else {
        (
            include_bytes!("../assets/services/openrouter.svg"),
            rgb(0x8c7cf0).into(),
        )
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

pub(crate) fn effort_index(fraction: f32, count: usize) -> usize {
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
