use crate::{
    controller::{self, Command},
    text_input::{Edited, TextInput},
    tray,
};
use gpui::{div, prelude::*, *};
use vpn_splitter_core::{
    config::{Config, MAX_CONFIG_BYTES, import_rules},
    session::SessionState,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Vpn,
    Routing,
    General,
    Helper,
    Diagnostics,
}
impl Page {
    const ALL: [Self; 5] = [
        Self::Vpn,
        Self::Routing,
        Self::General,
        Self::Helper,
        Self::Diagnostics,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Vpn => "VPN",
            Self::Routing => "Routing rules",
            Self::General => "General",
            Self::Helper => "Helper",
            Self::Diagnostics => "Diagnostics",
        }
    }

    fn icon_bytes(self) -> &'static [u8] {
        match self {
            Self::Vpn => include_bytes!("../assets/section-vpn.svg"),
            Self::Routing => include_bytes!("../assets/section-routing.svg"),
            Self::General => include_bytes!("../assets/section-general.svg"),
            Self::Helper => include_bytes!("../assets/section-helper.svg"),
            Self::Diagnostics => include_bytes!("../assets/section-diagnostics.svg"),
        }
    }
}
actions!(settings, [SaveSettings, NextField, PreviousField]);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Destructive,
}
pub fn bind(cx: &mut App) {
    crate::text_input::bind(cx);
    cx.bind_keys([
        KeyBinding::new("cmd-enter", SaveSettings, Some("Settings")),
        KeyBinding::new("ctrl-enter", SaveSettings, Some("Settings")),
        KeyBinding::new("tab", NextField, Some("Settings")),
        KeyBinding::new("shift-tab", PreviousField, Some("Settings")),
    ]);
}
fn connection_action_disabled(live: &controller::View, dirty: bool, awaiting_save: bool) -> bool {
    live.preparing
        || live.helper_busy
        || live.state == SessionState::CleanupRequired
        || (live.state == SessionState::Disconnected
            && (!live.config_ready || live.config.profile_id.is_none() || dirty || awaiting_save))
}
pub struct Settings {
    pub page: Page,
    focus_handle: FocusHandle,
    editor: Entity<TextInput>,
    draft: Config,
    revision: u64,
    dirty: bool,
    awaiting_save: bool,
    file_busy: bool,
    error: Option<String>,
    notice: Option<String>,
}
impl Settings {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = tray::view(cx);
        let draft = view.config.clone();
        let revision = view.config_revision;
        let content = draft
            .destinations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let editor = cx.new(|cx| TextInput::new(content, cx));
        cx.subscribe(&editor, |this, _, _: &Edited, cx| {
            this.dirty = true;
            this.error = None;
            this.notice = None;
            cx.notify();
        })
        .detach();
        Self {
            page: Page::Vpn,
            focus_handle: cx.focus_handle(),
            editor,
            draft,
            revision,
            dirty: false,
            awaiting_save: false,
            file_busy: false,
            error: None,
            notice: None,
        }
    }
    fn save(&mut self, _: &SaveSettings, _: &mut Window, cx: &mut Context<Self>) {
        match import_rules(&self.editor.read(cx).content()) {
            Ok(rules) => {
                self.draft.destinations = rules;
                self.error = None;
                self.awaiting_save = true;
                tray::send(cx, Command::Save(self.draft.clone()));
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn import(&mut self, cx: &mut Context<Self>) {
        if self.file_busy {
            return;
        }
        self.file_busy = true;
        self.error = None;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import VPN rules or JSON config".into()),
        });
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = match paths.await {
                Ok(Ok(Some(paths))) if !paths.is_empty() => {
                    let path = paths[0].clone();
                    Some(
                        executor
                            .spawn(async move {
                                use std::io::Read;
                                let file = std::fs::File::open(path)
                                    .map_err(|_| "Cannot open the selected file.".to_owned())?;
                                let mut bytes = vec![];
                                file.take((MAX_CONFIG_BYTES + 1) as u64)
                                    .read_to_end(&mut bytes)
                                    .map_err(|_| "Cannot read the selected file.".to_owned())?;
                                if bytes.len() > MAX_CONFIG_BYTES {
                                    return Err("File exceeds 1 MiB.".into());
                                }
                                let text = String::from_utf8(bytes)
                                    .map_err(|_| "Expected a UTF-8 text file.".to_owned())?;
                                if text.trim_start().starts_with('{') {
                                    Config::parse(&text)
                                        .map(|config| {
                                            (
                                                Some(config.clone()),
                                                config
                                                    .destinations
                                                    .iter()
                                                    .map(ToString::to_string)
                                                    .collect::<Vec<_>>()
                                                    .join("\n"),
                                            )
                                        })
                                        .map_err(|e| e.to_string())
                                } else {
                                    import_rules(&text)
                                        .map(|rules| {
                                            (
                                                None,
                                                rules
                                                    .iter()
                                                    .map(ToString::to_string)
                                                    .collect::<Vec<_>>()
                                                    .join("\n"),
                                            )
                                        })
                                        .map_err(|e| e.to_string())
                                }
                            })
                            .await,
                    )
                }
                Ok(Ok(None)) => None,
                _ => Some(Err("File selection failed.".into())),
            };
            let _ = this.update(cx, |this, cx| {
                this.file_busy = false;
                if let Some(result) = result {
                    match result {
                        Ok((config, text)) => {
                            if let Some(config) = config {
                                this.draft = config;
                            }
                            this.editor
                                .update(cx, |editor, cx| editor.set_content(text, cx));
                            this.dirty = true;
                            this.notice =
                                Some("Imported locally. Review the rules, then Save.".into());
                        }
                        Err(error) => this.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn export(&mut self, cx: &mut Context<Self>) {
        if self.file_busy {
            return;
        }
        let rules = match import_rules(&self.editor.read(cx).content()) {
            Ok(rules) => rules,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let mut config = self.draft.clone();
        config.destinations = rules;
        let path = cx.prompt_for_new_path(&std::env::temp_dir(), Some("vpn-splitter-config.json"));
        self.file_busy = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = match path.await {
                Ok(Ok(Some(path))) => Some(
                    executor
                        .spawn(async move {
                            vpn_splitter_core::config::save_config(&path, &config).map_err(|_| {
                                "Cannot save the export. Reload the file before retrying."
                                    .to_owned()
                            })
                        })
                        .await,
                ),
                Ok(Ok(None)) => None,
                _ => Some(Err("File selection failed.".to_owned())),
            };
            let _ =
                this.update(cx, |this, cx| {
                    this.file_busy = false;
                    if let Some(result) = result {
                        match result {
                            Ok(()) => this.notice = Some(
                                "Exported configuration contains private infrastructure details."
                                    .into(),
                            ),
                            Err(error) => this.error = Some(error),
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }
    fn next(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
    }
    fn previous(&mut self, _: &PreviousField, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
    }
}

impl Focusable for Settings {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
    callback: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> Stateful<Div> {
    styled_button(id, label, disabled, ButtonVariant::Secondary, callback)
}

#[allow(dead_code)]
pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
    callback: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> Stateful<Div> {
    styled_button(id, label, disabled, ButtonVariant::Primary, callback)
}

pub fn styled_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
    variant: ButtonVariant,
    callback: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> Stateful<Div> {
    let label = label.into();
    let click = callback.clone();
    let key = callback.clone();
    div()
        .id(id)
        .role(accesskit::Role::Button)
        .aria_label(label.clone())
        .when(!disabled, |d| d.focusable())
        .h(px(32.))
        .flex_shrink_0()
        .px_3()
        .rounded_md()
        .border_1()
        .cursor_pointer()
        .text_size(px(13.))
        .flex()
        .items_center()
        .justify_center()
        .when(variant == ButtonVariant::Primary, |d| {
            d.bg(rgb(0x0866d9))
                .text_color(rgb(0xffffff))
                .border_color(rgb(0x0866d9))
                .font_weight(FontWeight::MEDIUM)
                .hover(|s| s.bg(rgb(0x075bbe)))
                .active(|s| s.bg(rgb(0x064e9e)))
                .focus_visible(|s| s.border_color(rgb(0xffffff)))
        })
        .when(variant == ButtonVariant::Secondary, |d| {
            d.border_color(rgba(0x88888833))
                .hover(|s| s.bg(rgba(0x88888818)))
                .active(|s| s.bg(rgba(0x88888828)))
                .focus_visible(|s| s.border_color(rgb(0x0866d9)))
        })
        .when(variant == ButtonVariant::Destructive, |d| {
            d.border_color(rgba(0xd32f2f40))
                .text_color(rgb(0xd32f2f))
                .hover(|s| s.bg(rgba(0xd32f2f12)))
                .active(|s| s.bg(rgba(0xd32f2f24)))
                .focus_visible(|s| s.border_color(rgb(0xd32f2f)))
        })
        .when(disabled, |d| {
            d.opacity(0.4)
                .aria_description("Unavailable in the current state")
        })
        .when(!disabled, |d| {
            d.on_click(move |_, window, cx| click(window, cx))
                .on_key_down(move |event, window, cx| {
                    if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                        key(window, cx);
                        cx.stop_propagation();
                    }
                })
                .on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                    callback(window, cx)
                })
        })
        .child(label)
}
impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }
        let live = tray::view(cx).clone();
        if self.revision != live.config_revision && (!self.dirty || self.awaiting_save) {
            self.revision = live.config_revision;
            self.draft = live.config.clone();
            self.dirty = false;
            self.awaiting_save = false;
            let content = self
                .draft
                .destinations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            self.editor
                .update(cx, |editor, cx| editor.set_content(content, cx));
            self.notice = Some("Saved.".into());
        }
        if live.error.is_some() {
            self.awaiting_save = false;
        }
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let bg = if dark { rgb(0x202020) } else { rgb(0xf5f5f5) };
        let fg = if dark { rgb(0xf0f0f0) } else { rgb(0x242424) };
        let muted = if dark { rgb(0xb7b7b7) } else { rgb(0x606060) };
        let sidebar_bg = if dark { rgb(0x292929) } else { rgb(0xececec) };
        let separator = if dark {
            rgba(0xffffff20)
        } else {
            rgba(0x00000018)
        };
        let entity = cx.entity();
        let busy = live.helper_busy
            || live.preparing
            || !matches!(
                live.state,
                SessionState::Disconnected | SessionState::Active
            )
            || self.file_busy;

        let mut sidebar = div()
            .id("settings-navigation")
            .role(accesskit::Role::Navigation)
            .aria_label("Settings sections")
            .w(px(200.))
            .h_full()
            .flex_shrink_0()
            .overflow_y_scroll()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(separator)
            .pt(px(48.))
            .px_3()
            .pb_3()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .py_2p5()
                    .mb_1()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(
                        svg()
                            .data(include_bytes!("../assets/cloud-server.svg"))
                            .size(px(20.))
                            .text_color(if dark { rgb(0x64b5f6) } else { rgb(0x0866d9) }),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(14.))
                            .child("VPN Splitter"),
                    ),
            );
        for (index, page) in Page::ALL.into_iter().enumerate() {
            let this = entity.clone();
            let selected = self.page == page;
            sidebar =
                sidebar.child(
                    div()
                        .id(("section", index))
                        .role(accesskit::Role::Button)
                        .aria_label(page.title())
                        .aria_selected(selected)
                        .aria_description(if selected {
                            "Current section"
                        } else {
                            "Open section"
                        })
                        .focusable()
                        .h(px(32.))
                        .w_full()
                        .px_2p5()
                        .rounded_lg()
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .bg(if selected { rgb(0x0866d9) } else { sidebar_bg })
                        .text_color(if selected { rgb(0xffffff) } else { fg })
                        .text_size(px(13.))
                        .font_weight(if selected {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .hover(move |style| {
                            style.bg(if selected {
                                rgb(0x075bbe)
                            } else if dark {
                                rgb(0x363636)
                            } else {
                                rgb(0xdedede)
                            })
                        })
                        .active(move |style| {
                            style.bg(if selected {
                                rgb(0x064e9e)
                            } else if dark {
                                rgb(0x303030)
                            } else {
                                rgb(0xd4d4d4)
                            })
                        })
                        .focus_visible(move |style| {
                            style.border_1().border_color(if selected {
                                rgb(0xffffff)
                            } else {
                                rgb(0x0866d9)
                            })
                        })
                        .on_click(move |_, _, cx| {
                            this.update(cx, |this, cx| {
                                this.page = page;
                                cx.notify();
                            });
                        })
                        .on_key_down({
                            let this = entity.clone();
                            move |event, _, cx| {
                                if event.keystroke.key == "enter" || event.keystroke.key == "space"
                                {
                                    this.update(cx, |this, cx| {
                                        this.page = page;
                                        cx.notify();
                                    });
                                    cx.stop_propagation();
                                }
                            }
                        })
                        .on_a11y_action(accesskit::Action::Click, {
                            let this = entity.clone();
                            move |_, _, cx| {
                                this.update(cx, |this, cx| {
                                    this.page = page;
                                    cx.notify();
                                });
                            }
                        })
                        .child(svg().data(page.icon_bytes()).size(px(16.)).text_color(
                            if selected {
                                rgb(0xffffff)
                            } else if dark {
                                rgb(0xaaaaaa)
                            } else {
                                rgb(0x555555)
                            },
                        ))
                        .child(page.title()),
                );
        }

        let mut content = div()
            .id(("section-content", self.page as usize))
            .role(accesskit::Role::Group)
            .aria_label(self.page.title())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pt(px(48.))
            .px_8()
            .pb_6()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.page.title()),
            );

        match self.page {
            Page::Vpn => {
                content = content
                    .child(
                        div()
                            .text_color(muted)
                            .child("Choose a saved macOS L2TP profile for split routing."),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Saved profiles"),
                    );
                let mut profiles = div()
                    .id("profiles")
                    .role(accesskit::Role::Group)
                    .aria_label("Saved VPN profiles")
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2();
                if live.profiles.is_empty() {
                    profiles = profiles.child(
                        div()
                            .p_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(separator)
                            .bg(if dark { rgb(0x242424) } else { rgb(0xffffff) })
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("No saved profiles found"),
                            )
                            .child(div().text_sm().text_color(muted).child(
                                "Add an L2TP profile in macOS System Settings > VPN, then reload.",
                            )),
                    );
                }
                for (index, profile) in live.profiles.iter().enumerate() {
                    let id = profile.id.as_str().to_owned();
                    let selected = self.draft.profile_id.as_deref() == Some(id.as_str());
                    let this = entity.clone();
                    let is_disabled = busy || live.state != SessionState::Disconnected;
                    profiles = profiles.child(
                        div()
                            .id(("profile", index))
                            .role(accesskit::Role::RadioButton)
                            .aria_label(profile.name.clone())
                            .aria_selected(selected)
                            .when(!is_disabled, |d| d.focusable())
                            .px_3p5()
                            .py_2p5()
                            .rounded_lg()
                            .border_1()
                            .border_color(if selected { rgb(0x0866d9) } else { separator })
                            .bg(if selected {
                                if dark {
                                    rgba(0x0866d920)
                                } else {
                                    rgba(0x0866d90c)
                                }
                            } else {
                                if dark { rgb(0x242424) } else { rgb(0xffffff) }
                            })
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_between()
                            .hover(move |style| {
                                if !selected {
                                    style.bg(if dark { rgb(0x2a2a2a) } else { rgb(0xf8f8f8) })
                                } else {
                                    style
                                }
                            })
                            .focus_visible(|style| style.border_color(rgb(0x0866d9)))
                            .on_click(move |_, _, cx| {
                                if !is_disabled {
                                    this.update(cx, |this, cx| {
                                        this.draft.profile_id = Some(id.clone());
                                        this.dirty = true;
                                        this.error = None;
                                        this.notice = None;
                                        cx.notify();
                                    });
                                }
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .size(px(16.))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(if selected {
                                                rgb(0x0866d9)
                                            } else {
                                                muted
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(selected, |c| {
                                                c.child(
                                                    div()
                                                        .size(px(8.))
                                                        .rounded_full()
                                                        .bg(rgb(0x0866d9)),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_size(px(14.))
                                            .child(profile.name.clone()),
                                    ),
                            )
                            .child(if !profile.supported {
                                div().text_xs().text_color(muted).child("Unsupported")
                            } else if !profile.split_enabled {
                                div()
                                    .text_xs()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .bg(if dark {
                                        rgba(0xff950020)
                                    } else {
                                        rgba(0xff950015)
                                    })
                                    .text_color(if dark { rgb(0xffb340) } else { rgb(0xb36200) })
                                    .child("Send all traffic must be off")
                            } else {
                                div()
                                    .text_xs()
                                    .text_color(if selected { rgb(0x0866d9) } else { muted })
                                    .child("Ready")
                            }),
                    );
                }
                content = content.child(profiles).child(button(
                    "reload-profiles",
                    "Reload profiles",
                    busy,
                    |_, cx| tray::send(cx, Command::ReloadProfiles),
                ));
            }
            Page::Routing => {
                let focus_editor = self.editor.clone();
                let import = entity.clone();
                let export = entity.clone();
                let rule_count = self.draft.destinations.len();
                content = content
                    .child(
                        div()
                            .text_color(muted)
                            .child("Only these destinations use the VPN. Other traffic keeps its normal route."),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .id("destinations-label")
                                    .role(accesskit::Role::Label)
                                    .aria_label("VPN destinations")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .on_click(move |_, window, cx| {
                                        window.focus(&focus_editor.focus_handle(cx), cx)
                                    })
                                    .child("VPN destinations"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{rule_count} configured")),
                            ),
                    )
                    .child(self.editor.clone())
                    .child(div().text_sm().text_color(muted).child("One IPv4, CIDR, or hostname per line. No URLs, wildcards, or IPv6. Hostname routes refresh every 5 minutes while active."))
                    .child(div().flex().flex_wrap().gap_2()
                        .child(button("import-rules", "Import…", busy, move |_, cx| import.update(cx, |this, cx| this.import(cx))))
                        .child(button("export-rules", "Export…", busy, move |_, cx| export.update(cx, |this, cx| this.export(cx)))));
            }
            Page::General => {
                let login = entity.clone();
                let is_login = self.draft.launch_at_login;
                content =
                    content
                        .child(
                            div()
                                .text_color(muted)
                                .child("Choose how VPN Splitter starts on this Mac."),
                        )
                        .child(
                            div()
                                .p_3p5()
                                .rounded_lg()
                                .border_1()
                                .border_color(separator)
                                .bg(if dark { rgb(0x242424) } else { rgb(0xffffff) })
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_4()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_size(px(14.))
                                                .child("Launch at login"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child("Start VPN Splitter in the menu bar when you log in to macOS."),
                                        ),
                                )
                                .child(styled_button(
                                    "launch-at-login",
                                    if is_login { "Enabled" } else { "Disabled" },
                                    busy || live.state != SessionState::Disconnected,
                                    if is_login {
                                        ButtonVariant::Primary
                                    } else {
                                        ButtonVariant::Secondary
                                    },
                                    move |_, cx| {
                                        login.update(cx, |this, cx| {
                                            this.draft.launch_at_login = !this.draft.launch_at_login;
                                            this.dirty = true;
                                            this.notice = None;
                                            cx.notify();
                                        });
                                    },
                                )),
                        )
                        .child(div().text_sm().text_color(muted).child(
                            "Starts in the menu bar without showing the main window. VPN connections are initiated manually.",
                        ));
            }
            Page::Helper => {
                let (badge_bg, badge_fg, status_title, helper_desc) = match live.helper_status {
                    1 => (
                        if dark {
                            rgba(0x34c75925)
                        } else {
                            rgba(0x34c75918)
                        },
                        if dark { rgb(0x4cd964) } else { rgb(0x28a745) },
                        "Installed & Active",
                        "The privileged helper is running and authorized to manage routing tables.",
                    ),
                    2 => (
                        if dark {
                            rgba(0xff950025)
                        } else {
                            rgba(0xff950018)
                        },
                        if dark { rgb(0xffb340) } else { rgb(0xc67100) },
                        "Approval Required",
                        "Approve the helper in macOS System Settings > General > Login Items & Extensions.",
                    ),
                    _ => (
                        if dark {
                            rgba(0x8e8e9325)
                        } else {
                            rgba(0x8e8e9318)
                        },
                        muted,
                        "Local Setup Required",
                        "Installation requests administrator approval and registers the root helper tool.",
                    ),
                };
                content = content
                    .child(
                        div()
                            .text_color(muted)
                            .child("The helper manages VPN routes with administrator permission."),
                    )
                    .child(
                        div()
                            .p_3p5()
                            .rounded_lg()
                            .border_1()
                            .border_color(separator)
                            .bg(if dark { rgb(0x242424) } else { rgb(0xffffff) })
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_size(px(14.))
                                            .child("System Route Helper"),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_md()
                                            .bg(badge_bg)
                                            .text_color(badge_fg)
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(status_title),
                                    ),
                            )
                            .child(div().text_xs().text_color(muted).child(helper_desc)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button("install-helper", "Install / approve helper…",
                        live.state != SessionState::Disconnected || live.preparing || live.helper_busy,
                        |_, cx| tray::send(cx, Command::InstallHelper)))
                    .child(button("remove-helper", "Remove helper…",
                        live.state != SessionState::Disconnected || live.preparing || live.helper_busy || live.helper_status != 1,
                        |_, cx| tray::send(cx, Command::RemoveHelper))))
                    .child(div().text_sm().text_color(muted).child("Disconnect before changing the helper. Removal checks route cleanup first."));
            }
            Page::Diagnostics => {
                let redacted = crate::diagnostics::export(&live, false)
                    .unwrap_or_else(|_| "Diagnostics unavailable".into());
                let private = crate::diagnostics::export(&live, true).ok();
                let copy = redacted.clone();
                content = content
                    .child(div().text_color(muted).child("Reports hide private infrastructure details by default."))
                    .child(
                        div()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(separator)
                            .bg(if dark { rgb(0x181818) } else { rgb(0xffffff) })
                            .font_family("Menlo")
                            .text_xs()
                            .line_height(px(18.))
                            .child(redacted),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button("copy-redacted", "Copy redacted report", false, move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                    .child(button("copy-private", "Copy private report…", private.is_none(), move |_, cx| {
                        if let Some(text) = &private { cx.write_to_clipboard(ClipboardItem::new_string(text.clone())); }
                            })),
                    )
                    .child(div().text_sm().text_color(muted).child("Private reports include profile names and destinations. Share only with people you trust."));
            }
        }

        if let Some(error) = self
            .error
            .as_deref()
            .or_else(|| live.error.map(controller::message))
        {
            content = content.child(
                div()
                    .id("settings-error")
                    .role(accesskit::Role::Alert)
                    .aria_label(error.to_owned())
                    .text_color(if dark { rgb(0xffada3) } else { rgb(0xa52b20) })
                    .child(format!("Error: {error}")),
            );
        }
        if let Some(notice) = &self.notice {
            content = content.child(div().text_sm().child(notice.clone()));
        }
        for warning in &live.warnings {
            content = content.child(div().text_sm().text_color(muted).child(warning.clone()));
        }

        let save = entity;
        let (status_dot, status_text) = match live.state {
            SessionState::Active => (rgb(0x34c759), "Routing active"),
            SessionState::Connecting => (rgb(0xff9500), "Connecting…"),
            SessionState::Applying => (rgb(0xff9500), "Applying…"),
            SessionState::Disconnecting => (rgb(0xff9500), "Disconnecting…"),
            SessionState::CleanupRequired => (rgb(0xff3b30), "Cleanup required"),
            SessionState::Disconnected => (
                if live.vpn_connected {
                    rgb(0x34c759)
                } else {
                    rgb(0x8e8e93)
                },
                if live.vpn_connected {
                    "VPN connected"
                } else {
                    "Disconnected"
                },
            ),
        };

        let footer = div()
            .flex_shrink_0()
            .border_t_1()
            .border_color(separator)
            .px_6()
            .py_3p5()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(status_dot)
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .id("connection-status")
                            .role(accesskit::Role::Label)
                            .aria_label(format!(
                                "VPN {}. Routing {:?}",
                                if live.vpn_connected {
                                    "connected"
                                } else {
                                    "disconnected"
                                },
                                live.state
                            ))
                            .text_sm()
                            .child(status_text),
                    )
                    .when(self.dirty, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(if dark {
                                    rgba(0xff950025)
                                } else {
                                    rgba(0xff950018)
                                })
                                .text_color(if dark { rgb(0xffb340) } else { rgb(0xc67100) })
                                .child("Unsaved changes"),
                        )
                    }),
            );
        let footer = footer.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .child(styled_button(
                    "save-rules",
                    if live.state == SessionState::Active {
                        "Save / Apply"
                    } else {
                        "Save changes"
                    },
                    busy || self.awaiting_save || !self.dirty,
                    ButtonVariant::Secondary,
                    move |window, cx| {
                        save.update(cx, |this, cx| this.save(&SaveSettings, window, cx))
                    },
                ))
                .child(styled_button(
                    "connect-vpn",
                    if live.state == SessionState::Disconnected {
                        "Connect VPN"
                    } else if live.adopted {
                        "Clear routes / keep VPN"
                    } else {
                        "Disconnect VPN"
                    },
                    connection_action_disabled(&live, self.dirty, self.awaiting_save),
                    if live.state == SessionState::Disconnected {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Destructive
                    },
                    move |_, cx| {
                        tray::send(
                            cx,
                            if live.state == SessionState::Disconnected {
                                Command::Connect
                            } else {
                                Command::Disconnect
                            },
                        )
                    },
                ))
                .when(
                    live.state == SessionState::CleanupRequired
                        || live.error == Some(vpn_splitter_core::protocol::FaultCode::Cleanup),
                    |row| {
                        row.child(styled_button(
                            "retry-cleanup",
                            "Retry cleanup",
                            live.helper_busy,
                            ButtonVariant::Secondary,
                            |_, cx| tray::send(cx, Command::RetryCleanup),
                        ))
                    },
                ),
        );

        div()
            .id("vpn-settings")
            .track_focus(&self.focus_handle)
            .role(accesskit::Role::Group)
            .aria_label("VPN Splitter settings")
            .key_context("Settings")
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .size_full()
            .bg(bg)
            .text_color(fg)
            .text_size(px(14.))
            .flex()
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(content)
                    .child(footer),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unsaved_edits_do_not_block_disconnect_or_cancel() {
        let mut view = crate::controller::View::default();
        for state in [
            vpn_splitter_core::session::SessionState::Active,
            vpn_splitter_core::session::SessionState::Connecting,
            vpn_splitter_core::session::SessionState::Applying,
        ] {
            view.state = state;
            assert!(!super::connection_action_disabled(&view, true, true));
        }
        view.state = vpn_splitter_core::session::SessionState::Disconnected;
        assert!(super::connection_action_disabled(&view, true, false));
    }
}
