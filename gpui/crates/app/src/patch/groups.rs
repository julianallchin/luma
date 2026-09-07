//! Authored groups are editable sets; automatic groups follow the stage.
use super::{Patch, Section};
use crate::{shell::Body, Luma};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, Focusable, SharedString, Subscription, Window};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::TextInput;
use luma_ui::{float, ladder};
use std::collections::HashSet;

pub(crate) struct Editor {
    id: Option<String>,
    name: Entity<TextInput>,
    before: HashSet<String>,
    members: HashSet<String>,
    manual: bool,
    parent: String,
    original_parent: String,
    pub(crate) parent_open: bool,
    _subscription: Subscription,
}

impl Luma {
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
        let group = id
            .as_ref()
            .and_then(|id| state.data.as_ref()?.groups.iter().find(|g| &g.id == id));
        let label = group.map_or(String::new(), |g| g.label.replace('_', " "));
        let manual = group.is_none_or(|g| g.role.is_none());
        let parent = group.and_then(|g| g.parent_id.clone()).unwrap_or_default();
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
        state.section = Section::Groups;
        state.group_error = None;
        state.group_editor = Some(Editor {
            id,
            name,
            before,
            members,
            manual,
            original_parent: parent.clone(),
            parent,
            parent_open: false,
            _subscription: subscription,
        });
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
        let pending = self.library.save_venue_group(
            &venue,
            editor.id.as_deref(),
            &label,
            (editor.parent != editor.original_parent).then_some(editor.parent.as_str()),
            &added,
            &removed,
        );
        state.group_busy = true;
        state.group_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.patch_mut(&venue) {
                    state.group_busy = false;
                    match result {
                        Ok(()) => {
                            state.group_editor = None;
                            state.say("Group saved");
                        }
                        Err(error) => state.group_error = Some(super::refusal_message(&error)),
                    }
                }
                this.reload_patch(venue, cx);
            })
            .ok();
        })
        .detach();
    }
}

pub(super) fn groups(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let mut body = div()
        .id("venue-groups")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(px(6.0));
    if let Some(error) = &state.group_error {
        body = body.child(luma_ui::plate(error.clone(), ladder::danger()));
    }
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
            .child(parent_picker(state, editor, app))
            .child(
                div()
                    .flex()
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
                                            s.group_error = None;
                                        }
                                    }
                                    cx.notify();
                                })
                            })
                            .agent_node(Role::Button, "Cancel group edit"),
                    ),
            );
        if editor.manual {
            body = body.child(float::label(format!("{} fixtures", editor.members.len())));
            for fixture in state.rows() {
                let picked = editor.members.contains(&fixture.id);
                let click = app.clone();
                let id = fixture.id.clone();
                let name = fixture
                    .label
                    .clone()
                    .unwrap_or_else(|| fixture.model.clone());
                body = body.child(
                    div()
                        .id(SharedString::from(format!("group-member-{id}")))
                        .flex_none()
                        .flex()
                        .gap(px(10.0))
                        .py(px(8.0))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            click.update(cx, |this, cx| {
                                if let Some(Body::Patch(s)) = this.workspace.active_body_mut() {
                                    if !s.group_busy {
                                        if let Some(e) = &mut s.group_editor {
                                            if !e.members.remove(&id) {
                                                e.members.insert(id.clone());
                                            }
                                        }
                                    }
                                }
                                cx.notify();
                            })
                        })
                        .child(
                            div()
                                .text_color(if picked {
                                    ladder::accent().into()
                                } else {
                                    ladder::foreground_alpha(0.35)
                                })
                                .child(if picked { "●" } else { "○" }),
                        )
                        .child(div().text_size(px(12.0)).child(name.clone()))
                        .agent_node(Role::Checkbox, format!("Include {name}"))
                        .agent_focused(picked),
                );
            }
        } else {
            body = body.child(float::empty_row("Membership follows stage geometry."));
        }
        return body.into_any_element();
    }
    let new = app.clone();
    body = body.child(
        div().flex().child(
            float::btn("Create group", "group-create")
                .id("group-create")
                .on_click(move |_, window, cx| {
                    new.update(cx, |this, cx| {
                        this.edit_venue_group(None, false, window, cx)
                    })
                })
                .agent_node(Role::Button, "Create group"),
        ),
    );
    if let Some(data) = &state.data {
        for group in &data.groups {
            body = body.child(group_row(group, &data.groups, app));
        }
        if data.groups.is_empty() {
            body = body.child(float::empty_row("No groups"));
        }
    } else if let Some(error) = &state.error {
        body = body.child(luma_ui::plate(error.clone(), ladder::danger()));
    } else {
        body = body.child(float::empty_row("Loading groups…"));
    }
    body.into_any_element()
}

