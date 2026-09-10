//! Shared structured mapping control for graph inputs and clip overrides.
use gpui::prelude::*;
use gpui::{div, px, Context, Entity, EventEmitter, SharedString, Subscription, Window};
use luma_patterns::{Cell, MappingSource, MappingSpec, MirrorPlane};

use super::number::{DraftedNumber, NumberEvent};
use super::select::luma_arg_select;
use crate::node::{Instrument, Role};
use crate::{ladder, luma_toggle};

#[derive(Clone, Debug)]
pub struct MappingChanged(pub MappingSpec);

const MIRRORS: [&str; 5] = ["Off", "Left–right", "Front–back", "Up–down", "Custom plane"];

#[derive(Clone, Copy, PartialEq)]
enum Menu {
    Source,
    Mirror,
}

pub struct MappingEditor {
    name: SharedString,
    value: MappingSpec,
    direction: [Entity<DraftedNumber>; 3],
    origin: Entity<DraftedNumber>,
    normal: [Entity<DraftedNumber>; 3],
    offset: Entity<DraftedNumber>,
    menu: Option<Menu>,
    custom_mirror: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MappingChanged> for MappingEditor {}

impl MappingEditor {
    pub fn new(
        name: impl Into<SharedString>,
        value: MappingSpec,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = name.into();
        let components = direction(&value.source);
        let mut subscriptions = Vec::new();
        let direction = std::array::from_fn(|axis| {
            let field = cx.new(|cx| {
                DraftedNumber::new(
                    format!("{name}: {}", ["U", "V", "Z"][axis]),
                    components[axis],
                    -f64::MAX,
                    f64::MAX,
                    (width - 12.) / 3.,
                    window,
                    cx,
                )
            });
            subscriptions.push(cx.subscribe(&field, move |this, _, event, cx| {
                let NumberEvent::Committed(component) = *event;
                match &mut this.value.source {
                    MappingSource::Vector { direction } => direction[axis] = component,
                    MappingSource::MajorAxis { toward } => {
                        // The existing major-axis orientation is stored in world XYZ.
                        toward[axis] = if axis == 1 { -component } else { component };
                    }
                    _ => return,
                }
                this.publish(cx);
            }));
            field
        });
        let origin = cx.new(|cx| {
            DraftedNumber::new(
                format!("{name}: Circle origin"),
                circle_origin(&value.source),
                -1e9,
                1e9,
                width,
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe(&origin, |this, _, event, cx| {
            let NumberEvent::Committed(value) = *event;
            if let MappingSource::Circle { origin } = &mut this.value.source {
                *origin = value;
                this.publish(cx);
            }
        }));
        let normal = std::array::from_fn(|axis| {
            let field = cx.new(|cx| {
                DraftedNumber::new(
                    format!("{name}: Mirror {}", ["U", "V", "Z"][axis]),
                    value
                        .mirror
                        .as_ref()
                        .map_or([1., 0., 0.], |plane| plane.normal)[axis],
                    -f64::MAX,
                    f64::MAX,
                    (width - 12.) / 3.,
                    window,
                    cx,
                )
            });
            subscriptions.push(cx.subscribe(&field, move |this, _, event, cx| {
                let NumberEvent::Committed(value) = *event;
                if let Some(plane) = &mut this.value.mirror {
                    plane.normal[axis] = value;
                    this.publish(cx);
                }
            }));
            field
        });
        let offset = cx.new(|cx| {
            DraftedNumber::new(
                format!("{name}: Mirror offset"),
                value.mirror.as_ref().map_or(0., |plane| plane.offset),
                -1e9,
                1e9,
                width,
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe(&offset, |this, _, event, cx| {
            let NumberEvent::Committed(value) = *event;
            if let Some(plane) = &mut this.value.mirror {
                plane.offset = value;
                this.publish(cx);
            }
        }));
        let custom_mirror = mirror_index(&value.mirror) == 4;
        Self {
            name,
            value,
            direction,
            origin,
            normal,
            offset,
            menu: None,
            custom_mirror,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_value(&mut self, value: MappingSpec, cx: &mut Context<Self>) {
        if self.value == value {
            return;
        }
        self.value = value;
        self.custom_mirror = mirror_index(&self.value.mirror) == 4;
        self.error = None;
        self.sync_fields(cx);
        cx.notify();
    }

    fn sync_fields(&mut self, cx: &mut Context<Self>) {
        for (field, value) in self.direction.iter().zip(direction(&self.value.source)) {
            field.update(cx, |field, cx| field.set_value(value, cx));
        }
        self.origin.update(cx, |field, cx| {
            field.set_value(circle_origin(&self.value.source), cx)
        });
        let normal = self
            .value
            .mirror
            .as_ref()
            .map_or([1., 0., 0.], |plane| plane.normal);
        for (field, value) in self.normal.iter().zip(normal) {
            field.update(cx, |field, cx| field.set_value(value, cx));
        }
        self.offset.update(cx, |field, cx| {
            field.set_value(
                self.value.mirror.as_ref().map_or(0., |plane| plane.offset),
                cx,
            )
        });
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.error = self.value.validate().err().map(|error| error.to_string());
        if self.error.is_none() {
            cx.emit(MappingChanged(self.value.clone()));
        }
        cx.notify();
    }
}

fn mirror_index(plane: &Option<MirrorPlane>) -> usize {
    match plane.as_ref().map(|plane| plane.normal) {
        None => 0,
        Some([1., 0., 0.]) => 1,
        Some([0., 1., 0.]) => 2,
        Some([0., 0., 1.]) => 3,
        Some(_) => 4,
    }
}

fn direction(source: &MappingSource) -> [f64; 3] {
    match source {
        MappingSource::Vector { direction } => *direction,
        MappingSource::MajorAxis { toward } => Cell::stage_coordinates(*toward),
        _ => [0., 0., 1.],
    }
}

fn circle_origin(source: &MappingSource) -> f64 {
    match source {
        MappingSource::Circle { origin } => *origin,
        _ => 0.,
    }
}

impl Render for MappingEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let labels: Vec<_> = MappingSource::OPTIONS
            .iter()
            .map(|(_, label)| *label)
            .collect();
        let selected = MappingSource::OPTIONS
            .iter()
            .find(|(key, _)| *key == self.value.source.key())
            .map_or("Mapping", |(_, label)| *label);
        let toggle = cx.entity();
        let pick = cx.entity();
        let mirror_toggle = cx.entity();
        let mirror_pick = cx.entity();
        let mirror = if self.custom_mirror && self.value.mirror.is_some() {
            4
        } else {
            mirror_index(&self.value.mirror)
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(luma_arg_select(
                self.name.clone(),
                selected,
                &labels,
                self.menu == Some(Menu::Source),
                move |_, cx| {
                    toggle.update(cx, |this, cx| {
                        this.menu = if this.menu == Some(Menu::Source) {
                            None
                        } else {
                            Some(Menu::Source)
                        };
                        cx.notify();
                    })
                },
                move |index, _, cx| {
                    pick.update(cx, |this, cx| {
                        this.menu = None;
                        if let Some((key, _)) = MappingSource::OPTIONS.get(index) {
                            match MappingSource::from_key(key) {
                                Ok(source) => {
                                    this.value.source = source;
                                    this.sync_fields(cx);
                                    this.publish(cx);
                                }
                                Err(error) => {
                                    this.error = Some(error.to_string());
                                    cx.notify();
                                }
                            }
                        }
                    })
                },
            ))
            .when(
                matches!(
                    self.value.source,
                    MappingSource::Vector { .. } | MappingSource::MajorAxis { .. }
                ),
                |el| {
                    el.child(
                        div()
                            .text_size(px(11.))
                            .text_color(ladder::muted_foreground())
                            .child(
                                if matches!(self.value.source, MappingSource::Vector { .. }) {
                                    "Direction · U right, V downstage, Z up"
                                } else {
                                    "Orient axis toward · U, V, Z"
                                },
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .children(self.direction.iter().cloned()),
                    )
                },
            )
            .when(
                matches!(self.value.source, MappingSource::Circle { .. }),
                |el| {
                    el.child(div().text_size(px(11.)).child("Circle origin (turns)"))
                        .child(self.origin.clone())
                },
            )
            .child(div().text_size(px(11.)).child("Mirror"))
            .child(luma_arg_select(
                format!("{}: Mirror", self.name),
                MIRRORS[mirror],
                &MIRRORS,
                self.menu == Some(Menu::Mirror),
                move |_, cx| {
                    mirror_toggle.update(cx, |this, cx| {
                        this.menu = if this.menu == Some(Menu::Mirror) {
                            None
                        } else {
                            Some(Menu::Mirror)
                        };
                        cx.notify();
                    })
                },
                move |index, _, cx| {
                    mirror_pick.update(cx, |this, cx| {
                        this.menu = None;
                        this.custom_mirror = index == 4;
                        this.value.mirror = match index {
                            0 => None,
                            1..=4 => Some(MirrorPlane {
                                normal: match index {
                                    1 => [1., 0., 0.],
                                    2 => [0., 1., 0.],
                                    3 => [0., 0., 1.],
                                    _ => this
                                        .value
                                        .mirror
                                        .as_ref()
                                        .map_or([1., 0., 0.], |plane| plane.normal),
                                },
                                offset: this.value.mirror.as_ref().map_or(0., |plane| plane.offset),
                            }),
                            _ => return,
                        };
                        this.sync_fields(cx);
                        this.publish(cx);
                    })
                },
            ))
            .when(mirror == 4, |el| {
                el.child(
                    div()
                        .text_size(px(11.))
                        .text_color(ladder::muted_foreground())
                        .child("Plane normal · U, V, Z"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .children(self.normal.iter().cloned()),
                )
            })
            .when(self.value.mirror.is_some(), |el| {
                el.child(
                    div()
                        .text_size(px(11.))
                        .child("Plane offset (m from selection center)"),
                )
                .child(self.offset.clone())
            })
            .child(
                div().flex().gap(px(6.)).child(
                    luma_toggle("Reverse", self.value.reverse)
                        .id("mapping-reverse")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.value.reverse = !this.value.reverse;
                            this.publish(cx);
                        }))
                        .agent_node(Role::Button, format!("{}: Reverse", self.name)),
                ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .text_size(px(12.))
                        .text_color(ladder::danger())
                        .child(error.clone())
                        .agent_node(Role::Text, error),
                )
            })
    }
}
