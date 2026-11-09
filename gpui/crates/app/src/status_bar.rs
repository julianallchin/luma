//! The status bar on the window's trim, under the page: what the app is
//! doing in the background, and one line about the mode on screen.
//!
//! Left, the quiet states — sync, DMX output, an import running, an update
//! ready, something just done that did not land — each one [`Item`], drawn
//! one way. Right,
//! the mode's context line: in Arrange the open song, in Live what is
//! followed, in Rig the size of the rig.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::glass;
use luma_ui::icons::IconName;
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};

use crate::shell::Body;
use crate::workspace::Mode;
use crate::Luma;

/// One thing the bar says: a word or two, and what pressing it does.
pub(crate) struct Item {
    pub(crate) id: &'static str,
    pub(crate) icon: Option<IconName>,
    pub(crate) label: String,
    /// Worth a look: the item carries the notice mark ahead of its words.
    /// The words stay in the bar's own ink — nothing here is written in red.
    pub(crate) attention: bool,
    /// A healthy steady state gets the green dot ahead of its word.
    pub(crate) ok: bool,
    pub(crate) press: Option<fn(&mut Luma, &mut Context<Luma>)>,
    /// What hangs off the item while it is open — the sync details.
    pub(crate) detail: Option<AnyElement>,
}

impl Item {
    pub(crate) fn new(id: &'static str, label: impl Into<String>) -> Self {
        Self {
            id,
            icon: None,
            label: label.into(),
            attention: false,
            ok: false,
            press: None,
            detail: None,
        }
    }
}

/// The bar's cached region body.
pub(crate) fn bar(app: &mut Luma, _: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let app = &*app;
    let items = [
        crate::sync_status::item(app, &entity),
        output(app),
        crate::add_tracks::progress::item(app),
        crate::update::item(app),
        crate::failure::item(app),
    ];
    div()
        .size_full()
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(12.))
        // On the trim under the page, like the title bar: no card of its own.
        .bg(glass::window_trim())
        .text_size(px(11.))
        .text_color(glass::ink(glass::TEXT_TERTIARY))
        .children(
            items
                .into_iter()
                .flatten()
                .map(|item| render(item, &entity)),
        )
        .child(div().flex_1())
        .children(app.context_line().map(|line| {
            div()
                .truncate()
                .child(line.clone())
                .agent_node(Role::Text, line)
        }))
        .agent_node(Role::Card, "Status bar")
        .into_any_element()
}

fn render(item: Item, app: &Entity<Luma>) -> AnyElement {
    let label = item.label.clone();
    div()
        .id(item.id)
        .relative()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_color(glass::ink(glass::TEXT_TERTIARY))
        .when(item.ok, |row| {
            row.child(div().size(px(6.)).rounded_full().bg(ladder::status_ok()))
        })
        .when(item.attention, |row| row.child(luma_ui::notice::mark()))
        .children(
            item.icon
                .map(|icon| gpui_component::Icon::new(icon).size(px(11.))),
        )
        .child(label.clone())
        .when_some(item.press, |row, press| {
            let pressed = app.clone();
            row.cursor_pointer()
                .hover(|style| style.text_color(glass::ink(0.95)))
                .on_click(move |_, _, cx| pressed.update(cx, |this, cx| press(this, cx)))
        })
        .children(item.detail)
        .agent_node(
            if item.press.is_some() {
                Role::Button
            } else {
                Role::Text
            },
            label,
        )
        .into_any_element()
}

/// Whether DMX goes out at all — the setting every transport obeys.
fn output(app: &Luma) -> Option<Item> {
    app.songs.as_ref()?;
    let mut item = Item::new(
        "dmx-output",
        if app.dmx_output {
            "DMX output on"
        } else {
            "DMX output off"
        },
    );
    item.ok = app.dmx_output;
    item.press = Some(|this, cx| this.open_settings(cx));
    Some(item)
}

impl Luma {
    /// The mode's one line: in Arrange the open song, in Live what is
    /// followed, in Rig the size of the rig.
    fn context_line(&self) -> Option<String> {
        match (self.workspace.mode(), self.workspace.active_body()) {
            (Mode::Arrange, Some(Body::TrackEditor(editor))) => {
                let track = self.songs.as_ref()?.find(editor.track_id())?;
                let mut line = crate::tracks::track_name(&track);
                if let Some(artist) = track.artist.as_deref().filter(|a| !a.is_empty()) {
                    line.push_str(&format!(" — {artist}"));
                }
                if let Some(bpm) = track.bpm {
                    line.push_str(&format!(" · {bpm:.1} BPM"));
                }
                if let Some(seconds) = track.duration_seconds {
                    let total = seconds.max(0.).round() as u64;
                    line.push_str(&format!(" · {}:{:02}", total / 60, total % 60));
                }
                Some(line)
            }
            (Mode::Live, Some(Body::Perform(_))) => self.live_line(),
            (Mode::Rig, Some(Body::Patch(page))) => page.rig_line(),
            _ => None,
        }
    }
}
