//! Exercise the production Settings lifecycle without accessibility clients
//! retaining references to old native windows. Never changes VPN or routing.
#[path = "../src/tray.rs"]
mod tray;
#[path = "../src/window_visibility.rs"]
mod window_visibility;

use gpui::{App, MenuItem, NoAction, QuitMode};
use std::time::Duration;

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let hold_seconds = match arguments.as_slice() {
        [] => 330,
        [flag, seconds] if flag == "--hold-seconds" => {
            seconds.parse::<u64>().expect("hold seconds")
        }
        _ => panic!("Usage: window_lifecycle_probe [--hold-seconds N]"),
    };
    gpui_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx| {
            #[cfg(target_os = "macos")]
            {
                use objc2::MainThreadMarker;
                use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap())
                    .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            }
            tray::install(cx).expect("prototype tray installation");
            let executor = cx.background_executor().clone();
            cx.spawn(async move |cx| {
                let mut identity = None;
                for _ in 0..20 {
                    cx.update(|cx| cx.dispatch_action(&tray::OpenSettings));
                    executor.timer(Duration::from_millis(250)).await;
                    cx.update(|cx| cx.dispatch_action(&tray::OpenSettings));
                    executor.timer(Duration::from_millis(50)).await;
                    cx.update(|cx| {
                        assert!(
                            tray::settings_visible(cx),
                            "Settings must be natively visible"
                        );
                        assert_menu_state(cx, true);
                        let windows = cx.windows();
                        assert_eq!(windows.len(), 1);
                        let id = windows[0].window_id();
                        assert_eq!(
                            *identity.get_or_insert(id),
                            id,
                            "Settings must reuse the same window"
                        );
                        cx.dispatch_action(&tray::HideSettings);
                    });
                    executor.timer(Duration::from_millis(250)).await;
                    cx.update(|cx| {
                        assert!(
                            !tray::settings_visible(cx),
                            "Settings must be natively hidden"
                        );
                        assert_menu_state(cx, false);
                        assert_eq!(
                            cx.windows().len(),
                            1,
                            "Closing Settings must retain its window"
                        )
                    });
                }
                eprintln!("lifecycle_probe_complete: windows=1 cycles=20 hidden=true");
                executor.timer(Duration::from_secs(hold_seconds)).await;
                cx.update(|cx| cx.dispatch_action(&tray::Quit));
            })
            .detach();
        });
}

fn assert_menu_state(cx: &mut App, visible: bool) {
    let items = tray::menu(cx);
    let settings = items.iter().find(|item| {
        matches!(item, MenuItem::Action { action, .. } if action.as_any().is::<tray::OpenSettings>())
    }).expect("Settings action");
    assert_eq!(settings.is_checked(), visible);
    for item in &items {
        if matches!(item, MenuItem::Action { action, .. } if action.as_any().is::<NoAction>()) {
            assert!(
                item.is_disabled(),
                "Unimplemented actions must stay disabled"
            );
        }
    }
}
