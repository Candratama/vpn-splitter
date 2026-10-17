use gpui::{
    App, AppContext, Bounds, Context, Global, MenuItem, NoAction, Render, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_tray::{Icon, Tray};

actions!(
    vpn_splitter,
    [
        OpenSettings,
        HideSettings,
        Connect,
        Disconnect,
        Refresh,
        RetryCleanup,
        InstallHelper,
        ReloadProfiles,
        Quit
    ]
);

struct Desktop {
    tray: Tray,
    settings: Option<WindowHandle<PrototypeSettings>>,
    view: crate::controller::View,
    commands: std::sync::mpsc::Sender<crate::controller::Command>,
}

impl Global for Desktop {}

pub fn install(cx: &mut App) -> Result<Tray, Box<dyn std::error::Error>> {
    // Global actions can run while GPUI temporarily borrows the active window.
    // Defer handle updates until that dispatch finishes.
    cx.on_action(|_: &OpenSettings, cx| cx.defer(open_settings))
        .on_action(|_: &HideSettings, cx| cx.defer(hide_settings))
        .on_action(|_: &Connect, cx| cx.defer(|cx| send(cx, crate::controller::Command::Connect)))
        .on_action(|_: &Disconnect, cx| {
            cx.defer(|cx| send(cx, crate::controller::Command::Disconnect))
        })
        .on_action(|_: &Refresh, cx| cx.defer(|cx| send(cx, crate::controller::Command::Refresh)))
        .on_action(|_: &RetryCleanup, cx| {
            cx.defer(|cx| send(cx, crate::controller::Command::RetryCleanup))
        })
        .on_action(|_: &InstallHelper, cx| {
            cx.defer(|cx| send(cx, crate::controller::Command::InstallHelper))
        })
        .on_action(|_: &ReloadProfiles, cx| {
            cx.defer(|cx| send(cx, crate::controller::Command::ReloadProfiles))
        })
        .on_action(|_: &Quit, cx| cx.defer(quit));
    let tray = Tray::builder()
        .icon(icon()?)
        .title("VPN")
        .tooltip("VPN Splitter")
        .menu(menu)
        .build(cx)?;
    let (commands, updates) = crate::controller::start(crate::application_directory()?);
    cx.set_global(Desktop {
        tray: tray.clone(),
        settings: None,
        view: crate::controller::View::default(),
        commands,
    });
    cx.spawn(async move |cx| {
        while let Ok(view) = updates.recv().await {
            cx.update(|cx| {
                let quit_ready = view.quit_ready;
                cx.global_mut::<Desktop>().view = view;
                refresh_menu(cx);
                cx.refresh_windows();
                if quit_ready {
                    finish_quit(cx);
                }
            });
        }
    })
    .detach();
    cx.on_window_closed(|cx, _| {
        cx.global_mut::<Desktop>().settings = None;
        let tray = cx.global::<Desktop>().tray.clone();
        if let Err(error) = tray.refresh_menu(cx) {
            eprintln!("Cannot refresh tray: {error}");
        }
    })
    .detach();
    Ok(tray)
}

pub fn view(cx: &App) -> &crate::controller::View {
    &cx.global::<Desktop>().view
}
pub fn send(cx: &mut App, command: crate::controller::Command) {
    if cx.global::<Desktop>().commands.send(command).is_err() {
        cx.global_mut::<Desktop>().view.error =
            Some(vpn_splitter_core::protocol::FaultCode::Transport);
    }
    refresh_menu(cx);
}
pub fn menu(cx: &mut App) -> Vec<MenuItem> {
    use vpn_splitter_core::session::SessionState;
    let view = view(cx);
    let disconnected = view.state == SessionState::Disconnected;
    let status = if view.preparing {
        "Preparing VPN…"
    } else {
        match view.state {
            SessionState::Disconnected => "Disconnected",
            SessionState::Connecting => "Connecting…",
            SessionState::Applying => "VPN connected — applying routes…",
            SessionState::Active => "VPN connected — routing active",
            SessionState::Disconnecting => "Clearing routes / disconnecting…",
            SessionState::CleanupRequired => "Cleanup required",
        }
    };
    let mut items = vec![MenuItem::action(status, NoAction).disabled(true)];
    if disconnected {
        items.push(
            MenuItem::action("Connect VPN", Connect)
                .disabled(view.preparing || view.config.profile_id.is_none()),
        );
    } else {
        items.push(MenuItem::action(
            if view.adopted {
                "Clear routes (keep VPN connected)"
            } else {
                "Disconnect VPN"
            },
            Disconnect,
        ));
    }
    items.push(
        MenuItem::action("Refresh routes", Refresh).disabled(view.state != SessionState::Active),
    );
    items.push(
        MenuItem::action("Retry cleanup", RetryCleanup)
            .disabled(view.state != SessionState::CleanupRequired),
    );
    let adopted = view.adopted;
    items.push(MenuItem::separator());
    items
        .push(MenuItem::action("Profiles / Settings…", OpenSettings).checked(settings_visible(cx)));
    items.push(MenuItem::action("Reload VPN profiles", ReloadProfiles));
    items
        .push(MenuItem::action("Install / approve helper…", InstallHelper).disabled(!disconnected));
    items.push(MenuItem::separator());
    items.push(MenuItem::action(
        if adopted && !disconnected {
            "Quit (keep VPN connected)"
        } else {
            "Quit VPN Splitter"
        },
        Quit,
    ));
    items
}

fn open_settings(cx: &mut App) {
    let started = std::time::Instant::now();
    if let Some(handle) = cx.global::<Desktop>().settings
        && handle
            .update(cx, |_, window, _| {
                window.on_next_frame(move |_, _| {
                    eprintln!(
                        "settings_reveal_frame_ms={:.3}",
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                });
                window.activate_window();
            })
            .is_ok()
    {
        cx.activate(true);
        refresh_menu(cx);
        return;
    }
    let bounds = Bounds::centered(None, size(px(460.0), px(260.0)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("VPN Splitter".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            window.on_window_should_close(cx, |_, cx| {
                cx.defer(hide_settings);
                false
            });
            window.on_next_frame(move |_, _| {
                eprintln!(
                    "settings_frame_ms={:.3}",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            });
            cx.new(|_| PrototypeSettings)
        },
    );
    match result {
        Ok(handle) => {
            cx.global_mut::<Desktop>().settings = Some(handle);
            cx.activate(true);
            refresh_menu(cx);
        }
        Err(error) => eprintln!("Cannot open Settings: {error}"),
    }
}

fn hide_settings(cx: &mut App) {
    if let Some(handle) = cx.global::<Desktop>().settings {
        match handle.update(cx, |_, window, _| crate::window_visibility::hide(window)) {
            Ok(Ok(())) => refresh_menu(cx),
            Ok(Err(error)) => eprintln!("Cannot hide Settings: {error}"),
            Err(error) => eprintln!("Cannot access Settings: {error}"),
        }
    }
}

pub fn settings_visible(cx: &mut App) -> bool {
    let Some(handle) = cx.try_global::<Desktop>().and_then(|state| state.settings) else {
        return false;
    };
    match handle.update(cx, |_, window, _| {
        crate::window_visibility::is_visible(window)
    }) {
        Ok(Ok(visible)) => visible,
        Ok(Err(error)) => {
            eprintln!("Cannot read Settings visibility: {error}");
            false
        }
        Err(_) => false,
    }
}

fn refresh_menu(cx: &mut App) {
    let tray = cx.global::<Desktop>().tray.clone();
    if let Err(error) = tray.refresh_menu(cx) {
        eprintln!("Cannot refresh tray: {error}");
    }
}

fn quit(cx: &mut App) {
    send(cx, crate::controller::Command::Quit);
}
fn finish_quit(cx: &mut App) {
    let tray = cx.global::<Desktop>().tray.clone();
    if let Err(error) = tray.close(cx) {
        eprintln!("Cannot close tray: {error}");
    }
    cx.quit();
}

struct PrototypeSettings;

impl Render for PrototypeSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0xf5f5f3))
            .text_color(rgb(0x202421))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_xl().child("VPN Splitter"))
            .child(format!("VPN: {:?}",view(cx).state))
            .child(view(cx).error.map(crate::controller::message).unwrap_or("Select a profile and import destinations in Settings (editor integration follows)."))
            .child("Close this window to keep the app in the menu bar. Use the tray menu to quit.")
    }
}

fn icon() -> gpui_tray::Result<Icon> {
    const SIZE: u32 = 32;
    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    for y in 5..27 {
        for x in 7..25 {
            let border = y == 5 || x == 7 || x == 24 || y == 26;
            let check = (10..16).contains(&x) && y == x + 7 || (15..23).contains(&x) && y == 37 - x;
            if border || check {
                let offset = ((y * SIZE + x) * 4) as usize;
                rgba[offset..offset + 4].copy_from_slice(&[45, 90, 70, 255]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE)
}
