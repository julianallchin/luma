//! The sidebar's folders: a venue's named sets of its songs
//! (`docs/specs/venue-tabs.md`, phase 2).
//!
//! A folder holds links, not songs. A song in a folder is the same song as
//! under All songs: pressing it opens the same scores level, so a song in two
//! folders shows the same scores in both. Deleting a folder deletes only its
//! links.
//!
//! A folder row opens and closes on a press. A right-click on it offers rename
//! and delete; a right-click on a song offers the folders to put it in.

use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::glass;
use luma_ui::node::{Instrument, Role};
use luma_ui::text_input::{self, TextInput, DRAFT_CONTEXT};

use luma_lib::models::folders::Folder;

use super::{slot, Tracks, GAP, ROW_HEIGHT};
use crate::agent::TabStatus;
use crate::library::LibraryError;
use crate::tabs::Target;
use crate::Luma;

/// What a right-click raised, and over what.
///
/// Ids rather than snapshots of the rows: the folders are re-read after every
/// change, and the menu acts on whatever the folder is by then.
pub(crate) enum Menu {
    Folder {
        /// Window space — what a right-click hands you.
        at: Point<Pixels>,
        folder_id: String,
    },
    Song {
        at: Point<Pixels>,
        track_id: String,
    },
}

/// An inline rename: the folder, the live field, and what the name read when
/// the caret arrived. A commit that matches it writes nothing, so Enter and
/// blur can both commit without a second write.
pub(crate) struct Rename {
    pub(super) folder_id: String,
    pub(super) field: Entity<TextInput>,
    opened_on: String,
    _subscriptions: Vec<Subscription>,
}

/// What a new folder is called until it is renamed.
const NEW_FOLDER: &str = "New folder";

impl Tracks {
    /// The songs of `folder` the venue has, whatever the filters say.
    fn songs_in(&self, folder: &Folder) -> usize {
        self.rows
            .iter()
            .filter(|row| row.is_in_venue && folder.track_ids.contains(&row.id))
            .count()
    }
}

// -- reads and writes -----------------------------------------------------------

