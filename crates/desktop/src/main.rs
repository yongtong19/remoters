#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_component::{Root, TitleBar};
pub(crate) use remoters_core::Button;

mod account;
mod account_ui;
mod app;
mod audio;
mod clipboard;
mod codec;
mod gamepad;
mod i18n;
mod metrics;
mod platform;
mod privacy;
mod sharing;
mod ui;
mod viewer;

pub(crate) use app::{ActiveTab, Desktop, MenuDrag, Role, SessionTab, SessionTabStatus, Viewer};

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--virtual-display-worker"))
    {
        if let Err(error) = platform::display::virtual_display_worker() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    let startup = std::rc::Rc::new(app::StartupOptions::from_args());
    Application::new()
        .with_assets(app::DesktopAssets)
        .run(move |app: &mut App| {
            gpui_component::init(app);
            ui::configure_theme(app);
            let bounds = Bounds::centered(None, size(px(1110.), px(705.)), app);
            let startup = startup.clone();
            app.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Remoters".into()),
                        traffic_light_position: Some(gpui::point(px(9.), px(14.))),
                        ..TitleBar::title_bar_options()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(740.), px(540.))),
                    ..Default::default()
                },
                move |window, app| {
                    let desktop = app.new(|cx| Desktop::new(window, &startup, cx));
                    app.new(|cx| Root::new(desktop, window, cx))
                },
            )
            .expect("open the Remoters window");
            #[cfg(target_os = "macos")]
            platform::install_drag_handlers();
            app.activate(true);
            app.on_window_closed(|app| {
                if app.windows().is_empty() {
                    app.quit();
                }
            })
            .detach();
        });
}
