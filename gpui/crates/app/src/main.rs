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
            // No *visible* native chrome: `chrome::titlebar` draws it, the same
            // choice `decorations: false` makes for the Tauri window. The
            // titlebar is still requested, transparent and title-less, because
            // that is the only branch of gpui's macOS window that honours
            // `is_resizable` — `titlebar: None` pins the style mask to
            // `Titled | FullSizeContentView`, and without `Resizable` AppKit
            // gives no edge-resize and silently ignores `zoom:`. The system
            // buttons that come with the mask are hidden right after the
            // window opens; see `hide_native_window_buttons`.
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_decorations: Some(WindowDecorations::Client),
            app_owns_titlebar_drag: true,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(120.), px(120.)),
                size: size(px(1200.), px(800.)),
            })),
            // Below this the panel rows stop being readable; the graph editor
            // and the agent pane both reserve fixed gutters.
            window_min_size: Some(size(px(800.), px(600.))),
            // Blurred so the chat panel's glass tier reads as vibrancy rather
            // than a flat-alpha blend. Every app surface outside the chat
            // paints opaque ladder values, so nothing else changes. If this is
            // ever set at runtime, only `Blurred` keeps the backing
            // `NSVisualEffectView` alive — see `luma_chat::theme`.
            window_background: luma_chat::theme::window_background_appearance(),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            luma_app::hide_native_window_buttons(window);
            let luma = cx.new(|cx| Luma::new(library, cx));
            cx.new(|cx| Root::new(luma, window, cx).bordered(false))
        })
        .expect("failed to open the Luma window");
        cx.activate(true);
    });
}
