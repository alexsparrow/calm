mod anim;
mod color;
mod energy;
mod layout;
mod model;
mod spec;
mod sync;
mod text;
mod theme;
mod zones;
mod view;
mod viewconf;

use std::borrow::Cow;

use gpui::{App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, px, size};

const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
];

fn main() {
    // `calm config check|dump|path` runs without opening a window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("config") {
        std::process::exit(viewconf::cli(&args[1..]));
    }
    let mut opts = view::Options::from_args();
    // calm.kdl's `window` sets translucency unless the command line already did.
    let (config, _) = viewconf::load();
    if opts.opacity.is_none() {
        opts.opacity = config.settings.opacity;
        opts.blur |= config.settings.blur;
    }
    Application::new().run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
            .expect("bundled fonts load");
        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_background: match (opts.opacity, opts.blur) {
                    (None, _) => WindowBackgroundAppearance::Opaque,
                    (Some(_), true) => WindowBackgroundAppearance::Blurred,
                    (Some(_), false) => WindowBackgroundAppearance::Transparent,
                },
                titlebar: Some(TitlebarOptions { title: Some("calm".into()), ..Default::default() }),
                app_id: Some("calm".into()),
                focus: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| view::CalmView::new(opts, window, cx)),
        )
        .unwrap();
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
