//! `luma-app` — the window around [`luma_app::Luma`].

use gpui::*;
use gpui_component::Root;
use luma_app::{Library, Luma};

fn main() {
    // No flags. The app opens on the venue grid and every screen past it is
    // reached by pressing something — a flag that jumped straight to one would
    // be a second way in, and the automation harness drives the first one.
    if let Some(arg) = std::env::args().nth(1) {
        eprintln!("usage: luma-app  (unexpected `{arg}`)");
        std::process::exit(2);
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
            let luma = cx.new(|cx| Luma::new(library, cx));
            cx.new(|cx| Root::new(luma, window, cx).bordered(false))
        })
        .expect("failed to open the Luma window");
        cx.activate(true);
    });
}