impl Luma {
    /// Re-read the venue's folders. Admitted only while the same venue load
    /// still owns the sidebar, the rule its rows follow.
    pub(crate) fn reload_folders(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.sidebar else {
            return;
        };
        let (venue, generation) = (state.venue_id.clone(), state.load_generation);
        let pending = self.library.folders(&venue);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(state) = this
                    .sidebar
                    .as_mut()
                    .filter(|state| state.venue_id == venue && state.load_generation == generation)
                else {
                    return;
                };
                match result {
                    Ok(folders) => {
                        state.folders = folders.into();
                        state.refilter();
                    }
                    Err(error) => state.folder_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run a folder write, then re-read the folders: the listing is taken
    /// after the write, or the change is not in it yet.
    fn change_folders(
        &mut self,
        pending: impl Future<Output = Result<(), LibraryError>> + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.sidebar else {
            return;
        };
        let venue = state.venue_id.clone();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(state) = this
                    .sidebar
                    .as_mut()
                    .filter(|state| state.venue_id == venue)
                else {
                    return;
                };
                state.folder_error = result.err().map(|error| error.to_string());
                this.reload_folders(cx);
            })
            .ok();
        })
        .detach();
    }

    fn toggle_folder(&mut self, folder_id: &str, cx: &mut Context<Self>) {
        let Some(state) = &mut self.sidebar else {
            return;
        };
        if !state.expanded.remove(folder_id) {
            state.expanded.insert(folder_id.to_string());
        }
        state.refilter();
        cx.notify();
    }

    /// Make a folder and put the caret in its name.
    fn create_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = &self.sidebar else {
            return;
        };
        let venue = state.venue_id.clone();
        let pending = self.library.create_folder(&venue, NEW_FOLDER);
        cx.spawn_in(window, async move |this, cx| {
            let created = pending.await;
            this.update_in(cx, |this, window, cx| {
                let Some(state) = this
                    .sidebar
                    .as_mut()
                    .filter(|state| state.venue_id == venue)
                else {
                    return;
                };
                match created {
                    // Listed at once, from the row the backend wrote, so the
                    // field has a row to sit in; the next read replaces it.
                    Ok(folder) => {
                        state.folder_error = None;
                        let id = folder.id.clone();
                        let mut folders = state.folders.to_vec();
                        folders.push(folder);
                        folders.sort_by_key(|folder| folder.name.to_lowercase());
                        state.folders = folders.into();
                        state.refilter();
                        this.start_folder_rename(id, window, cx);
                    }
                    Err(error) => state.folder_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_folder_rename(
        &mut self,
        folder_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.sidebar.as_ref().and_then(|state| {
            state
                .folders
                .iter()
                .find(|folder| folder.id == folder_id)
                .map(|folder| folder.name.clone())
        }) else {
            return;
        };
        let field = cx.new(|cx| {
            let mut input = TextInput::search("Folder name", cx);
            input.set_text(name.clone(), cx);
            input
        });
        let keys = cx.subscribe(
            &field,
            |this: &mut Luma, _, event: &text_input::Event, cx| match event {
                text_input::Event::Submitted | text_input::Event::Blurred => {
                    this.finish_folder_rename(true, cx)
                }
                text_input::Event::Cancelled => this.finish_folder_rename(false, cx),
                _ => cx.notify(),
            },
        );
        // Focus leaving by the keyboard, such as Tab. Whichever of this and
        // `Blurred` comes second finds nothing to save.
        let luma = cx.entity().downgrade();
        let blur = window.on_focus_out(&field.read(cx).focus_handle(cx), cx, move |_, _, cx| {
            luma.update(cx, |this, cx| this.finish_folder_rename(true, cx))
                .ok();
        });
        let focus = field.read(cx).focus_handle(cx);
        if let Some(state) = &mut self.sidebar {
            state.menu = None;
            state.rename = Some(Rename {
                folder_id,
                field,
                opened_on: name,
                _subscriptions: vec![keys, blur],
            });
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// End the rename. `save` writes the typed name when it is a different,
    /// non-empty name; otherwise nothing is written.
    fn finish_folder_rename(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.sidebar.as_mut().and_then(|state| state.rename.take()) else {
            return;
        };
        cx.notify();
        let name = rename.field.read(cx).text().trim().to_string();
        if !save || name.is_empty() || name == rename.opened_on {
            return;
        }
        let pending = self.library.rename_folder(&rename.folder_id, &name);
        self.change_folders(pending, cx);
    }

    pub(crate) fn open_sidebar_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.sidebar {
            state.menu = Some(menu);
            cx.notify();
        }
    }

    /// Close it, reporting whether there was one.
    fn close_sidebar_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = &mut self.sidebar else {
            return false;
        };
        if state.menu.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    /// The delete gesture: straight through when the folder holds no song,
    /// and through [`crate::confirm`] when it holds some — the same rule a
    /// score's delete follows.
    fn request_delete_folder(&mut self, folder_id: String, cx: &mut Context<Self>) {
        self.close_sidebar_menu(cx);
        let Some((name, songs)) = self.sidebar.as_ref().and_then(|state| {
            let folder = state.folders.iter().find(|folder| folder.id == folder_id)?;
            Some((folder.name.clone(), state.songs_in(folder)))
        }) else {
            return;
        };
        if songs == 0 {
            self.delete_folder(folder_id, cx);
            return;
        }
        self.ask(
            crate::confirm::Confirm {
                title: format!("Delete the folder {name}?").into(),
                body: format!(
                    "It holds {songs} {}. The songs and their scores stay in the venue.",
                    if songs == 1 { "song" } else { "songs" }
                )
                .into(),
                verb: "Delete folder".into(),
                action: crate::confirm::Action::DeleteFolder {
                    folder_id: folder_id.into(),
                },
            },
            cx,
        );
    }

    pub(crate) fn delete_folder(&mut self, folder_id: String, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.sidebar {
            state.expanded.remove(&folder_id);
        }
        let pending = self.library.delete_folder(&folder_id);
        self.change_folders(pending, cx);
    }

    fn set_folder_track(
        &mut self,
        folder_id: &str,
        track_id: &str,
        linked: bool,
        cx: &mut Context<Self>,
    ) {
        let pending = self.library.set_folder_track(folder_id, track_id, linked);
        self.change_folders(pending, cx);
    }
}

// -- rendering ----------------------------------------------------------------

/// Each folder's status dot, by folder: the strongest dot among the score
/// tabs of its songs, while the folder is closed. Open, it is the songs under
/// it that are on screen.
pub(super) fn statuses(
    state: &Tracks,
    statuses: &HashMap<Target, TabStatus>,
) -> Rc<[Option<TabStatus>]> {
    state
        .folders
        .iter()
        .map(|folder| {
            if state.expanded.contains(&folder.id) {
                return None;
            }
            statuses
                .iter()
                .filter_map(|(target, status)| match target {
                    Target::Score { venue, track, .. }
                        if *venue == state.venue_id && folder.track_ids.contains(track) =>
                    {
                        Some(*status)
                    }
                    _ => None,
                })
                .reduce(TabStatus::max)
        })
        .collect()
}

/// One folder: the disclosure chevron, the name (or the field typing it), how
/// many songs the filters admit, and the dot of its songs' tabs.
pub(super) fn folder_row(
    folder: &Folder,
    songs: usize,
    open: bool,
    status: Option<TabStatus>,
    field: Option<&Entity<TextInput>>,
    app: &Entity<Luma>,
) -> AnyElement {
    let key = SharedString::from(format!("folder-{}", folder.id));
    let toggled = app.clone();
    let raised = app.clone();
    let (toggle_id, menu_id) = (folder.id.clone(), folder.id.clone());
    let label = format!("Folder {}", folder.name);
    slot(
        div()
            .id(key.clone())
            .w_full()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(GAP))
            .px(px(float::ROW_INSET))
            .rounded(px(luma_ui::radius::ROW))
            .cursor_pointer()
            .bg(luma_ui::motion::hover_blend(
                &key,
                glass::wash(0.),
                glass::glass_hover(),
            ))
            .on_hover(luma_ui::motion::hover_listener(key.clone()))
            .on_click(move |_, _, cx| {
                toggled.update(cx, |this, cx| this.toggle_folder(&toggle_id, cx));
            })
            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let menu = Menu::Folder {
                    at: event.position,
                    folder_id: menu_id.clone(),
                };
                raised.update(cx, |this, cx| this.open_sidebar_menu(menu, cx));
            })
            .child(
                gpui_component::Icon::new(if open {
                    luma_ui::icons::IconName::ChevronDown
                } else {
                    luma_ui::icons::IconName::ChevronRight
                })
                .size(px(11.))
                .text_color(glass::ink(0.55)),
            )
            .child(match field {
                Some(field) => name_field(folder, field),
                None => div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(glass::ink(0.85))
                    .child(folder.name.clone())
                    .into_any_element(),
            })
            .children(status.map(|status| crate::agent::status_dot(status, &label)))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(glass::ink(0.45))
                    .child(songs.to_string())
                    .agent_node(Role::Text, format!("{label} songs: {songs}")),
            )
            .agent_node(Role::Row, label),
    )
    .into_any_element()
}

/// The name as a live field. The draft context gives the field Enter and
/// Escape ahead of the sidebar's own keys; presses inside it stop here, so
/// placing the caret does not close the folder.
fn name_field(folder: &Folder, field: &Entity<TextInput>) -> AnyElement {
    float::field()
        .id(SharedString::from(format!(
            "folder-name-field-{}",
            folder.id
        )))
        .key_context(DRAFT_CONTEXT)
        .flex_1()
        .min_w(px(0.))
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(div().w_full().child(field.clone()))
        .agent_node(Role::Input, "Folder name")
        .into_any_element()
}

/// Make another folder.
pub(super) fn new_folder_row(app: &Entity<Luma>) -> AnyElement {
    let app = app.clone();
    slot(
        float::menu_row(RowState::Rest, "new-folder")
            .id("new-folder")
            .w_full()
            .on_click(move |_, window, cx| {
                app.update(cx, |this, cx| this.create_folder(window, cx));
            })
            .child(
                gpui_component::Icon::new(luma_ui::icons::IconName::Plus)
                    .size(px(11.))
                    .text_color(glass::ink(0.55)),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .text_color(glass::ink(0.7))
                    .child(NEW_FOLDER),
            )
            .agent_node(Role::Button, NEW_FOLDER),
    )
    .into_any_element()
}

/// The menu a right-click raised, if one is up.
pub(super) fn menu(state: &Tracks, app: &Entity<Luma>) -> Option<AnyElement> {
    match state.menu.as_ref()? {
        Menu::Folder { at, folder_id } => {
            let (renamed, deleted, dismissed) = (app.clone(), app.clone(), app.clone());
            let (rename_id, delete_id) = (folder_id.clone(), folder_id.clone());
            Some(
                luma_ui::menu::ContextMenu::new("folder-menu", *at)
                    .item("Rename", move |window, cx| {
                        let folder_id = rename_id.clone();
                        renamed.update(cx, |this, cx| {
                            this.start_folder_rename(folder_id, window, cx)
                        });
                    })
                    .destructive("Delete folder", move |_, cx| {
                        let folder_id = delete_id.clone();
                        deleted.update(cx, |this, cx| this.request_delete_folder(folder_id, cx));
                    })
                    .render(move |_, cx| {
                        dismissed.update(cx, |this, cx| {
                            this.close_sidebar_menu(cx);
                        });
                    }),
            )
        }
        Menu::Song { at, track_id } => Some(song_menu(state, *at, track_id, app)),
    }
}

/// The folders a song can be in, each with a checkbox. Ticking one stays in
/// the menu, so a song can be put in several folders at once.
fn song_menu(state: &Tracks, at: Point<Pixels>, track_id: &str, app: &Entity<Luma>) -> AnyElement {
    let dismissed = app.clone();
    let card = float::popover_card()
        .min_w(px(SONG_MENU_WIDTH))
        .child(float::section_heading("Folders").pt(px(4.)))
        .when(state.folders.is_empty(), |card| {
            card.child(float::empty_row("No folders yet"))
        })
        .children(state.folders.iter().map(|folder| {
            let linked = folder.track_ids.iter().any(|id| id == track_id);
            let key = SharedString::from(format!("song-folder-{}", folder.id));
            let app = app.clone();
            let (folder_id, track_id) = (folder.id.clone(), track_id.to_string());
            float::menu_row(RowState::Rest, key.clone())
                .id(key)
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.set_folder_track(&folder_id, &track_id, !linked, cx)
                    });
                })
                .child(float::checkbox(linked))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .child(folder.name.clone()),
                )
                .agent_node(Role::Checkbox, folder.name.clone())
        }));
    float::anchored_at(
        "song-menu",
        at,
        Dismiss::on_press_out(move |_, cx| {
            dismissed.update(cx, |this, cx| {
                this.close_sidebar_menu(cx);
            });
        }),
        card.agent_node(Role::Card, "Song folders")
            .into_any_element(),
    )
}

/// Wide enough for a folder name of a few words beside its checkbox.
const SONG_MENU_WIDTH: f32 = 200.;
