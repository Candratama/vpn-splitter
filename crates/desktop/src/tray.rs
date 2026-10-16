use gpui::{
    App, AppContext, Bounds, Context, Global, MenuItem, NoAction, Render, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_tray::{Icon, Tray};

actions!(vpn_splitter, [OpenSettings, Quit]);

struct Desktop {
    tray: Tray,
    settings: Option<WindowHandle<PrototypeSettings>>,
}

impl Global for Desktop {}

pub fn install(cx: &mut App) -> Result<Tray, Box<dyn std::error::Error>> {
    cx.on_action(open_settings).on_action(quit);
    let tray = Tray::builder()
        .icon(icon()?)
        .title("VPN")
        .tooltip("VPN Splitter — prototype, routing unavailable")
        .menu(|cx| {
            vec![
                MenuItem::action("Disconnected — prototype", NoAction).disabled(true),
                MenuItem::action("Connect VPN (not implemented)", NoAction).disabled(true),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings).checked(!cx.windows().is_empty()),
                MenuItem::separator(),
                MenuItem::action("Quit VPN Splitter", Quit),
            ]
        })
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

fn open_settings(_: &OpenSettings, cx: &mut App) {
    let started = std::time::Instant::now();
    if let Some(handle) = cx.global::<Desktop>().settings
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
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
        }
        Err(error) => eprintln!("Cannot open Settings: {error}"),
    }
}

fn quit(_: &Quit, cx: &mut App) {
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
