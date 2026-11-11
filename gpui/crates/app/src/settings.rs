//! The settings page: the app's key/value table, typed and editable.
//!
//! # A page, not a dialog
//!
//! Settings takes the window's whole page in one frame, and the title bar
//! keeps only the window's corner and Back. Back or Escape returns to what was
//! there, untouched: nothing under the page is torn down to show it
//! (`docs/specs/comet-shell.md` §6).
//!
//! # Search
//!
//! The nav's search field looks through every section at once. A row matches
//! on its label and its help text; while there is a query the nav's sections
//! step aside and the page column lists what matched, under the section each
//! match lives in. Escape empties the query before it leaves the page.
//!
//! # Shortcuts
//!
//! The Shortcuts section lists [`crate::keymap::SHORTCUTS`] under its groups,
//! each row's keys read back from the keymap. Clicking a row's keys records
//! the next keystroke (Escape cancels) through a keystroke interceptor, so a
//! key some command already holds is heard rather than run. One that another
//! command holds where this one would be live is refused, naming it.
//! Rebinds are this machine's ([`crate::shortcuts`]), not settings rows.
//!
//! # The write path
//!
//! Every control writes through `set_setting` on the seam and then re-reads
//! `get_settings`. Nothing is applied optimistically: the screen shows what
//! the database holds, so a write that silently failed shows as the control
//! springing back rather than as a lie that survives until the next reload.

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::arg::select::luma_arg_select;
use luma_ui::float::RowState;
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::Enabled;
use luma_ui::{glass, ladder, radius};

use luma_lib::settings::{
    agent_models, key, ApiKeyStatus, AppSettings, AGENT_PROVIDERS, MAX_DIMMER_RANGE,
};

use crate::keymap::{self, Shortcut};
use crate::Luma;

/// Which section is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Account,
    Ai,
    Output,
    Shortcuts,
    About,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::General,
        Tab::Account,
        Tab::Ai,
        Tab::Output,
        Tab::Shortcuts,
        Tab::About,
    ];

    fn icon(self) -> IconName {
        match self {
            Tab::General => IconName::Gear,
            Tab::Account => IconName::User,
            Tab::Ai => IconName::Bot,
            Tab::Output => IconName::Network,
            Tab::Shortcuts => IconName::Keyboard,
            Tab::About => IconName::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Account => "Account",
            Tab::Ai => "AI",
            Tab::Output => "DMX Output",
            Tab::Shortcuts => "Shortcuts",
            Tab::About => "About",
        }
    }

    /// The line under the page's title.
    fn description(self) -> &'static str {
        match self {
            Tab::General => "How Luma plays and draws on this machine.",
            Tab::Account => "Who this library belongs to.",
            Tab::Ai => "The agent that works beside you, and the keys it uses.",
            Tab::Output => "How light leaves this machine.",
            Tab::Shortcuts => {
                "The keys that ask for each command. Click a shortcut to record a new one."
            }
            Tab::About => "This copy of Luma, and how it stays current.",
        }
    }
}

/// The screen's whole state: which section, what the last read returned, and
/// which select is open.
pub struct Settings {
    tab: Tab,
    /// `None` until the first `get_settings` lands.
    values: Option<AppSettings>,
    /// The first read failed, so there is nothing to show. The cause is in
    /// the log; a failed write says so in the status bar instead.
    load_failed: bool,
    /// The setting key of the select whose menu is open. One at a time, and
    /// the key is enough of an identity because no two selects on this screen
    /// write the same setting.
    open_menu: Option<&'static str>,
    /// One key field per [`AGENT_PROVIDERS`] service, in that order.
    keys: Vec<KeyField>,
    /// The nav's search field, and what it holds — cached from its edits for
    /// the same reason as [`KeyField::has_draft`].
    search: Entity<TextInput>,
    query: String,
    _search: Subscription,
    /// The shortcut whose next keystroke is being recorded, and the
    /// interceptor hearing it — dropped, it stops listening.
    recording: Option<(&'static str, Subscription)>,
    /// The last refused key, on the row that refused it.
    refused: Option<(&'static str, String)>,
}

impl Settings {
    fn new(cx: &mut Context<Luma>) -> Self {
        let search = cx.new(|cx| {
            let mut input = TextInput::search(SEARCH_PLACEHOLDER, cx);
            input.set_text_size(12.5, cx);
            input
        });
        let events = cx.subscribe(&search, |this, field, event, cx| match event {
            text_input::Event::Edited => {
                let query = field.read(cx).text().to_string();
                this.with_settings(cx, |state| {
                    state.query = query;
                    state.open_menu = None;
                });
            }
            _ => cx.notify(),
        });
        Self {
            tab: Tab::General,
            values: None,
            load_failed: false,
            open_menu: None,
            keys: AGENT_PROVIDERS
                .iter()
                .map(|(provider, label)| KeyField::new(provider, label, cx))
                .collect(),
            search,
            query: String::new(),
            _search: events,
            recording: None,
            refused: None,
        }
    }
}

/// One service's API key field. It holds only what is being typed: the
/// stored key never comes back across the seam, so there is nothing here to
/// leak, and what the field shows about the stored one is its hint.
struct KeyField {
    /// The [`AGENT_PROVIDERS`] id, matched against [`ApiKeyStatus::provider`].
    provider: &'static str,
    label: &'static str,
    input: Entity<TextInput>,
    /// Whether the draft holds anything. Cached from the field's own edits
    /// because the screen renders without a context to read the field from.
    has_draft: bool,
    _events: Subscription,
}

impl KeyField {
    fn new(provider: &'static str, label: &'static str, cx: &mut Context<Luma>) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::secret("Paste a key", cx);
            input.set_text_size(12., cx);
            input
        });
        let events = cx.subscribe(&input, move |this, field, event, cx| match event {
            text_input::Event::Edited => {
                let has_draft = !field.read(cx).is_empty();
                this.with_settings(cx, |state| {
                    if let Some(key) = state.key_field_mut(provider) {
                        key.has_draft = has_draft;
                    }
                });
            }
            text_input::Event::Submitted => this.save_api_key(provider, cx),
            text_input::Event::Cancelled => {
                field.update(cx, |input, cx| input.set_text("", cx));
            }
            _ => cx.notify(),
        });
        Self {
            provider,
            label,
            input,
            has_draft: false,
            _events: events,
        }
    }
}

