//! Authored groups are editable sets; automatic groups follow the stage.
use super::Patch;
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
        if state.group_busy {
            return;
        }
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

pub(super) fn editor(state: &Patch, app: &Entity<Luma>) -> AnyElement {
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
                                            s.selected = e.members.clone();
                                        }
                                    }
                                }
                                if let Some(Body::Patch(s)) = this.workspace.active_body() {
                                    this.highlight_venue_lights(s.selected.clone(), cx);
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
    body.into_any_element()
}

pub(super) fn strip(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let new = app.clone();
    let from_selection = app.clone();
    let mut section = div()
        .flex_none()
        .px(px(16.0))
        .pb(px(8.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_1().child(float::label("Groups")))
                .when(!state.selected.is_empty(), |d| {
                    d.child(
                        float::btn("Group selected", "group-selected")
                            .id("group-selected")
                            .on_click(move |_, window, cx| {
                                from_selection.update(cx, |this, cx| {
                                    this.edit_venue_group(None, true, window, cx)
                                })
                            })
                            .agent_node(Role::Button, "Group selected lights")
                            .agent_disabled(state.group_busy),
                    )
                })
                .child(
                    float::btn("Create group", "group-create")
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
        let mut chips = div()
            .id("venue-group-chips")
            .max_h(px(108.0))
            .overflow_y_scroll()
            .flex()
            .flex_wrap()
            .gap(px(4.0));
        // Authored sets first, followed by the automatic vocabulary.
        for group in data
            .groups
            .iter()
            .filter(|g| g.role.is_none())
            .chain(data.groups.iter().filter(|g| g.role.is_some()))
        {
            let id = group.id.clone();
            let path = group_path(&data.groups, &id);
            let click = app.clone();
            let active = state.group_editor.as_ref().and_then(|e| e.id.as_ref()) == Some(&id);
            chips = chips.child(
                div()
                    .child(
                        float::btn(
                            format!(
                                "{path} · {}{}",
                                group.fixtures.len(),
                                if group.role.is_some() { " · auto" } else { "" }
                            ),
                            format!("group-{id}"),
                        )
                        .id(SharedString::from(format!("group-{id}")))
                        .max_w_full()
                        .when(active, |d| d.bg(luma_ui::glass::card_selected_bg()))
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
        section = section.child(chips);
    }
    section.into_any_element()
}

pub(super) fn memberships(state: &Patch, fixture: &str, app: &Entity<Luma>) -> AnyElement {
    let mut chips = div().flex().flex_wrap().gap(px(4.0));
    if let Some(data) = &state.data {
        for group in data.groups.iter().filter(|g| g.role.is_none()) {
            let member = group.fixtures.iter().any(|id| id == fixture);
            let id = group.id.clone();
            let fixture = fixture.to_string();
            let click = app.clone();
            chips = chips.child(
                float::btn(group.label.replace('_', " "), format!("fixture-group-{id}"))
                    .id(SharedString::from(format!("fixture-group-{id}")))
                    .when(member, |d| d.bg(luma_ui::glass::card_selected_bg()))
                    .on_click(move |_, _, cx| {
                        click.update(cx, |this, cx| {
                            this.set_fixture_group(fixture.clone(), id.clone(), !member, cx)
                        })
                    })
                    .agent_node(
                        Role::Checkbox,
                        format!("Member of {}", group.label.replace('_', " ")),
                    )
                    .agent_focused(member)
                    .agent_disabled(state.group_busy),
            );
        }
    }
    chips.into_any_element()
}

impl Luma {
    fn set_fixture_group(
        &mut self,
        fixture: String,
        group: String,
        member: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        let Some(node) = state
            .data
            .as_ref()
            .and_then(|data| data.groups.iter().find(|g| g.id == group))
        else {
            return;
        };
        let venue = state.venue_id.clone();
        let (added, removed) = if member {
            (vec![fixture], vec![])
        } else {
            (vec![], vec![fixture])
        };
        let pending = self.library.save_venue_group(
            &venue,
            Some(&group),
            &node.label,
            None,
            &added,
            &removed,
        );
        state.group_busy = true;
        state.group_error = None;
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.patch_mut(&venue) {
                    state.group_busy = false;
                    state.group_error = result.err().map(|e| super::refusal_message(&e));
                }
                this.reload_patch(venue, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
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
