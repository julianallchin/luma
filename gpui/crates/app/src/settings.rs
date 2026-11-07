//! The settings screen: the app's key/value table, typed and editable.
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
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::Enabled;
use luma_ui::{glass, ladder, radius};

use luma_lib::settings::{
    agent_models, key, ApiKeyStatus, AppSettings, AGENT_PROVIDERS, MAX_DIMMER_RANGE,
};

use crate::Luma;

/// Which section is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Account,
    Ai,
    Output,
    About,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::General, Tab::Account, Tab::Ai, Tab::Output, Tab::About];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Account => "Account",
            Tab::Ai => "AI",
            Tab::Output => "DMX Output",
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
    error: Option<String>,
    /// The setting key of the select whose menu is open. One at a time, and
    /// the key is enough of an identity because no two selects on this screen
    /// write the same setting.
    open_menu: Option<&'static str>,
    /// One key field per [`AGENT_PROVIDERS`] service, in that order.
    keys: Vec<KeyField>,
}

impl Settings {
    fn new(cx: &mut Context<Luma>) -> Self {
        Self {
            tab: Tab::General,
            values: None,
            error: None,
            open_menu: None,
            keys: AGENT_PROVIDERS
                .iter()
                .map(|(provider, label)| KeyField::new(provider, label, cx))
                .collect(),
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
/// sign-out is mid-flight, and what it said if it failed.
#[derive(Default)]
pub(crate) struct AccountAction {
    /// A sign-out is in flight. It is a multi-command durability boundary —
    /// flush, wipe, release — and pressing it twice would race its own journal.
    pub(crate) signing_out: bool,
    pub(crate) error: Option<String>,
}

// -- navigation and writes ----------------------------------------------------
//
// These hang off `Luma` rather than off `Settings` because a settings write is
// a `Library` call and a screen transition, and `Luma` is what owns both. The
// impl lives here so the router stays a router.

impl Luma {
    /// Open settings over the whole shell. The regions persist beneath the
    /// overlay, so closing it reveals them exactly as they were.
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.overlay.as_open(),
            Some(crate::shell::Overlay::Settings(_))
        ) {
            return;
        }
        let state = Settings::new(cx);
        self.overlay.open(crate::shell::Overlay::Settings(state));
        cx.notify();
        self.reload_settings(cx);
    }

    /// Dismiss the overlay. The Back button's handler; Escape lands in
    /// [`Luma::dismiss_overlay`] and does the same thing.
    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.overlay.as_open(),
            Some(crate::shell::Overlay::Settings(_))
        ) {
            self.close_overlay(cx);
            cx.notify();
        }
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

