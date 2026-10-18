mod controller;
mod diagnostics;
mod focus;
mod platform;
mod settings;
mod single_instance;
mod text_input;
mod tray;
mod window_visibility;

use gpui::QuitMode;
use single_instance::SingleInstance;
use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let started = std::time::Instant::now();
    let directory = application_directory()?;
    std::fs::create_dir_all(&directory)?;
    let Some(_instance) = SingleInstance::acquire(&directory.join("desktop.lock"))? else {
        focus::open_existing(&directory)?;
        return Ok(());
    };
    let (focus_tx, focus_rx) = async_channel::bounded(1);
    let _focus_server = focus::serve(&directory, focus_tx)?;
    let application = gpui_platform::application().with_quit_mode(QuitMode::Explicit);
    application.on_reopen(|cx| {
        cx.spawn(async |cx| {
            cx.update(|cx| cx.dispatch_action(&tray::OpenSettings));
        })
        .detach();
    });
    application.run(move |cx| {
        cx.spawn(async move |cx| {
            while focus_rx.recv().await.is_ok() {
                cx.update(|cx| cx.dispatch_action(&tray::OpenSettings));
            }
        })
        .detach();
        #[cfg(target_os = "macos")]
        {
            use objc2::MainThreadMarker;
            use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
            let application = NSApplication::sharedApplication(
                MainThreadMarker::new().expect("GPUI startup must run on the main thread"),
            );
            if !application.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
                eprintln!("Cannot configure menu-bar-only activation policy.");
                cx.quit();
                return;
            }
        }
        if let Err(error) = tray::install(cx) {
            eprintln!("Cannot create VPN Splitter tray: {error}");
            cx.quit();
        } else {
            eprintln!(
                "tray_ready_ms={:.3}",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    });
    Ok(())
}

fn application_directory() -> Result<PathBuf, Box<dyn Error>> {
    #[cfg(target_os = "macos")]
    let directory = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unavailable")?)
        .join("Library/Application Support/VPN Splitter");
    #[cfg(target_os = "windows")]
    let directory =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?)
            .join("VPN Splitter");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("This desktop prototype supports macOS and Windows only".into());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Ok(directory)
}
