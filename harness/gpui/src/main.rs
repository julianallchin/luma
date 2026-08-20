use std::process::Command;
use std::time::Duration;

use gpui::*;
use gpui_component::Root;

mod fixtures;

// Renders one fixture in a borderless fixed-bounds window, captures it with
// `screencapture -R`, writes the PNG, and exits.
//
//   cargo run -- --fixture button [--out harness/shots/gpui/button.png]
//   cargo run -- --list

struct FixtureView {
    build: fn() -> AnyElement,
}

impl Render for FixtureView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Mirrors the web harness wrapper: bg-background (#272727) + p-6.
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x272727))
            .font_family("Inter")
            .child((self.build)())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fixtures = fixtures::all();

    if args.iter().any(|a| a == "--list") {
        for f in &fixtures {
            println!("{}", f.id);
        }
        return;
    }

    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let Some(id) = arg("--fixture") else {
        eprintln!("usage: luma-gpui-harness --fixture <id> [--out <png>] | --list");
        std::process::exit(1);
    };
    let Some(fixture) = fixtures.into_iter().find(|f| f.id == id) else {
        eprintln!("unknown fixture: {id}");
        std::process::exit(1);
    };
    let out = arg("--out").unwrap_or(format!("harness/shots/gpui/{id}.png"));
    if let Some(dir) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(dir).ok();
    }

    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);

        // The window is larger than the fixture area and the capture region is
        // inset by MARGIN: macOS rounds borderless-window corners, and the
        // inset keeps the desktop bleed out of the shot.
        const MARGIN: f32 = 24.;
        let bounds = Bounds {
            origin: point(px(200.), px(200.)),
            size: size(
                px(fixture.width + 2. * MARGIN),
                px(fixture.height + 2. * MARGIN),
            ),
        };
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        let build = fixture.build;

        cx.spawn(async move |cx| {
            let handle = cx
                .open_window(options, |window, cx| {
                    let view = cx.new(|_| FixtureView { build });
                    cx.new(|cx| Root::new(view, window, cx).bordered(false))
                })
                .expect("failed to open window");
            cx.update(|cx| cx.activate(true));

            // Let the first frames land before capturing.
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;

            let bounds = handle
                .update(cx, |_, window, _| window.bounds())
                .expect("failed to read window bounds");
            let region = format!(
                "-R{},{},{},{}",
                f32::from(bounds.origin.x) + MARGIN,
                f32::from(bounds.origin.y) + MARGIN,
                f32::from(bounds.size.width) - 2. * MARGIN,
                f32::from(bounds.size.height) - 2. * MARGIN,
            );
            let status = Command::new("screencapture")
                .args(["-x", &region, &out])
                .status()
                .expect("failed to run screencapture");
            if status.success() {
                println!("{out}");
            } else {
                eprintln!("screencapture failed (screen-recording permission?)");
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
