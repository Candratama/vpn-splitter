use gpui::{
    App, AppContext, Bounds, Context, Global, MenuItem, NoAction, Render, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_tray::{Icon, Tray};

actions!(vpn_splitter, [OpenSettings, HideSettings, Quit]);

struct Desktop {
    tray: Tray,
    settings: Option<WindowHandle<PrototypeSettings>>,
}

impl Global for Desktop {}

pub fn install(cx: &mut App) -> Result<Tray, Box<dyn std::error::Error>> {
    // Global actions can run while GPUI temporarily borrows the active window.
    // Defer handle updates until that dispatch finishes.
    cx.on_action(|_: &OpenSettings, cx| cx.defer(open_settings))
        .on_action(|_: &HideSettings, cx| cx.defer(hide_settings))
        .on_action(|_: &Quit, cx| cx.defer(quit));
    let tray = Tray::builder()
        .icon(icon()?)
        .title("VPN")
        .tooltip("VPN Splitter — prototype, routing unavailable")
        .menu(menu)
        .build(cx)?;
    cx.set_global(Desktop {
        tray: tray.clone(),
        settings: None,
    });
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

pub fn menu(cx: &mut App) -> Vec<MenuItem> {
    vec![
        MenuItem::action("Disconnected — prototype", NoAction).disabled(true),
        MenuItem::action("Connect VPN (not implemented)", NoAction).disabled(true),
        MenuItem::separator(),
        MenuItem::action("Settings…", OpenSettings).checked(settings_visible(cx)),
        MenuItem::separator(),
        MenuItem::action("Quit VPN Splitter", Quit),
    ]
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
    let tray = cx.global::<Desktop>().tray.clone();
    if let Err(error) = tray.close(cx) {
        eprintln!("Cannot close tray: {error}");
    }
    cx.quit();
}

struct PrototypeSettings;

impl Render for PrototypeSettings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0xf5f5f3))
            .text_color(rgb(0x202421))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_xl().child("VPN Splitter"))
            .child("Desktop prototype")
            .child("VPN connections and routing are not enabled yet.")
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
