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
    Settings,
    Diagnostics,
}
actions!(settings, [SaveSettings, NextField, PreviousField]);
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
            page: Page::Settings,
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

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
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
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(0xb3bdb7))
        .cursor_pointer()
        .focus_visible(|style| style.border_color(rgb(0x2d6a4f)).bg(rgba(0x2d6a4f18)))
        .when(disabled, |d| {
            d.opacity(0.45)
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
        let bg = if dark { rgb(0x202522) } else { rgb(0xf5f5f3) };
        let fg = if dark { rgb(0xe7ece8) } else { rgb(0x202421) };
        let entity = cx.entity();
        let settings = entity.clone();
        let diagnostics = entity.clone();
        let mut root = div()
            .id("vpn-settings")
            .role(accesskit::Role::Group)
            .aria_label("VPN Splitter settings")
            .key_context("Settings")
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .size_full()
            .overflow_y_scroll()
            .bg(bg)
            .text_color(fg)
            .text_size(px(14.))
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("VPN Splitter"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button("settings-tab", "Settings", false, move |_, cx| {
                        settings.update(cx, |this, cx| {
                            this.page = Page::Settings;
                            cx.notify();
                        })
                    }))
                    .child(button(
                        "diagnostics-tab",
                        "Diagnostics",
                        false,
                        move |_, cx| {
                            diagnostics.update(cx, |this, cx| {
                                this.page = Page::Diagnostics;
                                cx.notify();
                            })
                        },
                    )),
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
                    .child(format!(
                        "VPN: {}   ·   Routes: {:?}",
                        if live.vpn_connected {
                            "connected"
                        } else {
                            "disconnected"
                        },
                        live.state
                    )),
            );
        if self.page == Page::Diagnostics {
            let redacted = crate::diagnostics::export(&live, false)
                .unwrap_or_else(|_| "Diagnostics unavailable".into());
            let private = crate::diagnostics::export(&live, true).ok();
            let copy = redacted.clone();
            return root.child(div().text_sm().child(redacted)).child(div().flex().gap_2()
                .child(button("copy-redacted","Copy redacted report",false,move|_,cx|cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                .child(button("copy-private","Copy with infrastructure (private)",private.is_none(),move|_,cx|{if let Some(text)=&private{cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));}})))
                .child("Reports exclude infrastructure by default. An explicit private report includes profile names and destinations.");
        }
        let busy = live.helper_busy
            || live.preparing
            || !matches!(
                live.state,
                SessionState::Disconnected | SessionState::Active
            )
            || self.file_busy;
        root = root.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Saved L2TP profile"),
        );
        let mut profiles = div()
            .id("profiles")
            .role(accesskit::Role::Group)
            .aria_label("Saved VPN profiles")
            .max_h(px(180.))
            .flex_shrink_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        if live.profiles.is_empty() {
            profiles = profiles.child("No saved profiles. Add one in macOS VPN settings.");
        }
        for (index, profile) in live.profiles.iter().enumerate() {
            let id = profile.id.as_str().to_owned();
            let selected = self.draft.profile_id.as_deref() == Some(id.as_str());
            let this = entity.clone();
            let label = format!(
                "{}{}{}",
                if selected { "● " } else { "○ " },
                profile.name,
                if !profile.supported {
                    " — unsupported"
                } else if !profile.split_enabled {
                    " — disable Send all traffic"
                } else {
                    ""
                }
            );
            profiles = profiles.child(
                button(
                    ("profile", index),
                    label,
                    busy || live.state != SessionState::Disconnected,
                    move |_, cx| {
                        this.update(cx, |this, cx| {
                            this.draft.profile_id = Some(id.clone());
                            this.dirty = true;
                            this.error = None;
                            cx.notify();
                        })
                    },
                )
                .flex_shrink_0(),
            );
        }
        let reload = button("reload-profiles", "Reload profiles", busy, |_, cx| {
            tray::send(cx, Command::ReloadProfiles)
        });
        let import = entity.clone();
        let export = entity.clone();
        let save = entity.clone();
        let login = entity.clone();
        let focus_editor = self.editor.clone();
        let login_label = if self.draft.launch_at_login {
            "☑ Launch at login"
        } else {
            "☐ Launch at login"
        };
        root=root.child(profiles).child(reload)
            .child(div().id("destinations-label").role(accesskit::Role::Label).aria_label("VPN destinations").font_weight(FontWeight::SEMIBOLD).cursor_pointer().on_click(move|_,window,cx|window.focus(&focus_editor.focus_handle(cx),cx)).child("VPN destinations"))
            .child(self.editor.clone())
            .child(div().text_sm().child("One IPv4, CIDR, or hostname per line. No URLs, wildcard domains, or IPv6. Hostname routes refresh every 5 minutes while active."))
            .child(div().flex().gap_2()
                .child(button("import-rules","Import…",busy,move|_,cx|import.update(cx,|this,cx|this.import(cx))))
                .child(button("export-rules","Export…",busy,move|_,cx|export.update(cx,|this,cx|this.export(cx))))
                .child(button("save-rules",if live.state==SessionState::Active{"Save / Apply"}else{"Save"},busy||self.awaiting_save,move|window,cx|save.update(cx,|this,cx|this.save(&SaveSettings,window,cx)))))
            .child(button("launch-at-login",login_label,busy||live.state!=SessionState::Disconnected,move|_,cx|login.update(cx,|this,cx|{this.draft.launch_at_login = !this.draft.launch_at_login;this.dirty=true;cx.notify();})))
            .child(div().flex().gap_2()
                .child(button("connect-vpn",if live.state==SessionState::Disconnected{"Connect VPN"}else if live.adopted{"Clear routes / keep VPN"}else{"Disconnect VPN"},connection_action_disabled(&live, self.dirty, self.awaiting_save),move|_,cx|tray::send(cx,if live.state==SessionState::Disconnected{Command::Connect}else{Command::Disconnect})))
                .child(button("retry-cleanup","Retry cleanup",live.helper_busy || (live.state!=SessionState::CleanupRequired && live.error!=Some(vpn_splitter_core::protocol::FaultCode::Cleanup)),|_,cx|tray::send(cx,Command::RetryCleanup))));
        let helper = match live.helper_status {
            -1 => {
                "Helper: local setup required. Install / approve will request administrator permission and reopen the installed app."
            }
            0 => "Helper: not registered.",
            1 => "Helper: installed.",
            2 => "Helper: approval required in System Settings.",
            _ => "Helper: bundle not installed.",
        };
        root = root
            .child(div().text_sm().child(helper))
            .child(button(
                "install-helper",
                "Install / approve helper…",
                live.state != SessionState::Disconnected || live.preparing || live.helper_busy,
                |_, cx| tray::send(cx, Command::InstallHelper),
            ))
            .child(button(
                "remove-helper",
                "Remove helper…",
                live.state != SessionState::Disconnected
                    || live.preparing
                    || live.helper_busy
                    || live.helper_status != 1,
                |_, cx| tray::send(cx, Command::RemoveHelper),
            ));
        if let Some(error) = self
            .error
            .as_deref()
            .or_else(|| live.error.map(controller::message))
        {
            root = root.child(
                div()
                    .id("settings-error")
                    .role(accesskit::Role::Alert)
                    .aria_label(error.to_owned())
                    .text_color(if dark { rgb(0xffada3) } else { rgb(0xa52b20) })
                    .child(error.to_owned()),
            );
        }
        if let Some(notice) = &self.notice {
            root = root.child(div().text_sm().child(notice.clone()));
        }
        for warning in live.warnings {
            root = root.child(div().text_sm().child(warning));
        }
        root
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
