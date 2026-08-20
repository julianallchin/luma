//! `luma-app [--venue <name|id>]` — the window around [`luma_app::Luma`].

use gpui::*;
use gpui_component::Root;
use luma_app::{Library, Luma};

fn main() {
    // `luma-app [--venue <name|id>]` — opening straight to a venue is how the
    // track browser is reachable without a pointer, which a headless capture
    // or a shell alias both want.
    let mut args = std::env::args().skip(1);
    let mut open_on_start = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--venue" => open_on_start = args.next(),
            other => {
                eprintln!("usage: luma-app [--venue <name|id>]  (unexpected `{other}`)");
                std::process::exit(2);
            }
        }
    }

    let library = match Library::open() {
        Ok(library) => library,
        Err(error) => {
            eprintln!("[luma] could not open the library: {error}");
            std::process::exit(1);
        }
    };

    // Icons are SVGs embedded by gpui-component's assets crate; without an
    // asset source every `Icon` silently renders nothing.
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        luma_app::init(cx);

        let options = WindowOptions {
            // No native chrome anywhere: `chrome::titlebar` draws it, the same
            // choice `decorations: false` makes for the Tauri window.
            titlebar: None,
            window_decorations: Some(WindowDecorations::Client),
            app_owns_titlebar_drag: true,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(120.), px(120.)),
                size: size(px(1200.), px(800.)),
            })),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            let luma = cx.new(|cx| Luma::new(library, open_on_start, cx));
            cx.new(|cx| Root::new(luma, window, cx).bordered(false))
        })
        .expect("failed to open the Luma window");
        cx.activate(true);
    });
}
