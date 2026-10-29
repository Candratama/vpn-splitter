use gpui::{
    App, AppContext, Bounds, Global, MenuItem, NoAction, WindowBounds, WindowHandle, WindowOptions,
    actions, point, px, size,
};
use gpui_tray::{Icon, Tray};
use std::time::Duration;

const SPINNER_FRAMES: usize = 12;
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
const TRAY_ICON_SIZE: usize = 18;

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
    tray_animating: bool,
    tray_animation_generation: u64,
}

impl Global for Desktop {}

pub fn install(cx: &mut App) -> Result<Tray, Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWorkspace;
        cx.set_reduce_motion(
            NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion(),
        );
    }
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
        tray_animating: false,
        tray_animation_generation: 0,
    });
    cx.spawn(async move |cx| {
        while let Ok(view) = updates.recv().await {
            cx.update(|cx| {
                let quit_ready = view.quit_ready;
                cx.global_mut::<Desktop>().view = view;
                sync_tray_icon(cx);
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
    open_page(cx, crate::settings::Page::Vpn);
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
    let bounds = Bounds::centered(None, size(px(820.0), px(680.0)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.0), px(520.0))),
            titlebar: Some(gpui::TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.0), px(16.0))),
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

fn sync_tray_icon(cx: &mut App) {
    use vpn_splitter_core::session::SessionState;

    let animate = {
        let view = &cx.global::<Desktop>().view;
        !cx.reduce_motion()
            && (view.preparing
                || matches!(
                    view.state,
                    SessionState::Connecting | SessionState::Applying
                ))
    };
    let generation = {
        let desktop = cx.global_mut::<Desktop>();
        if desktop.tray_animating == animate {
            return;
        }
        desktop.tray_animating = animate;
        desktop.tray_animation_generation = desktop.tray_animation_generation.wrapping_add(1);
        desktop.tray_animation_generation
    };

    if !animate {
        set_tray_icon(cx, icon());
        return;
    }

    set_tray_icon(cx, spinner_icon(0));
    cx.spawn(async move |cx| {
        let mut frame = 1;
        loop {
            cx.background_executor().timer(SPINNER_INTERVAL).await;
            let should_continue = cx.update(|cx| {
                let desktop = cx.global::<Desktop>();
                if !desktop.tray_animating || desktop.tray_animation_generation != generation {
                    return false;
                }
                set_tray_icon(cx, spinner_icon(frame));
                true
            });
            if !should_continue {
                break;
            }
            frame = (frame + 1) % SPINNER_FRAMES;
        }
    })
    .detach();
}

fn set_tray_icon(cx: &mut App, icon: gpui_tray::Result<Icon>) {
    let tray = cx.global::<Desktop>().tray.clone();
    match icon.and_then(|icon| tray.set_icon(Some(icon), cx)) {
        Ok(()) => {}
        Err(error) => eprintln!("Cannot update tray icon: {error}"),
    }
}

fn spinner_icon(frame: usize) -> gpui_tray::Result<Icon> {
    let mut rgba = vec![0; TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4];
    let center = (TRAY_ICON_SIZE as f64 - 1.0) / 2.0;
    for y in 0..TRAY_ICON_SIZE {
        for x in 0..TRAY_ICON_SIZE {
            let dx = x as f64 - center;
            let dy = y as f64 - center;
            let radius = dx.hypot(dy);
            if !(3.0..=7.4).contains(&radius) {
                continue;
            }
            let angle = dy.atan2(dx).rem_euclid(std::f64::consts::TAU);
            let spoke = (0..SPINNER_FRAMES).find(|spoke| {
                let spoke_angle = std::f64::consts::TAU * *spoke as f64 / SPINNER_FRAMES as f64;
                let difference = (angle - spoke_angle).abs();
                difference.min(std::f64::consts::TAU - difference) <= 0.22
            });
            let Some(spoke) = spoke else { continue };
            let distance = (spoke + SPINNER_FRAMES - frame % SPINNER_FRAMES) % SPINNER_FRAMES;
            let brightness = 48 + (207 * (SPINNER_FRAMES - distance) / SPINNER_FRAMES) as u8;
            let edge = (radius - 3.0).min(7.4 - radius).clamp(0.0, 1.0);
            let alpha = (brightness as f64 * edge).round() as u8;
            let pixel = (y * TRAY_ICON_SIZE + x) * 4;
            rgba[pixel..pixel + 3].fill(255);
            rgba[pixel + 3] = alpha;
        }
    }
    Icon::from_rgba(rgba, TRAY_ICON_SIZE as u32, TRAY_ICON_SIZE as u32)
}

fn quit(cx: &mut App) {
    send(cx, crate::controller::Command::Quit);
}
fn finish_quit(cx: &mut App) {
    #[cfg(target_os = "macos")]
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
    Icon::from_rgba(
        include_bytes!("../assets/menubar-icon.rgba").to_vec(),
        TRAY_ICON_SIZE as u32,
        TRAY_ICON_SIZE as u32,
    )
}