impl Settings {
    fn key_field_mut(&mut self, provider: &str) -> Option<&mut KeyField> {
        self.keys.iter_mut().find(|key| key.provider == provider)
    }
}

/// The account, as the app is currently able to speak about it.
///
/// Who is admitted is not kept here — [`crate::library::Library::account`] is
/// the cache of that and is written through at every transition, so a second
/// copy would only be able to name the previous account. What lives here is
/// the part of the gesture that is not durable: whether the one multi-command
/// sign-out is mid-flight. A sign-out that fails says so in the status bar.
/// The brightness slider's writes — see [`Luma::set_max_dimmer`].
#[derive(Default)]
pub(crate) struct DimmerWrite {
    /// The level the slider last asked for, until a write of it has landed.
    /// A read of the settings in the meantime shows this, not the database's
    /// older answer.
    wanted: Option<u8>,
    writing: bool,
}

#[derive(Default)]
pub(crate) struct AccountAction {
    /// A sign-out is in flight. It is a multi-command durability boundary —
    /// flush, wipe, release — and pressing it twice would race its own journal.
    pub(crate) signing_out: bool,
}

// -- navigation and writes ----------------------------------------------------
//
// These hang off `Luma` rather than off `Settings` because a settings write is
// a `Library` call and a screen transition, and `Luma` is what owns both. The
// impl lives here so the router stays a router.

