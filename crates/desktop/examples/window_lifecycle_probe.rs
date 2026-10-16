//! Exercise the production Settings lifecycle without accessibility clients
//! retaining references to old native windows. Never changes VPN or routing.
#[path = "../src/tray.rs"]
mod tray;

use gpui::QuitMode;
use std::time::Duration;

fn main() {
    gpui_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx| {
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
                for _ in 0..20 {
                    cx.update(|cx| cx.dispatch_action(&tray::OpenSettings));
                    executor.timer(Duration::from_millis(250)).await;
                    cx.update(|cx| {
                        let windows = cx.windows();
                        assert_eq!(windows.len(), 1);
                        windows[0]
                            .update(cx, |_, window, _| window.remove_window())
                            .unwrap();
                    });
                    executor.timer(Duration::from_millis(250)).await;
                    cx.update(|cx| assert!(cx.windows().is_empty()));
                }
                eprintln!("lifecycle_probe_complete: windows=0 cycles=20");
                executor.timer(Duration::from_secs(60)).await;
                cx.update(|cx| cx.dispatch_action(&tray::Quit));
            })
            .detach();
        });
}