    /// Sign out. Nothing is dismissed on success: sign-out is a long
    /// durability boundary, and the account reading "Working locally" is the
    /// only honest proof it landed.
    ///
    /// Hangs off the app rather than the settings screen because both doors to
    /// it — the sidebar's account menu and the settings row — press the same
    /// three commands, and a guard living on one screen could not stop the
    /// other from pressing them again mid-flight.
    pub(crate) fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.account_action.signing_out {
            return;
        }
        self.account_action = AccountAction {
            signing_out: true,
            error: None,
        };
        let pending = self.library.sign_out();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.account_action = AccountAction {
                    signing_out: false,
                    error: result
                        .as_ref()
                        .err()
                        .map(|error| format!("Could not sign out: {error}")),
                };
                // Signed out is the interval before the next sign-in, not a
                // state the shell shows: the library is nobody's now, so the
                // gate is the next screen.
                if result.is_ok() {
                    this.show_sign_in(false, cx);
                }
                cx.notify();
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
                    this.with_settings(cx, |state| state.error = Some(error.to_string()))
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
        let Some(crate::shell::Overlay::Settings(state)) = self.overlay.as_open() else {
            return None;
        };
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
                Err(error) => this.with_settings(cx, |state| state.error = Some(error.to_string())),
            })
            .ok();
        })
        .detach();
    }

    /// Put each stored key's hint in its field's placeholder. Here rather than
    /// in the render because a field's placeholder is the field's own state.
    fn show_key_hints(&self, values: &AppSettings, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::Settings(state)) = self.overlay.as_open() else {
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
                this.with_settings(cx, |state| match result {
                    Ok(values) => {
                        state.values = Some(values);
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error.to_string()),
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
        if self.hdr_output_allowed != values.hdr_output {
            self.hdr_output_allowed = values.hdr_output;
            cx.notify();
        }
        if self.dmx_output != values.dmx_output_enabled {
            self.dmx_output = values.dmx_output_enabled;
            cx.notify();
        }
        self.set_auto_update(values.auto_update);
    }

    /// The one gesture that changes who this library belongs to, in whichever
    /// direction it currently points. The venue menu's account row, and the
    /// settings screen's account button — one path, so the in-flight guard
    /// cannot be walked around by pressing the other door.
    pub(crate) fn switch_identity(&mut self, cx: &mut Context<Self>) {
        if self.library.user_id().is_some() {
            self.sign_out(cx);
        } else {
            self.show_sign_in(false, cx);
        }
    }

    /// Run `edit` against the settings overlay's state, if it is still up. A
    /// load that lands after it was dismissed is a no-op, not an overlay that
    /// snaps back.
    fn with_settings(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Settings)) {
        if let Some(crate::shell::Overlay::Settings(state)) = self.overlay.open_mut() {
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

/// What the account row says when nobody is signed in — the guest namespace,
/// which is a working library and not a failure.
pub(crate) const GUEST_ACCOUNT: &str = "Working locally";

/// The nav's width, divided from the page by an inner hairline.
const NAV_WIDTH: f32 = 196.;
/// The page column: zeron's settings measure.
const PAGE_WIDTH: f32 = 640.;
/// A row's floor; a row with help text grows past it.
const ROW_HEIGHT: f32 = 52.;
/// The control slot at a row's end. A wider control (a key field and its
/// buttons) grows it; nothing narrows it.
const CONTROL_WIDTH: f32 = 240.;
/// Air between the page's sections.
const SECTION_GAP: f32 = 24.;
/// A card's fill: zeron's `#ffffff08`.
const CARD_ALPHA: f32 = 0x08 as f32 / 255.0;

/// Render the screen. `app` is the root entity every control writes through;
/// `shell` is the app state the account section reads its identity from.
pub fn settings(shell: &Luma, state: &Settings, app: &Entity<Luma>) -> Div {
    div()
        .size_full()
        .flex()
        .flex_row()
        // The shared dialog host paints the card surface; route content stays
        // transparent so a future renderer effect can be upgraded centrally.
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
                .child(match (&state.values, &state.error) {
                    (None, None) => luma_ui::plate("Loading…", ladder::muted_foreground()),
                    (None, Some(error)) => luma_ui::plate(
                        format!("Failed to load settings: {error}"),
                        ladder::danger(),
                    ),
                    (Some(values), error) => {
                        page(shell, state, values, error.as_deref(), app).into_any_element()
                    }
                }),
        )
}

/// The way back and the sections, one row each.
fn nav(state: &Settings, app: &Entity<Luma>) -> Div {
    let back = app.clone();
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
            luma_ui::float::nav_row(RowState::Rest, "settings-back")
                .id("settings-back")
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.close_settings(cx)))
                .child(
                    gpui_component::Icon::new(luma_ui::icons::IconName::ChevronLeft).size(px(12.)),
                )
                .child("Back")
                .agent_node(Role::Button, "Back"),
        )
        .child(div().h(px(8.)))
        .children(Tab::ALL.into_iter().map(|tab| {
            let app = app.clone();
            luma_ui::float::nav_row(RowState::of(tab == state.tab, false), tab.label())
                .id(tab.label())
                .tab_index(0)
                .on_click(move |_, _, cx| app.update(cx, |this, cx| this.show_tab(tab, cx)))
                .child(tab.label())
                .agent_node(Role::Toggle, tab.label())
        }))
}

/// The page column: the section's title and description, a refused write,
/// and the section's own groups.
fn page(
    shell: &Luma,
    state: &Settings,
    values: &AppSettings,
    error: Option<&str>,
    app: &Entity<Luma>,
) -> Div {
    div()
        .w_full()
        .max_w(px(PAGE_WIDTH))
        .flex()
        .flex_col()
        .gap(px(SECTION_GAP))
        .px(px(24.))
        .pt(px(32.))
        .pb(px(40.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(state.tab.label())
                        .agent_node(Role::Text, state.tab.label()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(glass::ink(glass::TEXT_SECONDARY))
                        .child(state.tab.description()),
                ),
        )
        .when_some(error, |el, message| {
            el.child(
                div()
                    .text_size(px(12.))
                    .text_color(ladder::danger())
                    .child(format!("Failed to save: {message}"))
                    .agent_node(Role::Text, format!("Failed to save: {message}")),
            )
        })
        .children(match state.tab {
            Tab::General => general(values, app),
            Tab::Account => account(shell, app),
            Tab::Ai => ai(state, values, app),
            Tab::Output => output(values, app),
            Tab::About => about(shell, values, app),
        })
}

fn general(values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
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
            vec![checkbox(
                app,
                key::HDR_OUTPUT,
                "HDR output",
                values.hdr_output,
                Some("Used only when the display supports HDR. Bright stage light can go above the brightness of white in other windows."),
            )],
            None,
        ),
    ]
}

