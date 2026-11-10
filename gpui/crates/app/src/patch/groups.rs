//! Groups are saved fixture collections. Generation only provides a starting set.
use super::Patch;
use crate::{shell::Body, Luma};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, Focusable, SharedString, Subscription, Window};
use luma_ui::motion::FadeHover;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::TextInput;
use luma_ui::{float, ladder};
use std::collections::HashSet;

pub(crate) struct Editor {
    id: Option<String>,
    name: Entity<TextInput>,
    before: HashSet<String>,
    members: HashSet<String>,
    _subscription: Subscription,
}

impl Luma {
    pub(super) fn toggle_venue_group_member(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        let Some(editor) = state.group_editor.as_mut() else {
            return;
        };
        if !editor.members.remove(id) {
            editor.members.insert(id.to_string());
        }
        state.selected = editor.members.clone();
        let selected = state.selected.clone();
        self.highlight_venue_lights(selected, cx);
    }

    pub(crate) fn edit_venue_group(
        &mut self,
        id: Option<String>,
        from_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        let group = id
            .as_ref()
            .and_then(|id| state.data.as_ref()?.groups.iter().find(|g| &g.id == id));
        let label = group.map_or(String::new(), |g| g.name.replace('_', " "));
        let before: HashSet<String> = group
            .map(|g| g.fixtures.iter().cloned().collect())
            .unwrap_or_default();
        let members = if from_selection {
            state.selected.clone()
        } else {
            before.clone()
        };
        let name = cx.new(|cx| {
            let mut field = TextInput::search("Group name", cx);
            field.set_text(label, cx);
            field
        });
        let subscription = cx.subscribe(&name, |_: &mut Luma, _, _, cx| cx.notify());
        window.focus(&name.read(cx).focus_handle(cx), cx);

        state.group_refusal = None;
        state.group_editor = Some(Editor {
            id,
            name,
            before,
            members,
            _subscription: subscription,
        });
        let selected = state.group_editor.as_ref().unwrap().members.clone();
        state.selected = selected.clone();
        self.highlight_venue_lights(selected, cx);
        cx.notify();
    }

    fn save_venue_group(&mut self, cx: &mut Context<Self>) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        let Some(editor) = &state.group_editor else {
            return;
        };
        let label = editor.name.read(cx).text().trim().to_string();
        let venue = state.venue_id.clone();
        let added: Vec<String> = editor.members.difference(&editor.before).cloned().collect();
        let removed: Vec<String> = editor.before.difference(&editor.members).cloned().collect();
        let pending =
            self.library
                .save_venue_group(&venue, editor.id.as_deref(), &label, &added, &removed);
        state.group_busy = true;
        state.group_refusal = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut failed = None;
                if let Some(state) = this.patch_mut(&venue) {
                    state.group_busy = false;
                    match result {
                        Ok(()) => {
                            state.group_editor = None;
                            state.say("Group saved");
                        }
                        Err(error) => match super::refusal(&error) {
                            Some(message) => state.group_refusal = Some(message),
                            None => failed = Some(error),
                        },
                    }
                }
                if let Some(error) = failed {
                    this.report_failure("The group was not saved", error, cx);
                }
                this.reload_patch(venue, cx);
            })
            .ok();
        })
        .detach();
    }
}

fn editor(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let mut body = div()
        .id("venue-groups")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(px(6.0));
    if let Some(editor) = &state.group_editor {
        let save = app.clone();
        let cancel = app.clone();
        body = body
            .child(float::label(if editor.id.is_some() {
                "Edit group"
            } else {
                "New group"
            }))
            .child(editor.name.clone())
            .children(state.group_refusal.clone().map(luma_ui::notice::line))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(
                        float::btn_primary(if state.group_busy {
                            "Saving…"
                        } else {
                            "Save group"
                        })
                        .id("group-save")
                        .on_click(move |_, _, cx| {
                            save.update(cx, |this, cx| this.save_venue_group(cx))
                        })
                        .agent_node(Role::Button, "Save group"),
                    )
                    .child(
                        float::btn("Cancel", "group-cancel")
                            .id("group-cancel")
                            .on_click(move |_, _, cx| {
                                cancel.update(cx, |this, cx| {
                                    if let Some(Body::Patch(s)) = this.workspace.active_body_mut() {
                                        if !s.group_busy {
                                            s.group_editor = None;
                                            s.group_refusal = None;
                                        }
                                    }
                                    cx.notify();
                                })
                            })
                            .agent_node(Role::Button, "Cancel group edit"),
                    ),
            );
        body = body.child(float::label(format!(
            "{} fixtures selected",
            editor.members.len()
        )));
        if let Some(id) = &editor.id {
            let id = id.clone();
            let venue = state.venue_id.clone();
            let consequence = deleting(state, &id);
            let remove = app.clone();
            body = body.child(
                float::btn("Delete group", "delete-venue-group")
                    .id("delete-venue-group")
                    .on_click(move |_, _, cx| {
                        remove.update(cx, |this, cx| {
                            this.ask(
                                crate::confirm::Confirm {
                                    title: "Delete group?".into(),
                                    body: consequence.clone(),
                                    verb: "Delete group".into(),
                                    action: crate::confirm::Action::DeleteGroup {
                                        venue: venue.clone(),
                                        id: id.clone(),
                                    },
                                },
                                cx,
                            );
                        })
                    })
                    .agent_node(Role::Button, "Delete group"),
            );
        }
        return body.into_any_element();
    }
    body.into_any_element()
}