fn group_row(
    group: &luma_lib::models::groups::GroupTreeNode,
    tree: &[luma_lib::models::groups::GroupTreeNode],
    app: &Entity<Luma>,
) -> AnyElement {
    let edit = app.clone();
    let id = group.id.clone();
    let label = group.label.replace('_', " ");
    let path = group_path(tree, &id);
    let select = app.clone();
    let members: HashSet<String> = group.fixtures.iter().cloned().collect();
    let parent = group
        .parent_id
        .as_deref()
        .map(|id| group_path(tree, id))
        .unwrap_or_default();
    div()
        .id(SharedString::from(format!("venue-group-{id}")))
        .flex_none()
        .py(px(5.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .id(SharedString::from(format!("select-group-{id}")))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    select.update(cx, |this, cx| {
                        if let Some(Body::Patch(s)) = this.workspace.active_body_mut() {
                            s.selected = members.clone();
                        }
                        this.highlight_venue_lights(members.clone(), cx);
                    })
                })
                .child(div().text_size(px(12.5)).truncate().child(label.clone()))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::foreground_alpha(0.45))
                        .truncate()
                        .child(format!(
                            "{}{}{}",
                            super::plural(group.fixtures.len(), "fixture"),
                            if parent.is_empty() { "" } else { " · " },
                            parent
                        )),
                ),
        )
        .children(group.role.map(|_| {
            div()
                .text_size(px(10.0))
                .text_color(ladder::muted_foreground())
                .child("Auto")
        }))
        .child(
            float::btn("Edit", format!("edit-{id}"))
                .id(SharedString::from(format!("edit-{id}")))
                .on_click(move |_, window, cx| {
                    edit.update(cx, |this, cx| {
                        this.edit_venue_group(Some(id.clone()), false, window, cx)
                    })
                })
                .agent_node(Role::Button, format!("Edit group {path}")),
        )
        .agent_node(Role::Row, format!("Group {path}"))
        .into_any_element()
}

fn group_path(tree: &[luma_lib::models::groups::GroupTreeNode], id: &str) -> String {
    let mut labels = Vec::new();
    let mut at = Some(id);
    for _ in 0..tree.len() {
        let Some(group) = at.and_then(|id| tree.iter().find(|g| g.id == id)) else {
            break;
        };
        labels.push(group.label.replace('_', " "));
        at = group.parent_id.as_deref();
    }
    labels.reverse();
    labels.join(" / ")
}

fn parent_picker(state: &Patch, editor: &Editor, app: &Entity<Luma>) -> AnyElement {
    let tree = state.data.as_ref().map_or(&[][..], |d| d.groups.as_slice());
    let mut options = vec![(String::new(), "Top level".to_string())];
    for group in tree {
        let mut at = Some(group.id.as_str());
        let mut descendant = false;
        for _ in 0..tree.len() {
            if at.is_some() && at == editor.id.as_deref() {
                descendant = true;
                break;
            }
            at = at
                .and_then(|id| tree.iter().find(|g| g.id == id))
                .and_then(|g| g.parent_id.as_deref());
        }
        if !descendant {
            options.push((group.id.clone(), group_path(tree, &group.id)));
        }
    }
    let current = options
        .iter()
        .find(|(id, _)| id == &editor.parent)
        .map_or("Top level", |(_, name)| name.as_str());
    let labels: Vec<&str> = options.iter().map(|(_, name)| name.as_str()).collect();
    let ids: Vec<String> = options.iter().map(|(id, _)| id.clone()).collect();
    let toggle = app.clone();
    let pick = app.clone();
    float::field_row(
        "Parent",
        luma_ui::arg::select::luma_arg_select(
            "group-parent",
            current,
            &labels,
            editor.parent_open,
            move |_, cx| {
                toggle.update(cx, |this, cx| {
                    if let Some(Body::Patch(s)) = this.workspace.active_body_mut() {
                        if !s.group_busy {
                            if let Some(e) = &mut s.group_editor {
                                e.parent_open = !e.parent_open;
                            }
                        }
                    }
                    cx.notify();
                })
            },
            move |index, _, cx| {
                pick.update(cx, |this, cx| {
                    if let Some(Body::Patch(s)) = this.workspace.active_body_mut() {
                        if !s.group_busy {
                            if let Some(e) = &mut s.group_editor {
                                e.parent = ids[index].clone();
                                e.parent_open = false;
                            }
                        }
                    }
                    cx.notify();
                })
            },
        ),
    )
    .into_any_element()
}
