//! Groups are saved fixture collections. Generation only provides a starting set.
//!
//! The panel has no editing mode. A group's row selects its fixtures, its box
//! puts the selected fixtures in or takes them out, and both write at once.
//! The fixture table is never borrowed: what a click on a fixture means does
//! not depend on what the panel is doing.
use super::Patch;
use crate::{shell::Body, Luma};
use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, Context, Entity, Focusable, Point, SharedString, Subscription, Window,
};
use luma_lib::models::groups::GroupTreeNode;
use luma_ui::menu::ContextMenu;
use luma_ui::motion::FadeHover;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::TextInput;
use luma_ui::{float, glass, ladder};

/// The one name being typed: a new group's, or a rename.
pub(crate) struct Naming {
    /// The group being renamed, or `None` for a group not made yet.
    id: Option<String>,
    field: Entity<TextInput>,
    /// What the field read when it opened. Committing it again writes nothing.
    opened_on: String,
    _subscriptions: Vec<Subscription>,
}

/// How much of the selection a group holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Holds {
    None,
    Some,
    All,
}

/// A group's name as a person reads it. The stored name is snake_case because
/// a selection is typed by it.
fn spoken(group: &GroupTreeNode) -> String {
    group.name.replace('_', " ")
}

impl Patch {
    fn group(&self, id: &str) -> Option<&GroupTreeNode> {
        self.data.as_ref()?.groups.iter().find(|g| g.id == id)
    }

    fn holds(&self, group: &GroupTreeNode) -> Holds {
        let held = self
            .selected
            .iter()
            .filter(|id| group.fixtures.contains(id))
            .count();
        match held {
            0 => Holds::None,
            held if held == self.selected.len() => Holds::All,
            _ => Holds::Some,
        }
    }

    /// How many clips, Perform patterns and Perform group cells select `id`,
    /// or `None` when that could not be counted.
    fn uses(&self, id: &str) -> Option<usize> {
        let usage = self.data.as_ref()?.usage.as_ref()?;
        Some(
            usage
                .iter()
                .find(|used| used.group_id == id)
                .map_or(0, |used| {
                    used.clips + used.perform_patterns.len() + used.perform_cells
                }),
        )
    }
}

