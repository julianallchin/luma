//! Launching with every shape of stored session, through the production app
//! tree.
//!
//! The bug this pins: a spent Supabase refresh token used to abort `main`
//! before a window existed (`could not open the library: … refresh_token_
//! already_used`). Boot no longer asks the network anything, so the token's
//! liveness cannot decide whether Luma opens — only whether the *cloud* works,
//! which is a later, recoverable question. What decides the first screen is
//! whether the stored session still proves a principal offline.

#![cfg(feature = "app")]

use super::support;
use support::session::{self, Stored};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;

async fn seed(dir: &Path, stored: Stored) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open fixture app database");
    // Owned by whoever this launch will admit: `auth_visible_venues` shows the
    // admitted principal's rows, so seeing this venue is itself proof that
    // admission was armed for the right identity.
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', ?, 'Sign-in Venue')")
        .bind(session::owner(stored))
        .execute(&db.0)
        .await
        .expect("failed to seed venue");
    db.0.close().await;
    session::seed(dir, stored).await;
}

fn fixture_dir(name: &str, stored: Stored) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-signin-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start fixture runtime")
        .block_on(seed(&dir, stored));
    dir
}

fn harness(dir: &Path) -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
            luma_app::init(cx);
            // The assertion under test: this is the call that used to return
            // `Err` and take the process with it.
            let library = luma_app::Library::open().expect("failed to open fixture library");
            let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
            cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                .into()
        });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: support::runtime(dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start harness")
}

fn exec(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(90));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

#[test]
fn a_session_that_proves_nobody_launches_signed_out_at_the_gate() {
    let dir = fixture_dir("unproven", Stored::Unproven);
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        const gate = until("the sign-in gate", (s) =>
            s.find({ role: "card", label: "Sign-in dialog" }) !== undefined
                && s.find({ role: "text", label: "Sign in to Luma" }) !== undefined);
        ({
            field: gate.find({ role: "input", label: "you@example.com" }) !== undefined,
            submit: gate.find({ role: "button", label: "Send code" }) !== undefined,
            offline: gate.find({ role: "button", label: "Work offline" }) !== undefined,
            venues: gate.find({ role: "card", label: "Venue dialog" }) !== undefined,
            // Nothing failed. A launch that could not verify a session is a
            // signed-out launch, not an error to report.
            failure: gate.find((n) => n.role === "text"
                && n.label.toLowerCase().includes("could not open")) !== undefined,
            // Escape is the offline door, and it opens onto the venue picker
            // the launch was heading for.
            dismissed: (() => {
                app.key("escape");
                return until("the venue picker behind the gate", (s) =>
                    s.find({ role: "card", label: "Venue dialog" }) !== undefined
                        && s.find({ role: "card", label: "Sign-in dialog" }) === undefined)
                    !== undefined;
            })(),
        })
    "#,
    );
    assert_eq!(out["field"], true, "the gate shows an email field");
    assert_eq!(out["submit"], true, "the gate offers the committing chip");
    assert_eq!(out["offline"], true, "the gate offers a way past it");
    assert_eq!(out["venues"], false, "the gate replaces the venue picker");
    assert_eq!(out["failure"], false, "nothing is reported as a failure");
    assert_eq!(
        out["dismissed"], true,
        "escape works offline, it does not trap"
    );

    // The gate is not the only way back to it: settings is where an account
    // lives here, and a guest must be able to sign in from there later.
    let account = exec(
        &mut harness,
        r#"
        // The venue picker will not stand aside until a room is chosen, and
        // a guest's own rows are exactly what is still readable here.
        nav.venue("Sign-in Venue");
        const shell = until("the shell", (s) =>
            s.find({ role: "button", label: "Settings" }) !== undefined
                && s.find({ role: "card", label: "Venue dialog" }) === undefined);
        app.click(shell.find({ role: "button", label: "Settings" }));
        const settings = until("the settings dialog", (s) =>
            s.find({ role: "toggle", label: "Account" }) !== undefined);
        app.click(settings.find({ role: "toggle", label: "Account" }));
        const shown = until("the account section", (s) =>
            s.find({ role: "button", label: "Sign in" }) !== undefined);
        app.click(shown.find({ role: "button", label: "Sign in" }));
        const reopened = until("the gate reopened from settings", (s) =>
            s.find({ role: "card", label: "Sign-in dialog" }) !== undefined);
        ({
            identity: shown.find({ role: "input", label: "Signed out — working locally" })
                !== undefined,
            reopened: reopened.find({ role: "text", label: "Sign in to Luma" }) !== undefined,
        })
    "#,
    );
    assert_eq!(
        account["identity"], true,
        "the account section names the guest namespace"
    );
    assert_eq!(
        account["reopened"], true,
        "settings can reach the gate after it was dismissed"
    );
}

#[test]
fn a_proven_session_launches_past_the_gate_even_with_a_dead_refresh_token() {
    // Expired access token, spent refresh token: the exact state that used to
    // abort `main`. Boot proves the principal from the stored proof and never
    // reaches for the token, so the app opens where a signed-in app opens.
    let dir = fixture_dir("proven", Stored::Proven { expires_in: -3600 });
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        const shown = until("the venue picker", (s) =>
            s.find({ role: "card", label: "Sign-in Venue" }) !== undefined);
        ({ gate: shown.find({ role: "card", label: "Sign-in dialog" }) !== undefined })
    "#,
    );
    assert_eq!(out["gate"], false, "a proven session skips the gate");
}