impl Luma {
    /// Put the settings page up in place of the page's content. The regions
    /// it covers keep their state, so closing it shows them as they were. The
    /// venue menu and the dialogs that open it have already closed by now.
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        self.settings = Some(Settings::new(cx));
        cx.notify();
        self.reload_settings(cx);
    }

    /// The settings page on DMX Output: where output is turned on, from the
    /// venue page that needs it on.
    pub(crate) fn open_output_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings(cx);
        if let Some(state) = &mut self.settings {
            state.tab = Tab::Output;
        }
        cx.notify();
    }

    /// The menu's "About Luma": the page, on its About section.
    pub(crate) fn open_about(&mut self, cx: &mut Context<Self>) {
        self.open_settings(cx);
        if let Some(state) = &mut self.settings {
            state.tab = Tab::About;
        }
        cx.notify();
    }

    /// Take the page down: Back or Escape. Reports whether it was up —
    /// Escape's rung.
    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) -> bool {
        if self.settings.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    /// Escape on the page: an open select first, then a search, then the
    /// page itself.
    pub(crate) fn dismiss_settings(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = &mut self.settings else {
            return false;
        };
        if state.open_menu.take().is_some() {
            cx.notify();
            return true;
        }
        if !state.query.is_empty() {
            self.clear_search(cx);
            return true;
        }
        self.close_settings(cx)
    }

    /// Empty the search, which puts the nav's sections and the shown page
    /// back.
    fn clear_search(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.settings else {
            return;
        };
        state.query.clear();
        let field = state.search.clone();
        field.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    fn show_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.with_settings(cx, |state| {
            state.tab = tab;
            state.open_menu = None;
        });
    }

    fn toggle_menu(&mut self, key: &'static str, cx: &mut Context<Self>) {
        self.with_settings(cx, |state| {
            state.open_menu = if state.open_menu == Some(key) {
                None
            } else {
                Some(key)
            };
        });
    }

    /// Sign out, and land at the sign-in screen.
    ///
    /// Hangs off the app rather than the settings screen because both doors to
    /// it — the venue menu's account row and the settings row — press the same
    /// three commands, and a guard living on one screen could not stop the
    /// other from pressing them again mid-flight.
    pub(crate) fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.account_action.signing_out {
            return;
        }
        self.account_action = AccountAction { signing_out: true };
        let pending = self.library.sign_out();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.account_action = AccountAction { signing_out: false };
                // Signed out is the interval before the next sign-in, not a
                // state the shell shows: the library is nobody's now, so the
                // gate is the next screen.
                match result {
                    Ok(_) => this.show_sign_in(false, cx),
                    // Pressed from a menu that has since closed: a report
                    // that lived only in Settings would be a gesture that
                    // silently did nothing.
                    Err(error) => this.report_failure("Could not sign out", error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Set the brightness cap from its slider. The level goes on screen at
    /// once and the write follows it: a fill that waited for the write and a
    /// read-back would trail the pointer.
    ///
    /// One write at a time, and the last level asked for is the last one
    /// written — a drag is a burst of moves, and writes sent side by side
    /// land in any order, so the database could end on a level from halfway
    /// along. The writes belong to the app, not to the page: leaving
    /// Settings mid-drag still writes where the pointer stopped.
    fn set_max_dimmer(&mut self, level: u8, cx: &mut Context<Self>) {
        self.with_settings(cx, |state| {
            if let Some(values) = state.values.as_mut() {
                values.max_dimmer = level;
            }
        });
        self.dimmer_write.wanted = Some(level);
        if !self.dimmer_write.writing {
            self.write_max_dimmer(level, cx);
        }
    }

    fn write_max_dimmer(&mut self, level: u8, cx: &mut Context<Self>) {
        self.dimmer_write.writing = true;
        let pending = self
            .library
            .set_setting(key::MAX_DIMMER, &level.to_string());
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.dimmer_write.writing = false;
                match result {
                    Ok(()) => match this.dimmer_write.wanted {
                        Some(next) if next != level => this.write_max_dimmer(next, cx),
                        _ => this.dimmer_write.wanted = None,
                    },
                    Err(error) => {
                        this.dimmer_write.wanted = None;
                        this.report_failure("The setting was not saved", error, cx);
                        this.reload_settings(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Write one setting, then read every setting back.
    fn write_setting(&mut self, key: &str, value: String, cx: &mut Context<Self>) {
        self.with_settings(cx, |state| state.open_menu = None);
        let pending = self.library.set_setting(key, &value);
        cx.spawn(async move |this, cx| match pending.await {
            Ok(()) => {
                this.update(cx, |this, cx| this.reload_settings(cx)).ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.report_failure("The setting was not saved", error, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Store the typed key for `provider`, or nothing if the draft is blank.
    /// The draft is emptied only once the write has landed, so a failed save
    /// leaves the key where it was typed.
    fn save_api_key(&mut self, provider: &'static str, cx: &mut Context<Self>) {
        let Some((setting, input, key)) = self.key_draft(provider, cx) else {
            return;
        };
        if key.is_empty() {
            return;
        }
        self.write_api_key(setting, key, Some(input), cx);
    }

    /// Remove `provider`'s stored key. An empty row is how the agent's key
    /// lookup already reads "no key" ([`luma_lib::agent::model::api_key`]),
    /// so clearing is a write like any other.
    fn clear_api_key(&mut self, provider: &'static str, cx: &mut Context<Self>) {
        let Some((setting, _, _)) = self.key_draft(provider, cx) else {
            return;
        };
        self.write_api_key(setting, String::new(), None, cx);
    }

    /// The settings row `provider`'s key lives in, its field, and the trimmed
    /// draft in that field.
    fn key_draft(&self, provider: &str, cx: &App) -> Option<(String, Entity<TextInput>, String)> {
        let state = self.settings.as_ref()?;
        let setting = state
            .values
            .as_ref()?
            .agent_api_keys
            .iter()
            .find(|status| status.provider == provider)?
            .setting
            .clone();
        let input = state
            .keys
            .iter()
            .find(|key| key.provider == provider)?
            .input
            .clone();
        let draft = input.read(cx).text().trim().to_string();
        Some((setting, input, draft))
    }

    /// [`Self::write_setting`] for a key: the same ordered write and re-read,
    /// plus emptying the field that held the draft once it is stored. Never
    /// the value in an error — `set_setting`'s errors name the key only.
    fn write_api_key(
        &mut self,
        setting: String,
        value: String,
        drafted_in: Option<Entity<TextInput>>,
        cx: &mut Context<Self>,
    ) {
        let pending = self.library.set_setting(&setting, &value);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    if let Some(input) = drafted_in {
                        input.update(cx, |input, cx| input.set_text("", cx));
                    }
                    this.reload_settings(cx);
                }
                Err(error) => this.report_failure("The key was not saved", error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Put each stored key's hint in its field's placeholder. Here rather than
    /// in the render because a field's placeholder is the field's own state.
    fn show_key_hints(&self, values: &AppSettings, cx: &mut Context<Self>) {
        let Some(state) = &self.settings else {
            return;
        };
        let updates: Vec<_> = state
            .keys
            .iter()
            .filter_map(|key| {
                let status = values
                    .agent_api_keys
                    .iter()
                    .find(|status| status.provider == key.provider)?;
                Some((key.input.clone(), key_placeholder(status)))
            })
            .collect();
        for (input, placeholder) in updates {
            input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
    }

    fn reload_settings(&self, cx: &mut Context<Self>) {
        let pending = self.library.settings();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Ok(values) = &result {
                    this.apply_display_settings(values, cx);
                    this.show_key_hints(values, cx);
                }
                let wanted = this.dimmer_write.wanted;
                this.with_settings(cx, |state| match result {
                    Ok(mut values) => {
                        if let Some(level) = wanted {
                            values.max_dimmer = level;
                        }
                        state.values = Some(values);
                        state.load_failed = false;
                    }
                    Err(error) => {
                        log::warn!("could not read the settings: {error}");
                        state.load_failed = state.values.is_none();
                    }
                })
            })
            .ok();
        })
        .detach();
    }

    /// Read the settings the window itself applies, once, at launch. A failed
    /// read keeps the defaults: the window has to open either way.
    pub(crate) fn load_display_settings(&self, cx: &mut Context<Self>) {
        let pending = self.library.settings();
        cx.spawn(async move |this, cx| {
            if let Ok(values) = pending.await {
                this.update(cx, |this, cx| this.apply_display_settings(&values, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn apply_display_settings(&mut self, values: &AppSettings, cx: &mut Context<Self>) {
        luma_chat::set_compact_turns(values.compact_turns, cx);
        if self.hdr_output_allowed != values.hdr_output {
            self.hdr_output_allowed = values.hdr_output;
            cx.notify();
        }
        // The stage open now takes it at once; one opened later reads it
        // with the rest of its settings.
        if self
            .visualizer
            .as_mut()
            .is_some_and(|stage| stage.show_frame_rate(values.stage_frame_rate))
        {
            self.notify_stage(cx);
        }
        if self.dmx_output != values.dmx_output_enabled {
            self.dmx_output = values.dmx_output_enabled;
            cx.notify();
        }
        self.set_auto_update(values.auto_update);
        self.set_update_channel(values.update_channel.as_deref());
    }

    /// Listen for `row`'s next keystroke. Every key goes to the recorder
    /// while it listens — ⌘P records as ⌘P rather than opening the song
    /// list — and nothing does once it stops.
    fn record_shortcut(&mut self, row: &'static Shortcut, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let heard = cx.intercept_keystrokes(move |event, _, cx| {
            let stroke = event.keystroke.clone();
            if this
                .update(cx, |this, cx| this.recorded_key(stroke, cx))
                .unwrap_or(false)
            {
                cx.stop_propagation();
            }
        });
        self.with_settings(cx, |state| {
            state.recording = Some((row.id, heard));
            state.refused = None;
        });
    }

    /// One keystroke while recording: Escape stops, a key no other command
    /// holds becomes the row's, one another holds is refused by name.
    /// `false` when nothing is recording, so the key goes on as usual.
    fn recorded_key(&mut self, mut stroke: Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(row) = self
            .settings
            .as_ref()
            .and_then(|state| state.recording.as_ref())
            .and_then(|(id, _)| Shortcut::by_id(id))
        else {
            return false;
        };
        if MODIFIER_KEYS.contains(&stroke.key.as_str()) {
            return true;
        }
        let cancel = stroke.key == "escape" && !stroke.modifiers.modified();
        let stopped = |state: &mut Settings| state.recording = None;
        if cancel {
            self.with_settings(cx, stopped);
            return true;
        }
        stroke.key_char = None;
        stroke.modifiers.function = false;
        let key = stroke.unparse();
        match keymap::holder(row, &key, cx) {
            Some(holder) => {
                let shown = luma_ui::shortcut::keystroke_text(&stroke.modifiers, &stroke.key);
                self.with_settings(cx, |state| {
                    state.recording = None;
                    state.refused = Some((row.id, format!("{shown} is already {holder}")));
                });
            }
            None => {
                self.with_settings(cx, stopped);
                self.rebind_shortcut(row, Some(vec![key]), cx);
            }
        }
        true
    }

    fn reset_shortcut(&mut self, row: &'static Shortcut, cx: &mut Context<Self>) {
        self.with_settings(cx, |state| state.refused = None);
        self.rebind_shortcut(row, None, cx);
    }

    fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.with_settings(cx, |state| {
            state.refused = None;
            state.recording = None;
        });
        self.reset_shortcuts(cx);
    }

    /// Run `edit` against the settings page's state, if it is still up. A
    /// load that lands after it was closed is a no-op, not a page that snaps
    /// back.
    fn with_settings(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Settings)) {
        if let Some(state) = &mut self.settings {
            edit(state);
            cx.notify();
        }
    }
}

// -- rendering ----------------------------------------------------------------
//
// Zeron's settings page: a nav down the left, and a page column beside it — a
// title over a description, then sections, each a small title over a card of
// rows. A row is the setting's name and help on the left and its control at
// the end.

/// The nav's width, divided from the page by an inner hairline: zeron's 200.
const NAV_WIDTH: f32 = 200.;
/// The page column: zeron's settings measure.
const PAGE_WIDTH: f32 = 640.;
/// A row's floor; a row with help text grows past it.
const ROW_HEIGHT: f32 = 52.;
/// The control slot at a row's end. A wider control (a key field and its
/// buttons) grows it; nothing narrows it.
const CONTROL_WIDTH: f32 = 240.;
/// Air between the page's sections.
const SECTION_GAP: f32 = 24.;
/// Where the words of a card's rows start — the row's 16 and the card's 1px
/// edge. The page's and the sections' titles start there too, so every word
/// on the page lines up down one edge.
const TEXT_INSET: f32 = 17.;
/// A card's fill: zeron's `#ffffff08`.
const CARD_ALPHA: f32 = 0x08 as f32 / 255.0;

/// Render the page. `app` is the root entity every control writes through;
/// `shell` is the app state the account section reads its identity from.
pub fn settings(shell: &Luma, state: &Settings, app: &Entity<Luma>, cx: &App) -> Div {
    div()
        .size_full()
        .flex()
        .flex_row()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(nav(state, app))
        .child(
            div()
                .id("settings-page")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .items_center()
                .child(match &state.values {
                    None if state.load_failed => {
                        luma_ui::plate("Could not load the settings", ladder::muted_foreground())
                    }
                    None => luma_ui::plate("Loading…", ladder::muted_foreground()),
                    Some(values) => page(shell, state, values, app, cx).into_any_element(),
                }),
        )
}

/// The way back, in the title bar where the venue menu sits while settings
/// is not up: the bar carries nothing else then.
pub(crate) fn back(app: &Entity<Luma>) -> AnyElement {
    let back = app.clone();
    luma_ui::float::nav_row(RowState::Rest, "settings-back")
        .id("settings-back")
        .flex_none()
        .px(px(10.))
        .text_size(px(13.))
        .text_color(glass::ink(glass::TEXT_SECONDARY))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            back.update(cx, |this, cx| {
                this.close_settings(cx);
            })
        })
        .child(gpui_component::Icon::new(luma_ui::icons::IconName::ChevronLeft).size(px(12.)))
        .child("Back")
        .agent_node(Role::Button, "Back")
        .into_any_element()
}

/// What the search field says while it is empty.
const SEARCH_PLACEHOLDER: &str = "Search settings";

/// The search field, then the sections, one row each — or, while there is a
/// query, the field alone: its results stand in the page column. The way back
/// is the title bar's ([`back`]).
fn nav(state: &Settings, app: &Entity<Luma>) -> Div {
    let searching = Query::parse(&state.query).is_some();
    div()
        .w(px(NAV_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(8.))
        .border_r_1()
        .border_color(glass::inner_rule())
        .child(
            div()
                .px(px(8.))
                .pt(px(4.))
                .pb(px(4.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(glass::ink(glass::TEXT_TERTIARY))
                .child("Settings")
                .agent_node(Role::Text, "Settings"),
        )
        .child(search_field(state))
        .children(Tab::ALL.into_iter().filter(|_| !searching).map(|tab| {
            let app = app.clone();
            luma_ui::float::nav_row(RowState::of(tab == state.tab, false), tab.label())
                .id(tab.label())
                .tab_index(0)
                .on_click(move |_, _, cx| app.update(cx, |this, cx| this.show_tab(tab, cx)))
                .child(gpui_component::Icon::new(tab.icon()).size(px(16.)))
                .child(tab.label())
                .agent_node(Role::Toggle, tab.label())
        }))
}

/// The nav's search: a [`luma_ui::float::field`] around the text field.
/// Escape is the page's ([`Luma::dismiss_settings`]), so it needs no key
/// handling of its own.
fn search_field(state: &Settings) -> impl IntoElement {
    let label = if state.query.is_empty() {
        SEARCH_PLACEHOLDER
    } else {
        &state.query
    };
    luma_ui::float::field()
        .h(px(28.))
        .mb(px(6.))
        .gap(px(6.))
        .child(
            gpui_component::Icon::new(IconName::Search)
                .size(px(14.))
                .text_color(glass::ink(glass::TEXT_TERTIARY)),
        )
        .child(div().flex_1().min_w_0().child(state.search.clone()))
        // The label is the field's value once it has one, as the song list's
        // search does: a driver asks what it says.
        .agent_node(Role::Input, label.to_string())
}

/// A search's terms, lowercased. A row matches when its words hold every
/// one, so word order and case do not matter.
struct Query(Vec<String>);

impl Query {
    /// `None` for a query with no terms: a blank search shows the page.
    fn parse(text: &str) -> Option<Self> {
        let terms: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        (!terms.is_empty()).then_some(Self(terms))
    }

    fn matches(&self, row: &Row) -> bool {
        self.0.iter().all(|term| row.words.contains(term.as_str()))
    }
}

/// The page column: the shown section's title,
/// description and groups — or, while there is a search, every section's
/// matching rows under their section's title.
fn page(shell: &Luma, state: &Settings, values: &AppSettings, app: &Entity<Luma>, cx: &App) -> Div {
    let column = div()
        .w_full()
        .max_w(px(PAGE_WIDTH))
        .flex()
        .flex_col()
        .gap(px(SECTION_GAP))
        .px(px(24.))
        .pt(px(40.))
        .pb(px(48.));
    let sections = |tab| match tab {
        Tab::General => general(values, app),
        Tab::Account => account(shell, app),
        Tab::Ai => ai(state, values, app),
        Tab::Output => output(values, app),
        Tab::Shortcuts => shortcuts(shell, state, app, cx),
        Tab::About => about(shell, values, app),
    };
    let Some(query) = Query::parse(&state.query) else {
        return column
            .child(heading(state.tab))
            .children(sections(state.tab).into_iter().map(Section::render));
    };
    let found: Vec<Div> = Tab::ALL
        .into_iter()
        .filter_map(|tab| {
            let matched: Vec<Section> = sections(tab)
                .into_iter()
                .filter_map(|section| section.matching(&query))
                .collect();
            (!matched.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SECTION_GAP))
                    .child(heading(tab))
                    .children(matched.into_iter().map(Section::render))
            })
        })
        .collect();
    if found.is_empty() {
        return column.child(nothing_matches(&state.query, app));
    }
    column.children(found)
}

/// A section's title over its description, at the head of the page.
fn heading(tab: Tab) -> Div {
    div()
        .px(px(TEXT_INSET))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(tab.label())
                .agent_node(Role::Text, tab.label()),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .child(tab.description()),
        )
}

/// A search that found nothing: what was looked for, and the way back to
/// the page.
fn nothing_matches(query: &str, app: &Entity<Luma>) -> Div {
    let message = format!("No settings match \u{201c}{}\u{201d}", query.trim());
    let clear = app.clone();
    div()
        .px(px(TEXT_INSET))
        .flex()
        .flex_col()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .text_size(px(13.))
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .child(message.clone())
                .agent_node(Role::Text, message),
        )
        .child(
            luma_ui::button("Clear search", Enabled::Yes)
                .id("settings-clear-search")
                .on_click(move |_, _, cx| clear.update(cx, |this, cx| this.clear_search(cx)))
                .agent_node(Role::Button, "Clear search"),
        )
}

fn general(values: &AppSettings, app: &Entity<Luma>) -> Vec<Section> {
    vec![
        section(
            "Audio",
            vec![checkbox(
                app,
                key::AUDIO_OUTPUT_ENABLED,
                "Enable Audio Output",
                values.audio_output_enabled,
                Some("When disabled, playback stays in sync but stays silent."),
            )],
            None,
        ),
        section(
            "Display",
            vec![
                checkbox(
                    app,
                    key::HDR_OUTPUT,
                    "HDR output",
                    values.hdr_output,
                    Some("Used only when the display supports HDR. Bright stage light can go above the brightness of white in other windows."),
                ),
                checkbox(
                    app,
                    key::STAGE_FRAME_RATE,
                    "Show frame rate on the stage",
                    values.stage_frame_rate,
                    Some("The frames per second in the stage's corner. Click it for the frame stats."),
                ),
            ],
            None,
        ),
        section(
            "Chat",
            vec![checkbox(
                app,
                key::COMPACT_TURNS,
                "Compact turns",
                values.compact_turns,
                Some("Fold a finished turn's thinking, tool calls and narration behind one \"Worked for\" header. The final reply stays visible."),
            )],
            None,
        ),
    ]
}

fn ai(state: &Settings, values: &AppSettings, app: &Entity<Luma>) -> Vec<Section> {
    use luma_lib::agent::engine::Engine;
    let engine = row(
        "Default agent engine",
        Some("Used for new conversations. Choose a service and model for an existing conversation beside its message box."),
        select(
            state,
            app,
            key::AGENT_ENGINE,
            Engine::CHOICES,
            values.agent_engine.key(),
        ),
    );
    if values.agent_engine != Engine::Api {
        return vec![section(
            "Defaults",
            vec![engine],
            Some("Uses the locally installed agent and its signed-in account. Sign in with the agent's CLI on each machine."),
        )];
    }
    let defaults = section(
        "Defaults",
        vec![
            engine,
            row(
                "Default API service",
                Some("Used for new API conversations. Each service keeps its own API key."),
                select(
                    state,
                    app,
                    key::AGENT_PROVIDER,
                    AGENT_PROVIDERS,
                    &values.agent_provider,
                ),
            ),
            row(
                "Default API model",
                Some("Used for new API conversations."),
                select(
                    state,
                    app,
                    key::AGENT_MODEL,
                    &agent_models(),
                    &values.agent_model,
                ),
            ),
        ],
        None,
    );
    let keys = state
        .keys
        .iter()
        .filter_map(|key| {
            let status = values
                .agent_api_keys
                .iter()
                .find(|status| status.provider == key.provider)?;
            Some(api_key(key, status, app))
        })
        .collect();
    vec![
        defaults,
        section(
            "API keys",
            keys,
            Some("Keys are kept on this device only. They are not synced to your account."),
        ),
    ]
}

/// One service's key: the masked draft field, Save, and Clear while a key is
/// stored. The field's placeholder is the stored key's hint (see
/// [`key_placeholder`]), so the row reads "saved, ending in …" at rest and a
/// typed draft replaces it.
fn api_key(key: &KeyField, status: &ApiKeyStatus, app: &Entity<Luma>) -> Row {
    let provider = key.provider;
    let label = format!("{} API key", key.label);
    let save = app.clone();
    let clear = app.clone();
    let controls = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            luma_ui::float::field()
                .w(px(CONTROL_WIDTH))
                .when(key.has_draft, |field| {
                    field.key_context(text_input::DRAFT_CONTEXT)
                })
                .child(div().flex_1().min_w_0().child(key.input.clone()))
                // The automation label names the field, never what is in it.
                .agent_node(Role::Input, label.clone()),
        )
        .child(
            luma_ui::button("Save", Enabled::from(key.has_draft))
                .id(SharedString::from(format!("save-key-{provider}")))
                .on_click(move |_, _, cx| {
                    save.update(cx, |this, cx| this.save_api_key(provider, cx));
                })
                .agent_node(Role::Button, format!("Save {label}")),
        )
        .when(status.hint.is_some(), |row| {
            row.child(
                luma_ui::button("Clear", Enabled::Yes)
                    .id(SharedString::from(format!("clear-key-{provider}")))
                    .on_click(move |_, _, cx| {
                        clear.update(cx, |this, cx| this.clear_api_key(provider, cx));
                    })
                    .agent_node(Role::Button, format!("Clear {label}")),
            )
        });
    let help = status
        .overridden_by
        .as_ref()
        .map(|env| format!("{env} is set in the environment and is used instead of this key."));
    row(&label, help.as_deref(), controls.into_any_element())
}

/// What an empty key field says: the stored key's hint, or the invitation.
fn key_placeholder(status: &ApiKeyStatus) -> String {
    match &status.hint {
        Some(hint) => format!("Saved key {hint}"),
        None => "Paste a key".to_string(),
    }
}

fn output(values: &AppSettings, app: &Entity<Luma>) -> Vec<Section> {
    let (dimmer_low, dimmer_high) = (
        f32::from(*MAX_DIMMER_RANGE.start()),
        f32::from(*MAX_DIMMER_RANGE.end()),
    );
    vec![
        section(
            "Output",
            vec![
                row(
                    "Max Brightness",
                    Some("Limits overall brightness of DMX output (100 = no limit)."),
                    // Whole percent: the setting is stored as a `u8` and read
                    // back as one, so a slider handing over 61.4 would show 61
                    // on the next reload and disagree with itself in between.
                    luma_ui::float::scrub(
                        key::MAX_DIMMER,
                        f64::from(values.max_dimmer),
                        f64::from(dimmer_low),
                        f64::from(dimmer_high),
                        1.,
                        CONTROL_WIDTH,
                        {
                            let app = app.clone();
                            let current = values.max_dimmer;
                            move |value, _, cx| {
                                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                                let level = value as u8;
                                if level != current {
                                    app.update(cx, |this, cx| this.set_max_dimmer(level, cx));
                                }
                            }
                        },
                    )
                    .agent_node(
                        Role::Slider,
                        format!("Max Brightness {}", values.max_dimmer),
                    )
                    .into_any_element(),
                ),
                checkbox(
                    app,
                    key::DMX_OUTPUT_ENABLED,
                    "Enable DMX Output",
                    values.dmx_output_enabled,
                    Some("Sends to Art-Net nodes and USB DMX dongles. Choose where each universe goes under Outputs on a venue's patch page."),
                ),
            ],
            None,
        ),
        section(
            "Art-Net",
            vec![row(
                "Art-Net Interface IP (Bind Address)",
                Some("0.0.0.0 binds to all interfaces."),
                readonly_value(&values.artnet_interface, "0.0.0.0"),
            )],
            Some("The Art-Net interface is not editable from the native host yet."),
        ),
    ]
}

/// Who is signed in, and the way out.
fn account(shell: &Luma, app: &Entity<Luma>) -> Vec<Section> {
    // The address where the session carries one — see `Account::label`. The
    // page is part of the shell, which is only drawn for a signed-in account.
    let identity = shell
        .library
        .account()
        .map(|account| account.label().to_string())
        .unwrap_or_default();
    let signing_out = shell.account_action.signing_out;
    let label = if signing_out {
        "Signing out…"
    } else {
        "Sign out"
    };
    let act = app.clone();
    let rows = vec![
        row("Signed in as", None, readonly_value(&identity, "")),
        row(
            "Sign out",
            Some(
                "Signing out syncs this library to the cloud, then removes it from this computer.",
            ),
            luma_ui::button(label, Enabled::from(!signing_out))
                .id("settings-sign-out")
                .on_click(move |_, _, cx| {
                    act.update(cx, |this, cx| this.sign_out(cx));
                })
                .agent_node(Role::Button, label)
                .into_any_element(),
        ),
    ];
    vec![section("Account", rows, None)]
}

fn about(shell: &Luma, values: &AppSettings, app: &Entity<Luma>) -> Vec<Section> {
    // The running build's own channel, which a switch does not change until
    // the build it installs is the one running.
    let build = shell.update.build();
    let (button, status) = crate::update::about(shell, app);
    let version = format!("Luma {} · {}", build.version, build.channel.name());
    let words = (version.clone(), status.clone());
    let version_row = card_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(luma_ui::caption(version))
                // Named, so a script reads the answer to a check.
                .when_some(status, |el, status| {
                    el.child(help_text(&status).agent_node(Role::Text, status))
                }),
        )
        .children(button.map(control_slot));
    let version_row = Row::new(version_row, &words.0, words.1.as_deref());
    vec![
        section("Version", vec![version_row], None),
        section(
            "Updates",
            vec![
                update_channel(shell, values, app),
                checkbox(
                    app,
                    key::AUTO_UPDATE,
                    "Update automatically",
                    values.auto_update,
                    Some("Checks for a new release every hour and downloads it in the background. When it is ready, Luma asks whether to relaunch now; otherwise it runs the next time you open Luma."),
                ),
            ],
            None,
        ),
    ]
}

/// Which release channel this install follows: Stable or Nightly, the
/// build's own until someone chooses. Choosing checks that channel at once
/// ([`Luma::set_update_channel`]); the help says what the choice means.
fn update_channel(shell: &Luma, values: &AppSettings, app: &Entity<Luma>) -> Row {
    let followed = shell.update.followed(values.update_channel.as_deref());
    let help = match followed {
        luma_update::Channel::Nightly => "A build of the newest work, most days, less tested than a release. Choosing Stable installs the latest release, even one older than this.",
        _ => "Tested releases. Nightly is a build of the newest work, most days.",
    };
    let track =
        luma_ui::float::segmented().children(luma_update::Channel::CHOICES.map(|channel| {
            let app = app.clone();
            luma_ui::float::segment(channel.name(), channel == followed, channel.name())
                .id(SharedString::from(format!(
                    "update-channel-{}",
                    channel.label()
                )))
                .on_click(move |_, _, cx| {
                    if channel != followed {
                        app.update(cx, |this, cx| {
                            this.write_setting(
                                key::UPDATE_CHANNEL,
                                channel.label().to_string(),
                                cx,
                            );
                        });
                    }
                })
                .agent_node(Role::Toggle, channel.name())
        }));
    row("Update channel", Some(help), track.into_any_element())
}

/// Keys a recorder hears go down without being a shortcut of their own.
const MODIFIER_KEYS: [&str; 6] = [
    "shift", "control", "alt", "platform", "function", "capslock",
];

/// Settings → Shortcuts: the way back to every default, then the table's
/// groups, each row its title and keys.
fn shortcuts(shell: &Luma, state: &Settings, app: &Entity<Luma>, cx: &App) -> Vec<Section> {
    let reset = app.clone();
    let any = !shell.shortcuts.value().is_empty();
    let button = luma_ui::button("Reset all", if any { Enabled::Yes } else { Enabled::No })
        .id("shortcuts-reset-all")
        .on_click(move |_, _, cx| reset.update(cx, |this, cx| this.reset_all_shortcuts(cx)))
        .agent_node(Role::Button, "Reset all shortcuts");
    let mut sections = vec![section(
        "Defaults",
        vec![row(
            "Reset all shortcuts",
            Some("Put every shortcut back to the key it came with."),
            button.into_any_element(),
        )],
        None,
    )];
    for row in keymap::SHORTCUTS {
        let entry = shortcut_row(shell, state, row, app, cx);
        match sections.last_mut() {
            Some(section) if section.title == row.group => section.rows.push(entry),
            _ => sections.push(section(row.group, vec![entry], None)),
        }
    }
    sections
}

/// One shortcut: its title, and at the end its keys — the recorder — with
/// Reset beside them once it is rebound.
fn shortcut_row(
    shell: &Luma,
    state: &Settings,
    shortcut: &'static Shortcut,
    app: &Entity<Luma>,
    cx: &App,
) -> Row {
    let action = shortcut.action();
    let mut keys: Vec<String> = Vec::new();
    for binding in luma_ui::shortcut::bindings(action.as_ref(), cx) {
        let text = luma_ui::shortcut::binding_text(&binding);
        if keymap::from_table(&binding) && !keys.contains(&text) {
            keys.push(text);
        }
    }
    let name = format!("{}: {}", shortcut.group, shortcut.title);
    let recording = state
        .recording
        .as_ref()
        .is_some_and(|(id, _)| *id == shortcut.id);
    let refused = state
        .refused
        .as_ref()
        .filter(|(id, _)| *id == shortcut.id)
        .map(|(_, message)| message.clone());
    let customized = shell.shortcuts.value().contains_key(shortcut.id);

    let record = app.clone();
    let caps: Vec<AnyElement> = if recording {
        vec![luma_ui::float::key_cap()
            .child("Press a key…")
            .agent_node(Role::Text, "Press a key…")
            .into_any_element()]
    } else if keys.is_empty() {
        vec![luma_ui::float::key_cap().child("None").into_any_element()]
    } else {
        keys.iter()
            .cloned()
            .map(luma_ui::shortcut::cap_of)
            .collect()
    };
    let recorder = div()
        .id(SharedString::from(format!("shortcut-{}", shortcut.id)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .p(px(3.))
        .rounded(px(radius::CONTROL))
        .cursor_pointer()
        .when(recording, |el| el.bg(glass::wash(glass::WASH_SUBTLE)))
        .hover(|style| style.bg(glass::wash(glass::WASH_SUBTLE)))
        .on_click(move |_, _, cx| record.update(cx, |this, cx| this.record_shortcut(shortcut, cx)))
        .children(caps)
        .agent_node(Role::Button, name.clone());
    let reset = customized.then(|| {
        let reset = app.clone();
        luma_ui::button("Reset", Enabled::Yes)
            .id(SharedString::from(format!(
                "shortcut-reset-{}",
                shortcut.id
            )))
            .on_click(move |_, _, cx| {
                reset.update(cx, |this, cx| this.reset_shortcut(shortcut, cx))
            })
            .agent_node(Role::Button, format!("Reset {name}"))
    });
    let control = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .children(reset)
        .child(recorder)
        .into_any_element();
    let element = card_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(shortcut.title),
                )
                // The shortcut just typed is someone else's: validation,
                // said on the row being typed into.
                .when_some(refused, |el, message| {
                    el.child(luma_ui::notice::line(message))
                }),
        )
        .child(control_slot(control));
    Row {
        words: format!("{}\n{}\n{}", shortcut.title, shortcut.group, keys.join(" ")).to_lowercase(),
        element,
    }
}

// -- the pieces every section is built from -----------------------------------

/// A section: its small title over a card of rows, and a footnote under the
/// card when it has one. Kept as rows until it is drawn, so a search can
/// keep the ones it found.
struct Section {
    title: &'static str,
    rows: Vec<Row>,
    note: Option<&'static str>,
}

/// One row of a section, and the words a search finds it by: its label and
/// its help, lowercased.
struct Row {
    words: String,
    element: Div,
}

impl Row {
    fn new(element: Div, label: &str, help: Option<&str>) -> Self {
        let words = match help {
            Some(help) => format!("{label}\n{help}"),
            None => label.to_string(),
        };
        Self {
            words: words.to_lowercase(),
            element,
        }
    }
}

fn section(title: &'static str, rows: Vec<Row>, note: Option<&'static str>) -> Section {
    Section { title, rows, note }
}

impl Section {
    /// The rows `query` finds, or `None` if it finds none.
    fn matching(self, query: &Query) -> Option<Self> {
        let rows: Vec<Row> = self
            .rows
            .into_iter()
            .filter(|row| query.matches(row))
            .collect();
        (!rows.is_empty()).then_some(Self { rows, ..self })
    }

    /// The title over the card, hairlines between its rows.
    fn render(self) -> Div {
        let count = self.rows.len();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .px(px(TEXT_INSET))
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(glass::ink(glass::TEXT_SECONDARY))
                    .child(self.title),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(radius::CARD))
                    .bg(glass::ink(CARD_ALPHA))
                    .border_1()
                    .border_color(glass::inner_rule())
                    .overflow_hidden()
                    .children(self.rows.into_iter().enumerate().map(|(index, row)| {
                        row.element.when(index + 1 < count, |row| {
                            row.border_b_1().border_color(glass::inner_rule())
                        })
                    })),
            )
            .when_some(self.note, |el, note| {
                el.child(div().px(px(TEXT_INSET)).child(help_text(note)))
            })
    }
}

/// A row's frame: 52 high at least, square, its content on one line.
fn card_row() -> Div {
    div()
        .min_h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(16.))
        .py(px(12.))
}

/// One setting: its name and help, and its control at the end.
fn row(label: &str, help: Option<&str>, control: AnyElement) -> Row {
    let element = card_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(label.to_string()),
                )
                .when_some(help, |el, help| el.child(help_text(help))),
        )
        .child(control_slot(control));
    Row::new(element, label, help)
}

