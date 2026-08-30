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

    // Resolve the environment's answer once, on the thread that will own the
    // app, so nothing downstream re-reads it. The harness installs its own
    // instead — see `luma_ui::runtime`.
    luma_ui::runtime::Runtime::default().install();

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
            // No *visible* native chrome: `chrome`'s head bands draw it, the same
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
            // Blurred, because the shell's whole chrome tier is translucent:
            // the plane, the titlebar and the sidebar are `luma_ui::glass`
            // surfaces and there is nothing behind them but this. Instrument
            // cards paint opaque over it. If this is ever set at runtime, only
            // `Blurred` keeps the backing `NSVisualEffectView` alive — see the
            // function's own docs.
            //
            // Straight from `luma_ui`: the chat re-exports this, but reading a
            // window property through the chat's palette would say the chat
            // owns it, and it belongs to the tier the whole shell paints in.
            window_background: luma_ui::glass::window_background_appearance(),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            luma_app::hide_native_window_buttons(window);
            // As early as there is a window, because compiling every render
            // pipeline is the longest thing between launch and a first frame
            // — and not before, because on a wgpu compositor the pipelines
            // belong on the window's own device. Returns immediately; the
            // stage reports the progress (see `visualizer::body`).
            luma_app::warm_renderer(window);
            let luma = cx.new(|cx| Luma::new(library, cx));
            cx.new(|cx| Root::new(luma, window, cx).bordered(false))
        })
        .expect("failed to open the Luma window");
        cx.activate(true);
    });
}
