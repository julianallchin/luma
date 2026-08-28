//! The settings screen: the app's key/value table, typed and editable.
//!
//! Mirrors `src/features/settings/components/settings-dialog.tsx` — the same
//! four sections behind the same `<ToggleGroup>`, the same controls and the
//! same helper copy under each. The web version is a modal dialog; here it is
//! a screen, because a native host has one window and no portal layer, and a
//! full-window panel is what the ladder already knows how to draw.
//!
//! # The write path
//!
//! Every control writes through `set_setting` on the seam and then re-reads
//! `get_settings`. Nothing is applied optimistically: the screen shows what
//! the database holds, so a write that silently failed shows as the control
//! springing back rather than as a lie that survives until the next reload.
//! (The web side debounces its slider for the same reason it is *not*
//! debounced here — see the max-brightness note below.)

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::arg::select::luma_arg_select;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::Enabled;

use luma_lib::settings::{AppSettings, AGENT_MODELS, AGENT_PROVIDERS};

use crate::Luma;

/// Which section is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Account,
    Ai,
    ArtNet,
    About,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::General, Tab::Account, Tab::Ai, Tab::ArtNet, Tab::About];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Account => "Account",
            Tab::Ai => "AI",
            Tab::ArtNet => "Art-Net / DMX",
            Tab::About => "About",
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
    /// The principal admitted when this screen opened, and after any sign-out
    /// it performs. A snapshot rather than a live read for the same reason
    /// `values` is one: the screen shows what the host said, so a transition
    /// that failed shows as the row not changing.
    account: Option<String>,
    /// A sign-out is in flight. It is a multi-command durability boundary —
    /// flush, wipe, release — and pressing it twice would race its own journal.
    signing_out: bool,
    account_error: Option<String>,
}

impl Settings {
    fn new(account: Option<String>) -> Self {
        Self {
            tab: Tab::General,
            values: None,
            error: None,
            open_menu: None,
            account,
            signing_out: false,
            account_error: None,
        }
    }
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
        let account = self.library.user_id();
        self.overlay
            .open(crate::shell::Overlay::Settings(Settings::new(account)));
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

    /// Write one setting, then read every setting back.
    /// Leave settings for the sign-in gate. One dialog is up at a time, so
    /// this is a replacement, not a stack.
    fn sign_in_from_settings(&mut self, cx: &mut Context<Self>) {
        self.close_overlay(cx);
        self.show_sign_in(cx);
    }

    /// Sign out, and show the result where it was asked for. The screen is not
    /// dismissed on success: sign-out is a long durability boundary and the row
    /// changing to "Signed out" is the only honest proof it landed.
    fn sign_out(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::Settings(state)) = self.overlay.open_mut() else {
            return;
        };
        if state.signing_out {
            return;
        }
        state.signing_out = true;
        state.account_error = None;
        let pending = self.library.sign_out();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.with_settings(cx, |state| {
                    state.signing_out = false;
                    match result {
                        Ok(()) => {
                            state.account = None;
                            state.account_error = None;
                        }
                        Err(error) => {
                            state.account_error = Some(format!("Could not sign out: {error}"));
                        }
                    }
                });
            })
            .ok();
        })
        .detach();
    }

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

    fn reload_settings(&self, cx: &mut Context<Self>) {
        let pending = self.library.settings();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
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

const PAD: f32 = 16.;
/// Gap between one labelled control and the next.
const SECTION_GAP: f32 = 20.;

/// Render the screen. `app` is the root entity every control writes through.
pub fn settings(state: &Settings, app: &Entity<Luma>) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        // The shared dialog host paints the card surface; route content stays
        // transparent so a future renderer effect can be upgraded centrally.
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        .child(match (&state.values, &state.error) {
            (None, None) => luma_ui::plate("Loading…", ladder::muted_foreground()),
            (None, Some(error)) => luma_ui::plate(
                format!("Failed to load settings: {error}"),
                ladder::danger(),
            ),
            (Some(values), error) => body(state, values, error.as_deref(), app).into_any_element(),
        })
}