pub(super) fn panel(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let new = app.clone();
    let generate = app.clone();
    let from_selection = app.clone();
    let mut section = div()
        .flex_none()
        .w(gpui::relative(0.30))
        .min_w(px(200.0))
        .max_w(px(320.0))
        .h_full()
        .overflow_hidden()
        .border_l_1()
        .border_color(ladder::trim())
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .px(px(12.0))
                .py(px(8.0))
                .gap(px(8.0))
                .child(div().flex_1().text_size(px(12.5)).child("Groups"))
                .when(
                    !state.selected.is_empty() && state.group_editor.is_none(),
                    |d| {
                        d.child(
                            float::btn("From selection", "group-selected")
                                .px(px(6.0))
                                .id("group-selected")
                                .on_click(move |_, window, cx| {
                                    from_selection.update(cx, |this, cx| {
                                        this.edit_venue_group(None, true, window, cx)
                                    })
                                })
                                .agent_node(Role::Button, "Group selected lights")
                                .agent_disabled(state.group_busy),
                        )
                    },
                )
                .child(
                    float::btn("New", "group-create")
                        .px(px(6.0))
                        .id("group-create")
                        .on_click(move |_, window, cx| {
                            new.update(cx, |this, cx| {
                                this.edit_venue_group(None, false, window, cx)
                            })
                        })
                        .agent_node(Role::Button, "Create group")
                        .agent_disabled(state.group_busy),
                ),
        );
    if let Some(data) = &state.data {
        if data.groups.is_empty() {
            section = section.child(
                float::btn("Generate from stage", "generate-groups")
                    .id("generate-groups")
                    .on_click(move |_, _, cx| {
                        generate.update(cx, |this, cx| this.generate_groups(cx))
                    })
                    .agent_node(Role::Button, "Generate groups from stage"),
            );
        }
    }
    if state.group_editor.is_some() {
        return section
            .child(editor(state, app))
            .agent_node(Role::Card, "Venue groups")
            .into_any_element();
    }
    if let Some(data) = &state.data {
        let mut rows = div()
            .id("venue-groups-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(px(4.0))
            .gap(px(2.0));
        for group in &data.groups {
            let id = group.id.clone();
            let path = group.name.replace('_', " ");
            let click = app.clone();
            rows = rows.child(
                div()
                    .flex_none()
                    .child(
                        div()
                            .id(SharedString::from(format!("group-{id}")))
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(10.0))
                            .py(px(7.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .bg(luma_ui::glass::wash(0.))
                            .hover(|style| style.bg(luma_ui::glass::glass_hover()))
                            .fade_hover()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .truncate()
                                            .child(group.name.replace('_', " ")),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.0))
                                    .text_color(ladder::foreground_alpha(0.45))
                                    .child(group.fixtures.len().to_string()),
                            )
                            .on_click(move |_, window, cx| {
                                click.update(cx, |this, cx| {
                                    this.edit_venue_group(Some(id.clone()), false, window, cx)
                                })
                            })
                            .agent_node(Role::Button, format!("Edit group {path}"))
                            .agent_disabled(state.group_busy),
                    )
                    .agent_node(Role::Row, format!("Group {path}")),
            );
        }
        section = section.child(rows);
    }
    section
        .agent_node(Role::Card, "Venue groups")
        .into_any_element()
}

impl Luma {
    pub(crate) fn delete_venue_group(&mut self, venue: String, id: String, cx: &mut Context<Self>) {
        let pending = self.library.delete_venue_group(&id);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        if let Some(state) = this.patch_mut(&venue) {
                            state.group_editor = None;
                        }
                    }
                    Err(error) => this.report_failure("The group was not deleted", error, cx),
                }
                this.reload_patch(venue, cx);
            })
            .ok();
        })
        .detach();
    }
    fn generate_groups(&mut self, cx: &mut Context<Self>) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        state.group_busy = true;
        let venue = state.venue_id.clone();
        let pending = self.library.generate_venue_groups(&venue);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.patch_mut(&venue) {
                    state.group_busy = false;
                }
                if let Err(error) = result {
                    this.report_failure("Groups were not generated", error, cx);
                }
                this.reload_patch(venue, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

/// What deleting group `id` does, for the question asked first.
///
/// A selection names the group itself, so everything that selects it is left
/// selecting nothing in its place: the sentence says how much that is, or
/// that it could not be counted — never that nothing uses it when that is
/// not known.
fn deleting(state: &Patch, id: &str) -> SharedString {
    let Some(usage) = state.data.as_ref().and_then(|data| data.usage.as_ref()) else {
        return "Could not check which scores use this group. Anything that selects it will \
                light nothing there. The fixtures stay in the venue."
            .into();
    };
    let Some(used) = usage.iter().find(|used| used.group_id == id) else {
        return "Nothing selects this group. The fixtures stay in the venue.".into();
    };
    let mut places = Vec::new();
    if used.clips > 0 {
        places.push(format!(
            "{} across {}",
            super::plural(used.clips, "time"),
            super::plural(used.scores.len(), "score")
        ));
    }
    if !used.perform_patterns.is_empty() {
        places.push(format!(
            "by {}",
            super::plural(used.perform_patterns.len(), "Perform pattern")
        ));
    }
    if used.perform_cells > 0 {
        places.push(format!(
            "on {}",
            super::plural(used.perform_cells, "Perform group cell")
        ));
    }
    format!(
        "It is used {}. Deleting this group will break them: each will light nothing there. \
         The fixtures stay in the venue.",
        places.join(", ")
    )
    .into()
}