/// The end of a row, where its control sits: [`CONTROL_WIDTH`] wide, the
/// control against its trailing edge.
fn control_slot(control: AnyElement) -> Div {
    div()
        .flex_none()
        .min_w(px(CONTROL_WIDTH))
        .flex()
        .justify_end()
        .child(control)
}

fn help_text(text: &str) -> Div {
    div()
        .text_size(px(12.))
        .text_color(ladder::muted_foreground())
        .child(text.to_string())
}

/// A boolean setting: the whole row presses, and the box sits at its end.
fn checkbox(
    app: &Entity<Luma>,
    key: &'static str,
    label: &str,
    checked: bool,
    help: Option<&str>,
) -> Row {
    let app = app.clone();
    let Row { words, element } = row(
        label,
        help,
        luma_ui::float::checkbox(checked).into_any_element(),
    );
    let element = div().child(
        element
            .id(key)
            .cursor_pointer()
            .hover(|style| style.bg(glass::wash(glass::WASH_SUBTLE)))
            .on_click(move |_, _, cx| {
                let value = (!checked).to_string();
                app.update(cx, |this, cx| this.write_setting(key, value, cx));
            })
            .agent_node(Role::Checkbox, label),
    );
    Row { words, element }
}

/// The app's one select ([`luma_arg_select`]), over this screen's `(id, label)`
/// options: labels are what the chip and its rows show, ids are what the
/// setting stores.
///
/// The pair-to-index translation is all this adds. The trigger, the anchored
/// card, the rows and their selected state are the widget's — a second copy
/// of that wiring here is how the screen ended up with a slab trigger under a
/// glass menu while the strip had a chip.
fn select(
    state: &Settings,
    app: &Entity<Luma>,
    key: &'static str,
    options: &[(&str, &str)],
    value: &str,
) -> AnyElement {
    let labels: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
    let ids: Vec<String> = options.iter().map(|(id, _)| (*id).to_string()).collect();
    let toggle = app.clone();
    let pick = app.clone();
    luma_arg_select(
        key,
        label_of(options, value),
        &labels,
        state.open_menu == Some(key),
        move |_, cx| toggle.update(cx, |this, cx| this.toggle_menu(key, cx)),
        move |index, _, cx| {
            let Some(id) = ids.get(index).cloned() else {
                return;
            };
            // `write_setting` closes the menu itself — it is the one place
            // that knows the write started.
            pick.update(cx, |this, cx| this.write_setting(key, id, cx));
        },
    )
    .into_any_element()
}

/// A value this host can show but not yet edit, drawn in the input's box so
/// the section keeps its shape.
fn readonly_value(value: &str, placeholder: &str) -> AnyElement {
    let empty = value.is_empty();
    let text = if empty { placeholder } else { value };
    luma_ui::float::field()
        .w(px(CONTROL_WIDTH))
        .when(empty, |field| field.text_color(ladder::muted_foreground()))
        .child(text.to_string())
        .agent_node(Role::Input, text)
        .agent_disabled(true)
        .into_any_element()
}

/// The label an option id shows as, or the id itself if the list has moved on
/// under a stored value.
fn label_of<'a>(options: &'a [(&'a str, &'a str)], value: &'a str) -> &'a str {
    options
        .iter()
        .find(|(id, _)| *id == value)
        .map(|(_, label)| *label)
        .unwrap_or(value)
}