/// The way back, the screen's name, and the section picker.
fn toolbar(state: &Settings, app: &Entity<Luma>) -> Div {
    let back = app.clone();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(PAD))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            luma_ui::luma_button("Back", Enabled::Yes)
                .id("settings-back")
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.close_settings(cx)))
                .agent_node(Role::Button, "Back"),
        )
        .child(luma_ui::silkscreen("SETTINGS"))
        .child(div().flex_1())
        .child(tabs(state, app))
}

fn tabs(state: &Settings, app: &Entity<Luma>) -> Div {
    div()
        .flex()
        .children(Tab::ALL.into_iter().enumerate().map(|(index, tab)| {
            let app = app.clone();
            luma_ui::luma_toggle_segment(tab.label(), tab == state.tab, index == 0)
                .id(tab.label())
                .tab_index(0)
                .on_click(move |_, _, cx| app.update(cx, |this, cx| this.show_tab(tab, cx)))
                .agent_node(Role::Toggle, tab.label())
        }))
}

fn body(state: &Settings, values: &AppSettings, error: Option<&str>, app: &Entity<Luma>) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(SECTION_GAP))
        .p(px(PAD))
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
            Tab::Account => account(state, app),
            Tab::Ai => ai(state, values, app),
            Tab::ArtNet => artnet(values, app),
            Tab::About => about(),
        })
}

fn general(values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
    vec![field(
        None,
        checkbox(
            app,
            "audio_output_enabled",
            "Enable Audio Output",
            values.audio_output_enabled,
        ),
        Some("When disabled, playback stays in sync but stays silent."),
    )]
}

fn ai(state: &Settings, values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
    vec![
        field(
            Some("Model Provider"),
            select(
                state,
                app,
                "agent_provider",
                AGENT_PROVIDERS,
                &values.agent_provider,
            ),
            Some(
                "The service Luma's agents call. Keys are stored per provider — \
                 switching keeps both.",
            ),
        ),
        field(
            Some("Model"),
            select(state, app, "agent_model", AGENT_MODELS, &values.agent_model),
            Some(
                "The model behind the track agent. Applies from the next message, \
                 including in open threads.",
            ),
        ),
        // The web dialog edits both providers' API keys here. They live in the
        // browser's localStorage, which a native host does not have and should
        // not grow an imitation of — a secret belongs in the OS keychain, not
        // in a settings row. Said plainly rather than drawn as a dead control.
        note("API keys are not editable from the native host yet."),
    ]
}

fn artnet(values: &AppSettings, app: &Entity<Luma>) -> Vec<Div> {
    vec![
        field(
            Some("Max Brightness"),
            // Whole percent: the setting is stored as a `u8` and read back as
            // one, so a slider handing over 61.4 would show 61 on the next
            // reload and disagree with itself in between.
            luma_ui::luma_slider("max_dimmer", values.max_dimmer as f32, 0., 100., 240., {
                let app = app.clone();
                let current = values.max_dimmer;
                move |value, _, cx| {
                    let level = value.round().clamp(0., 100.) as u8;
                    if level != current {
                        app.update(cx, |this, cx| {
                            this.write_setting("max_dimmer", level.to_string(), cx);
                        });
                    }
                }
            })
            .agent_node(
                Role::Slider,
                format!("Max Brightness {}", values.max_dimmer),
            )
            .into_any_element(),
            Some("Limits overall brightness of DMX output (100 = no limit)."),
        ),
        field(
            None,
            checkbox(
                app,
                "artnet_enabled",
                "Enable Art-Net Output",
                values.artnet_enabled,
            ),
            None,
        ),
        field(
            None,
            checkbox(
                app,
                "artnet_broadcast",
                "Always Broadcast (255.255.255.255)",
                values.artnet_broadcast,
            ),
            None,
        ),
        field(
            Some("Interface IP (Bind Address)"),
            readonly_value(&values.artnet_interface, "0.0.0.0"),
            Some("0.0.0.0 binds to all interfaces."),
        ),
        field(
            Some("Unicast Destination IP"),
            readonly_value(&values.artnet_unicast_ip, "Broadcast only"),
            None,
        ),
        field(
            Some("Net / Subnet"),
            readonly_value(
                &format!("{} / {}", values.artnet_net, values.artnet_subnet),
                "",
            ),
            None,
        ),
        // Node discovery is a poll loop over start/stop/get_discovered_nodes
        // and the text fields above are the only thing that consumes its
        // result, so it lands with them.
        note("Text fields and node discovery are not editable from the native host yet."),
    ]
}