impl Luma {
    /// Select a group's fixtures, here and on the stage.
    pub(crate) fn select_venue_group(&mut self, id: &str, extend: bool, cx: &mut Context<Self>) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        let Some(members) = state.group(id).map(|group| group.fixtures.clone()) else {
            return;
        };
        if !extend {
            state.selected.clear();
        }
        state.selected.extend(members);
        state.anchor = None;
        let selected = state.selected.clone();
        self.highlight_venue_lights(selected, cx);
    }

    /// Put the selected fixtures in the group, or take them out if it already
    /// holds them all.
    fn toggle_group_membership(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy || state.selected.is_empty() {
            return;
        }
        let Some(group) = state.group(id) else {
            return;
        };
        let selected: Vec<String> = state
            .rows()
            .iter()
            .filter(|row| state.selected.contains(&row.id))
            .map(|row| row.id.clone())
            .collect();
        let (added, removed, said) = if state.holds(group) == Holds::All {
            let said = format!(
                "Took {} out of {}",
                super::plural(selected.len(), "fixture"),
                spoken(group)
            );
            (Vec::new(), selected, said)
        } else {
            let added: Vec<String> = selected
                .into_iter()
                .filter(|id| !group.fixtures.contains(id))
                .collect();
            let said = format!(
                "Put {} in {}",
                super::plural(added.len(), "fixture"),
                spoken(group)
            );
            (added, Vec::new(), said)
        };
        let label = spoken(group);
        self.write_venue_group(Some(id.to_string()), label, added, removed, said, cx);
    }

    /// Open the name field: on a group to rename it, or on none to make one
    /// from the selected fixtures.
    pub(crate) fn name_venue_group(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        state.group_menu = None;
        let opened_on = id
            .as_deref()
            .and_then(|id| state.group(id))
            .map(spoken)
            .unwrap_or_default();
        let field = cx.new(|cx| {
            let mut field = TextInput::search("Group name", cx);
            field.set_text(opened_on.clone(), cx);
            field
        });
        let edited = cx.subscribe(&field, |_: &mut Luma, _, _, cx| cx.notify());
        // Leaving the field is the other commit point, as it is for a
        // fixture's label. An empty name commits to nothing.
        let luma = cx.entity().downgrade();
        let blur = window.on_focus_out(&field.read(cx).focus_handle(cx), cx, move |_, _, cx| {
            luma.update(cx, |this, cx| this.commit_group_name(cx)).ok();
        });
        window.focus(&field.read(cx).focus_handle(cx), cx);
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        state.group_refusal = None;
        state.naming = Some(Naming {
            id,
            field,
            opened_on,
            _subscriptions: vec![edited, blur],
        });
        cx.notify();
    }

    /// Write the typed name down. A new group takes the selected fixtures.
    fn commit_group_name(&mut self, cx: &mut Context<Self>) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        if state.group_busy {
            return;
        }
        let Some(naming) = &state.naming else {
            return;
        };
        let label = naming.field.read(cx).text().trim().to_string();
        if label.is_empty() || label == naming.opened_on {
            state.naming = None;
            state.group_refusal = None;
            cx.notify();
            return;
        }
        let id = naming.id.clone();
        let (added, said) = if id.is_some() {
            (Vec::new(), format!("Renamed to {label}"))
        } else {
            let added: Vec<String> = state
                .rows()
                .iter()
                .filter(|row| state.selected.contains(&row.id))
                .map(|row| row.id.clone())
                .collect();
            let said = format!(
                "Made {label} with {}",
                super::plural(added.len(), "fixture")
            );
            (added, said)
        };
        self.write_venue_group(id, label, added, Vec::new(), said, cx);
    }

    /// One group write, then the reload that shows it. A name the backend will
    /// not take stays in its field with the reason under it.
    fn write_venue_group(
        &mut self,
        id: Option<String>,
        label: String,
        added: Vec<String>,
        removed: Vec<String>,
        said: String,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        let venue = state.venue_id.clone();
        let pending =
            self.library
                .save_venue_group(&venue, id.as_deref(), &label, &added, &removed);
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
                            state.naming = None;
                            state.say(said);
                        }
                        Err(error) => match super::refusal(&error) {
                            Some(message) if state.naming.is_some() => {
                                state.group_refusal = Some(message);
                            }
                            _ => failed = Some(error),
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

    fn open_group_menu(&mut self, id: String, at: Point<gpui::Pixels>, cx: &mut Context<Self>) {
        if let Some(Body::Patch(state)) = self.workspace.active_body_mut() {
            state.group_menu = Some((id, at));
        }
        cx.notify();
    }

    pub(crate) fn delete_venue_group(&mut self, venue: String, id: String, cx: &mut Context<Self>) {
        let pending = self.library.delete_venue_group(&id);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.report_failure("The group was not deleted", error, cx);
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

pub(super) fn panel(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let new = app.clone();
    let making = state.naming.as_ref().is_some_and(|n| n.id.is_none());
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
                .h(px(super::BAND_HEIGHT))
                .flex()
                .items_center()
                .px(px(12.0))
                .gap(px(8.0))
                .child(div().flex_1().text_size(px(12.5)).child("Groups"))
                .child(
                    float::btn("New group", "group-create")
                        .px(px(6.0))
                        .id("group-create")
                        .on_click(move |_, window, cx| {
                            new.update(cx, |this, cx| this.name_venue_group(None, window, cx))
                        })
                        .agent_node(Role::Button, "New group")
                        .agent_disabled(state.group_busy),
                ),
        );
    let Some(data) = &state.data else {
        return section
            .agent_node(Role::Card, "Venue groups")
            .into_any_element();
    };
    let mut rows = div()
        .id("venue-groups-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .px(px(4.0))
        .gap(px(2.0));
    if making {
        rows = rows.child(new_group(state, app));
    }
    if data.groups.is_empty() && !making {
        let generate = app.clone();
        rows = rows
            .child(float::empty_row(
                "No groups yet. Select fixtures, then New group.",
            ))
            .child(
                div().px(px(8.0)).child(
                    float::btn("Generate from stage", "generate-groups")
                        .id("generate-groups")
                        .on_click(move |_, _, cx| {
                            generate.update(cx, |this, cx| this.generate_groups(cx))
                        })
                        .agent_node(Role::Button, "Generate groups from stage")
                        .agent_disabled(state.group_busy),
                ),
            );
    }
    rows = rows.children(data.groups.iter().map(|group| group_row(state, group, app)));
    section = section.child(rows);
    section
        .children(
            state
                .group_menu
                .as_ref()
                .map(|(id, at)| group_menu(state, id, *at, app)),
        )
        .agent_node(Role::Card, "Venue groups")
        .into_any_element()
}

/// The name field, committed on `enter` and on the way out.
fn name_field(state: &Patch, naming: &Naming, app: &Entity<Luma>) -> AnyElement {
    let committed = app.clone();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .on_key_down(move |event, _, cx| {
            if event.keystroke.key != "enter" {
                return;
            }
            committed.update(cx, |this, cx| this.commit_group_name(cx));
            cx.stop_propagation();
        })
        .child(float::field().w_full().child(naming.field.clone()))
        .children(state.group_refusal.clone().map(luma_ui::notice::line))
        .agent_node(Role::Input, "Group name")
        .into_any_element()
}

/// The row a group not made yet is named in, saying what it will hold.
fn new_group(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let Some(naming) = &state.naming else {
        return div().into_any_element();
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(10.0))
        .py(px(7.0))
        .child(name_field(state, naming, app))
        .child(float::row_subtitle(if state.selected.is_empty() {
            "No fixtures selected: the group starts empty".to_string()
        } else {
            format!(
                "Holds the {} selected",
                super::plural(state.selected.len(), "fixture")
            )
        }))
        .agent_node(Role::Row, "New group")
        .into_any_element()
}

fn group_row(state: &Patch, group: &GroupTreeNode, app: &Entity<Luma>) -> AnyElement {
    let id = group.id.clone();
    let name = spoken(group);
    let holds = state.holds(group);
    // The row is lit when the selection is exactly this group: that is what a
    // click on it makes, and what tells two overlapping groups apart.
    let shown = !state.selected.is_empty()
        && holds == Holds::All
        && group.fixtures.len() == state.selected.len();
    let renaming = state
        .naming
        .as_ref()
        .filter(|naming| naming.id.as_deref() == Some(group.id.as_str()));

    let mut meta = super::plural(group.fixtures.len(), "fixture");
    match state.uses(&group.id) {
        Some(0) => meta.push_str(" · not used"),
        Some(uses) => meta.push_str(&format!(" · {}", super::plural(uses, "use"))),
        None => {}
    }

    let mut line = div()
        .id(SharedString::from(format!("group-{id}")))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(if shown {
            glass::card_selected_bg()
        } else {
            glass::wash(0.)
        })
        .when(!shown, |line| {
            line.hover(|style| style.bg(glass::glass_hover()))
                .fade_hover()
        });

    // The box is only a question about the selection, so it is only there
    // when there is one.
    if !state.selected.is_empty() {
        let toggled = app.clone();
        let toggle_id = id.clone();
        let count = super::plural(state.selected.len(), "selected fixture");
        let (mark, does) = match holds {
            Holds::All => (float::checkbox(true), format!("Take {count} out of {name}")),
            Holds::Some => (float::checkbox_partial(), format!("Put {count} in {name}")),
            Holds::None => (float::checkbox(false), format!("Put {count} in {name}")),
        };
        line = line.child(
            mark.id(SharedString::from(format!("group-holds-{id}")))
                .cursor_pointer()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    toggled.update(cx, |this, cx| this.toggle_group_membership(&toggle_id, cx));
                    cx.stop_propagation();
                })
                .agent_node(Role::Checkbox, does)
                .agent_focused(holds == Holds::All)
                .agent_disabled(state.group_busy),
        );
    }

    let body: AnyElement = match renaming {
        Some(naming) => name_field(state, naming, app),
        None => {
            let picked = app.clone();
            let pick_id = id.clone();
            div()
                .id(SharedString::from(format!("group-pick-{id}")))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .cursor_pointer()
                .child(div().text_size(px(12.5)).truncate().child(name.clone()))
                .child(float::row_subtitle(meta))
                .on_click(move |event, _, cx| {
                    let extend = event.modifiers().secondary() || event.modifiers().shift;
                    picked.update(cx, |this, cx| {
                        this.select_venue_group(&pick_id, extend, cx);
                    });
                })
                .agent_node(Role::Button, format!("Select group {name}"))
                .agent_focused(shown)
                .into_any_element()
        }
    };

    let menued = app.clone();
    line.child(body)
        .on_mouse_down(gpui::MouseButton::Right, move |event, _, cx| {
            let id = id.clone();
            let at = event.position;
            menued.update(cx, |this, cx| this.open_group_menu(id, at, cx));
        })
        .agent_node(Role::Row, format!("Group {name}"))
        .into_any_element()
}

