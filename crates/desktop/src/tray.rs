use gpui::{
    App, AppContext, Bounds, Global, MenuItem, NoAction, WindowBounds, WindowHandle, WindowOptions,
    actions, px, size,
};
use gpui_tray::{Icon, Tray};

actions!(
    vpn_splitter,
    [
        OpenSettings,
        OpenDiagnostics,
        HideSettings,
        Connect,
        Disconnect,
        Refresh,
        RetryCleanup,
        InstallHelper,
        RemoveHelper,
        ReloadProfiles,
        Quit
    ]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(no_json)]
struct SelectProfile {
    id: String,
}

struct Desktop {
    tray: Tray,
    settings: Option<WindowHandle<crate::settings::Settings>>,
    view: crate::controller::View,
    commands: std::sync::mpsc::Sender<crate::controller::Command>,
}

impl Global for Desktop {}

pub fn install(cx: &mut App) -> Result<Tray, Box<dyn std::error::Error>> {
    // Global actions can run while GPUI temporarily borrows the active window.
    // Defer handle updates until that dispatch finishes.
    cx.on_action(|action: &SelectProfile, cx| {
        let id = action.id.clone();
        cx.defer(move |cx| {
            let view = view(cx);
            if view.state == vpn_splitter_core::session::SessionState::Disconnected
                && !view.preparing
            {
                let mut config = view.config.clone();
                config.profile_id = Some(id);
                send(cx, crate::controller::Command::Save(config));
            }
        });
    });
    cx.on_action(|_: &OpenSettings, cx| cx.defer(open_settings))
        .on_action(|_: &OpenDiagnostics, cx| cx.defer(open_diagnostics))
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
        .on_action(|_: &RemoveHelper, cx| {
            cx.defer(|cx| send(cx, crate::controller::Command::RemoveHelper))
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
    crate::settings::bind(cx);
    cx.bind_keys([gpui::KeyBinding::new("cmd-q", Quit, None)]);
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
                if let Some(handle) = cx.global::<Desktop>().settings {
                    let _ = handle.update(cx, |_, _, cx| cx.notify());
                }
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
    // The tray builder requests its first menu before Desktop can own the tray.
    if !cx.has_global::<Desktop>() {
        return vec![MenuItem::action("Starting VPN Splitter…", NoAction).disabled(true)];
    }
    use vpn_splitter_core::session::SessionState;
    let settings_open = settings_visible(cx);
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
        items.push(MenuItem::action("Connect VPN", Connect).disabled(
            view.preparing
                || view.helper_busy
                || !view.config_ready
                || view.config.profile_id.is_none(),
        ));
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
    items.push(MenuItem::action("Retry cleanup", RetryCleanup).disabled(
        view.helper_busy
            || (view.state != SessionState::CleanupRequired
                && view.error != Some(vpn_splitter_core::protocol::FaultCode::Cleanup)),
    ));
    items.push(MenuItem::submenu(gpui::Menu {
        name: "VPN profile".into(),
        disabled: false,
        items: view
            .profiles
            .iter()
            .map(|profile| {
                MenuItem::action(
                    profile.name.clone(),
                    SelectProfile {
                        id: profile.id.as_str().to_owned(),
                    },
                )
                .checked(view.config.profile_id.as_deref() == Some(profile.id.as_str()))
                .disabled(
                    !disconnected || view.preparing || !profile.supported || !profile.split_enabled,
                )
            })
            .collect(),
    }));
    let adopted = view.adopted;
    items.push(MenuItem::separator());
    items.push(MenuItem::action("Profiles / Settings…", OpenSettings).checked(settings_open));
    items.push(MenuItem::action("Diagnostics…", OpenDiagnostics));
    items.push(MenuItem::action("Reload VPN profiles", ReloadProfiles));
    items.push(
        MenuItem::action("Install / approve helper…", InstallHelper)
            .disabled(!disconnected || view.preparing || view.helper_busy),
    );
    items.push(
        MenuItem::action("Remove helper…", RemoveHelper).disabled(
            !disconnected || view.preparing || view.helper_busy || view.helper_status != 1,
        ),
    );
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

pub fn open_settings(cx: &mut App) {
    open_page(cx, crate::settings::Page::Settings);
}
pub fn open_diagnostics(cx: &mut App) {
    open_page(cx, crate::settings::Page::Diagnostics);
}
fn open_page(cx: &mut App, page: crate::settings::Page) {
    let started = std::time::Instant::now();
    if let Some(handle) = cx.global::<Desktop>().settings
        && handle
            .update(cx, |view, window, cx| {
                view.page = page;
                cx.notify();
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
    let bounds = Bounds::centered(None, size(px(600.0), px(700.0)), cx);
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
            cx.new(|cx| {
                let mut view = crate::settings::Settings::new(cx);
                view.page = page;
                view
            })
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
    if let Err(error) = crate::platform::macos::relaunch_local_if_requested() {
        eprintln!("Cannot reopen installed app: {error}");
    }
    let tray = cx.global::<Desktop>().tray.clone();
    if let Err(error) = tray.close(cx) {
        eprintln!("Cannot close tray: {error}");
    }
    cx.quit();
}

fn icon() -> gpui_tray::Result<Icon> {
    Icon::from_rgba(include_bytes!("../assets/menubar-icon.rgba").to_vec(), 18, 18)
}
