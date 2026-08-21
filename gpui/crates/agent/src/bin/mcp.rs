//! `gpui-agent-mcp` — MCP over stdio, driving the real Luma app.
//!
//! ```text
//! gpui-agent-mcp [--pixel] [--seed N] [--size WxH]
//! ```
//!
//! Reads the library the same way the app does, so `LUMA_CONFIG_DIR` points it
//! at a disposable database:
//!
//! ```sh
//! LUMA_CONFIG_DIR=/tmp/luma-agent cargo run -p gpui-agent --bin gpui-agent-mcp
//! ```
//!
//! # Which thread is which
//!
//! The pump runs on `main` and stdio runs on a thread, which looks backwards
//! for a stdio server. The app is the thing this process exists to hold: it is
//! the only `!Send` state here, it outlives any single request, and giving it
//! `main` means the process ends exactly when it does.

use std::io::{BufReader, Write};
use std::sync::Arc;

use gpui::{px, size, AppContext as _};
#[cfg(feature = "pixel")]
use gpui_agent::Mode;
use gpui_agent::{mcp, pump, Config, Harness};

fn main() {
    let config = match parse_args() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: gpui-agent-mcp [--pixel] [--seed N] [--size WxH]");
            std::process::exit(2);
        }
    };

    // Fail here rather than inside the pump: a library that will not open is a
    // startup problem, and an MCP client deserves to see it on stderr instead
    // of as a panic on the first `exec`.
    if let Err(error) = luma_app::Library::open() {
        eprintln!("[gpui-agent] could not open the library: {error}");
        std::process::exit(1);
    }

    let root = Arc::new(|_: &mut gpui::Window, cx: &mut gpui::App| {
        luma_app::init(cx);
        // Reopened per build so that `reset` gets a clean connection; the
        // failure path is already ruled out above.
        let library = luma_app::Library::open().expect("the library stopped opening");
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });

    pump::run(config, root, |client| {
        std::thread::Builder::new()
            .name("gpui-agent-mcp".into())
            .spawn(move || {
                let mut harness = match Harness::new(client) {
                    Ok(harness) => harness,
                    Err(error) => {
                        eprintln!("[gpui-agent] {error}");
                        std::process::exit(1);
                    }
                };
                let stdout = std::io::stdout();
                if let Err(error) = mcp::serve(
                    &mut harness,
                    BufReader::new(std::io::stdin()),
                    stdout.lock(),
                ) {
                    eprintln!("[gpui-agent] stdio closed: {error}");
                }
                let _ = std::io::stderr().flush();
                // The pump is parked on `main` waiting for commands that will
                // never come now, and it has no other way to learn that.
                std::process::exit(0);
            })
            .expect("failed to spawn the stdio thread");
    });
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pixel" => {
                #[cfg(feature = "pixel")]
                {
                    config.mode = Mode::Pixel;
                }
                #[cfg(not(feature = "pixel"))]
                return Err("--pixel needs the crate built with `--features pixel`".into());
            }
            "--seed" => {
                let value = args.next().ok_or("--seed needs a number")?;
                config.seed = value.parse().map_err(|_| format!("bad seed: {value}"))?;
            }
            "--size" => {
                let value = args.next().ok_or("--size needs WxH")?;
                let (width, height) = value.split_once('x').ok_or("--size must be WxH")?;
                config.window_size = size(
                    px(width.parse().map_err(|_| format!("bad width: {width}"))?),
                    px(height
                        .parse()
                        .map_err(|_| format!("bad height: {height}"))?),
                );
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    Ok(config)
}