/// A group's own menu: rename it, or delete it after saying what uses it.
fn group_menu(state: &Patch, id: &str, at: Point<gpui::Pixels>, app: &Entity<Luma>) -> AnyElement {
    let rename = app.clone();
    let rename_id = id.to_string();
    let remove = app.clone();
    let remove_id = id.to_string();
    let venue = state.venue_id.clone();
    let consequence = deleting(state, id);
    let closed = app.clone();
    let closed_venue = state.venue_id.clone();
    ContextMenu::new("group-menu", at)
        .item("Rename", move |window, cx| {
            let id = rename_id.clone();
            rename.update(cx, |this, cx| this.name_venue_group(Some(id), window, cx));
        })
        .separator()
        .destructive("Delete group", move |_, cx| {
            let confirm = crate::confirm::Confirm {
                title: "Delete group?".into(),
                body: consequence.clone(),
                verb: "Delete group".into(),
                action: crate::confirm::Action::DeleteGroup {
                    venue: venue.clone(),
                    id: remove_id.clone(),
                },
            };
            remove.update(cx, |this, cx| {
                if let Some(Body::Patch(state)) = this.workspace.active_body_mut() {
                    state.group_menu = None;
                }
                this.ask(confirm, cx);
            });
        })
        .render(move |_, cx| {
            let venue = closed_venue.clone();
            closed.update(cx, |this, cx| this.close_patch_menus(venue, cx));
        })
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