/// Who this library belongs to, and the one gesture that changes it.
///
/// The web app hangs sign-out off a titlebar dropdown; there is no titlebar
/// here (see `crate::chrome`), and the gear that opens this screen sits in the
/// same corner that dropdown did. So this *is* the account menu.
fn account(state: &Settings, app: &Entity<Luma>) -> Vec<Div> {
    let signed_in = state.account.is_some();
    let identity = state
        .account
        .clone()
        .unwrap_or_else(|| "Signed out — working locally".to_string());
    let label = if state.signing_out {
        "Signing out…"
    } else if signed_in {
        "Sign out"
    } else {
        "Sign in"
    };
    let pressable = !state.signing_out;
    let act = app.clone();
    let mut rows = vec![
        field(Some("Signed in as"), readonly_value(&identity, ""), None),
        field(
            None,
            luma_ui::luma_button(label, if pressable { Enabled::Yes } else { Enabled::No })
                .id("settings-account-action")
                .when(pressable, |button| {
                    button.on_click(move |_, _, cx| {
                        act.update(cx, |this, cx| {
                            if signed_in {
                                this.sign_out(cx);
                            } else {
                                this.sign_in_from_settings(cx);
                            }
                        });
                    })
                })
                .agent_node(Role::Button, label)
                .agent_disabled(!pressable)
                .into_any_element(),
            Some(if signed_in {
                "Signing out flushes this library to the cloud, then removes the signed-in copy. Guest work stays."
            } else {
                "Signing in claims the rows you write from here; work already done as a guest stays a guest's."
            }),
        ),
    ];
    if let Some(error) = &state.account_error {
        rows.push(field(
            None,
            div()
                .max_w(px(560.))
                .text_size(px(12.))
                .text_color(ladder::danger())
                .child(error.clone())
                .agent_node(Role::Text, error.clone())
                .into_any_element(),
            None,
        ));
    }
    rows
}

fn about() -> Vec<Div> {
    vec![
        field(
            None,
            luma_ui::silkscreen(format!("LUMA V{}", luma_lib::VERSION)).into_any_element(),
            None,
        ),
        field(
            None,
            luma_ui::luma_button("Check for Updates", Enabled::No)
                .agent_node(Role::Button, "Check for Updates")
                .agent_disabled(true)
                .into_any_element(),
            Some("Updates are delivered by the desktop host, not this one."),
        ),
    ]
}

// -- the pieces every section is built from -----------------------------------

/// One labelled control with its helper line: the `space-y-2` stack the web
/// dialog repeats for every setting.
fn field(label: Option<&str>, control: AnyElement, help: Option<&str>) -> Div {
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(8.))
        .when_some(label, |el, label| {
            el.child(
                div()
                    .text_size(px(9.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(ladder::foreground_90())
                    .child(label.to_uppercase()),
            )
        })
        .child(control)
        .when_some(help, |el, help| el.child(help_text(help)))
}

fn help_text(text: &str) -> Div {
    div()
        .text_size(px(12.))
        .text_color(ladder::muted_foreground())
        .child(text.to_string())
}

/// A standalone note — the same voice as a field's helper line, with no
/// control above it.
fn note(text: &str) -> Div {
    help_text(text)
}

/// A checkbox and its label, pressable as one row (`<Checkbox>` + `<Label>`).
fn checkbox(app: &Entity<Luma>, key: &'static str, label: &str, checked: bool) -> AnyElement {
    let app = app.clone();
    div()
        .id(key)
        .flex()
        .items_center()
        .gap(px(8.))
        .child(luma_ui::luma_checkbox(checked))
        .child(div().text_size(px(12.)).child(label.to_string()))
        .on_click(move |_, _, cx| {
            let value = (!checked).to_string();
            app.update(cx, |this, cx| this.write_setting(key, value, cx));
        })
        .agent_node(Role::Checkbox, label)
        .into_any_element()
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
/// the section keeps the web dialog's shape.
fn readonly_value(value: &str, placeholder: &str) -> AnyElement {
    let empty = value.is_empty();
    let text = if empty { placeholder } else { value };
    luma_ui::luma_input(text, empty, 240.)
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