fn ai(state: &Settings, values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
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
fn api_key(key: &KeyField, status: &ApiKeyStatus, app: &Entity<Luma>) -> Div {
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
                .when(key.has_draft, |button| {
                    button.on_click(move |_, _, cx| {
                        save.update(cx, |this, cx| this.save_api_key(provider, cx));
                    })
                })
                .agent_node(Role::Button, format!("Save {label}"))
                .agent_disabled(!key.has_draft),
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

fn output(values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
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
                    luma_ui::luma_slider(
                        key::MAX_DIMMER,
                        values.max_dimmer as f32,
                        dimmer_low,
                        dimmer_high,
                        CONTROL_WIDTH,
                        {
                            let app = app.clone();
                            let current = values.max_dimmer;
                            move |value, _, cx| {
                                let level = value.round().clamp(dimmer_low, dimmer_high) as u8;
                                if level != current {
                                    app.update(cx, |this, cx| {
                                        this.write_setting(
                                            key::MAX_DIMMER,
                                            level.to_string(),
                                            cx,
                                        );
                                    });
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

/// Who this library belongs to, and the one gesture that changes it.
fn account(shell: &Luma, app: &Entity<Luma>) -> Vec<Div> {
    let account = shell.library.account();
    let signed_in = account.is_some();
    // The address where the session carries one, the principal where it does
    // not — one spelling of "who this is", shared with the venue menu.
    let identity = account.as_ref().map_or_else(
        || GUEST_ACCOUNT.to_string(),
        |account| account.label().to_string(),
    );
    let label = if shell.account_action.signing_out {
        "Signing out…"
    } else if signed_in {
        "Sign out"
    } else {
        "Sign in"
    };
    let pressable = !shell.account_action.signing_out;
    let act = app.clone();
    let mut rows = vec![
        row("Signed in as", None, readonly_value(&identity, "")),
        row(
            if signed_in { "Sign out" } else { "Sign in" },
            Some(if signed_in {
                "Signing out flushes this library to the cloud, then removes the signed-in copy. Guest work stays."
            } else {
                "Signing in claims the rows you write from here; work already done as a guest stays a guest's."
            }),
            luma_ui::button(label, Enabled::from(pressable))
                .id("settings-account-action")
                .when(pressable, |button| {
                    button.on_click(move |_, _, cx| {
                        act.update(cx, |this, cx| this.switch_identity(cx));
                    })
                })
                .agent_node(Role::Button, label)
                .agent_disabled(!pressable)
                .into_any_element(),
        ),
    ];
    if let Some(error) = &shell.account_action.error {
        rows.push(
            card_row().child(
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error.clone()),
            ),
        );
    }
    vec![section("Account", rows, None)]
}

fn about(shell: &Luma, values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
    let channel = match luma_update::Channel::CURRENT {
        luma_update::Channel::Stable => String::new(),
        channel => format!(" ({})", channel.label()),
    };
    let (button, status) = crate::update::about(shell, app);
    let version = format!("Luma v{}{channel}", luma_lib::VERSION);
    let version_row = card_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(luma_ui::caption(version))
                .when_some(status, |el, status| el.child(help_text(&status))),
        )
        .children(button.map(control_slot));
    vec![
        section("Version", vec![version_row], None),
        section(
            "Updates",
            vec![checkbox(
                app,
                key::AUTO_UPDATE,
                "Update automatically",
                values.auto_update,
                Some("Checks for a new release every hour and downloads it in the background. When it is ready, Luma asks whether to relaunch now; otherwise it runs the next time you open Luma."),
            )],
            None,
        ),
    ]
}

// -- the pieces every section is built from -----------------------------------

/// A section: its small title over a card of rows, hairlines between them,
/// and a footnote under the card when the section has one.
fn section(title: &str, rows: Vec<Div>, note: Option<&str>) -> Div {
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .px(px(4.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .child(title.to_string()),
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
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    row.when(index + 1 < count, |row| {
                        row.border_b_1().border_color(glass::inner_rule())
                    })
                })),
        )
        .when_some(note, |el, note| {
            el.child(div().px(px(4.)).child(help_text(note)))
        })
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
fn row(label: &str, help: Option<&str>, control: AnyElement) -> Div {
    card_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(13.)).child(label.to_string()))
                .when_some(help, |el, help| el.child(help_text(help))),
        )
        .child(control_slot(control))
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
) -> Div {
    let app = app.clone();
    let row = row(
        label,
        help,
        luma_ui::float::checkbox(checked).into_any_element(),
    );
    div().child(
        row.id(key)
            .cursor_pointer()
            .hover(|style| style.bg(glass::wash(glass::WASH_SUBTLE)))
            .on_click(move |_, _, cx| {
                let value = (!checked).to_string();
                app.update(cx, |this, cx| this.write_setting(key, value, cx));
            })
            .agent_node(Role::Checkbox, label),
    )
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
