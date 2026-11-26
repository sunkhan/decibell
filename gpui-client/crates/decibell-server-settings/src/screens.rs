//! The settings screens, drawn from [`crate::fixture`] (no network, a pinned clock): the main
//! layout of the shell's fixture with a settings window open over it, on a given tab and state.
//! `decibell-capture --screen <name>` / `decibell --screen <name>`; `server-settings-narrow` is
//! meant for `--size 900x700` (the rail).

use decibell_harness::Screen;
use decibell_model::ChannelId;
use decibell_picture_editor::{EditorOptions, PictureEditor, PictureSource};
use decibell_shell::Shell;
use decibell_ui::AssumeClientDecorations;
use gpui::{AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, div};

use crate::channel::{ChannelSettingsWindow, Tab as ChannelTab};
use crate::fixture;
use crate::server::{ServerSettingsWindow, Tab, TabId};

/// The main layout with a window over it.
struct ScreenRoot {
    shell: Entity<Shell>,
    window: AnyView,
}

impl Render for ScreenRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().child(self.shell.clone()).child(self.window.clone())
    }
}

/// Captures render one frame on a clock that doesn't run: entrances show their end.
fn settle(cx: &mut App) {
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
}

fn server_screen(tab: TabId, window: &mut Window, cx: &mut App, configure: impl FnOnce(&Tab, &mut Window, &mut App)) -> AnyView {
    settle(cx);
    fixture::apply(cx);
    let shell = cx.new(Shell::new);
    let settings = cx.new(|cx| ServerSettingsWindow::new(fixture::server(), window, cx));
    settings.update(cx, |w, cx| w.show(tab, window, cx));
    let open = settings.read(cx).tab().clone();
    configure(&open, window, cx);
    cx.new(|_| ScreenRoot { shell, window: settings.into() }).into()
}

fn channel_screen(channel: &'static str, tab: ChannelTab, window: &mut Window, cx: &mut App, configure: impl FnOnce(&Entity<ChannelSettingsWindow>, &mut Window, &mut App)) -> AnyView {
    settle(cx);
    let model = fixture::apply(cx);
    model.overlays.update(cx, |o, _| o.channel_settings_channel = Some(ChannelId::from(channel)));
    let shell = cx.new(Shell::new);
    let settings = cx.new(|cx| ChannelSettingsWindow::new(fixture::server(), ChannelId::from(channel), window, cx));
    settings.update(cx, |w, cx| w.select_tab(tab, cx));
    configure(&settings, window, cx);
    cx.new(|_| ScreenRoot { shell, window: settings.into() }).into()
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "server-settings-overview",
            about: "Server Settings → Overview as the owner: identity, public listing, picture, transfer ownership (fixture)",
            build: |window, cx| server_screen(TabId::Overview, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "server-settings-narrow",
            about: "Server Settings below 1024 px (capture with --size 900x700): the icon rail (fixture)",
            build: |window, cx| server_screen(TabId::Overview, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "transfer-ownership",
            about: "Server Settings → Overview: a new owner chosen, the confirmation half typed (fixture)",
            build: |window, cx| {
                server_screen(TabId::Overview, window, cx, |tab, window, cx| {
                    if let Tab::Overview(t) = tab {
                        t.update(cx, |t, cx| t.fixture_transfer(window, cx));
                    }
                })
            },
        },
        Screen {
            name: "server-settings-members",
            about: "Server Settings → Members: the server's search, roles, the counts (fixture)",
            build: |window, cx| server_screen(TabId::Members, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "server-settings-members-search",
            about: "Server Settings → Members searching \"er\" (fixture)",
            build: |window, cx| {
                server_screen(TabId::Members, window, cx, |tab, window, cx| {
                    if let Tab::Members(t) = tab {
                        t.update(cx, |t, cx| t.fixture_search("er", window, cx));
                    }
                })
            },
        },
        Screen {
            name: "server-settings-roles",
            about: "Server Settings → Roles: the hierarchy and Moderator's Display section (fixture)",
            build: |window, cx| server_screen(TabId::Roles, window, cx, |tab, _, cx| roles(tab, 3, crate::server::roles::Section::Display, false, cx)),
        },
        Screen {
            name: "server-settings-roles-permissions",
            about: "Server Settings → Roles: Moderator's permissions (fixture)",
            build: |window, cx| server_screen(TabId::Roles, window, cx, |tab, _, cx| roles(tab, 3, crate::server::roles::Section::Permissions, false, cx)),
        },
        Screen {
            name: "server-settings-roles-members",
            about: "Server Settings → Roles: Moderator's members, searched on the server (fixture)",
            build: |window, cx| server_screen(TabId::Roles, window, cx, |tab, _, cx| roles(tab, 3, crate::server::roles::Section::Members, false, cx)),
        },
        Screen {
            name: "server-settings-roles-everyone",
            about: "Server Settings → Roles: @everyone's reduced editor (fixture)",
            build: |window, cx| server_screen(TabId::Roles, window, cx, |tab, _, cx| roles(tab, 1, crate::server::roles::Section::Permissions, false, cx)),
        },
        Screen {
            name: "server-settings-roles-unsaved",
            about: "Server Settings → Roles: Streamer renamed and recoloured, the save bar up (fixture)",
            build: |window, cx| server_screen(TabId::Roles, window, cx, |tab, _, cx| roles(tab, 6, crate::server::roles::Section::Display, true, cx)),
        },
        Screen {
            name: "color-picker",
            about: "Server Settings → Roles: Event Host's custom colour open in the picker (fixture)",
            build: |window, cx| {
                server_screen(TabId::Roles, window, cx, |tab, window, cx| {
                    roles(tab, 5, crate::server::roles::Section::Display, false, cx);
                    if let Tab::Roles(t) = tab {
                        t.update(cx, |t, cx| t.fixture_color_picker(window, cx));
                    }
                })
            },
        },
        Screen {
            name: "server-settings-bans",
            about: "Server Settings → Bans: three bans, the truncated notice (fixture)",
            build: |window, cx| server_screen(TabId::Bans, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "server-settings-audit-log",
            about: "Server Settings → Audit Log: one entry of every action, grouped by day (fixture)",
            build: |window, cx| server_screen(TabId::Audit, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "server-settings-audit-log-filters",
            about: "Server Settings → Audit Log: the action filter open (fixture)",
            build: |window, cx| {
                server_screen(TabId::Audit, window, cx, |tab, _, cx| {
                    if let Tab::Audit(t) = tab {
                        t.update(cx, |t, cx| t.fixture_open_filter(cx));
                    }
                })
            },
        },
        Screen {
            name: "server-settings-storage",
            about: "Server Settings → Storage (fixture)",
            build: |window, cx| server_screen(TabId::Storage, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "channel-settings-overview",
            about: "Channel Settings → Overview of #general (fixture)",
            build: |window, cx| channel_screen("general", ChannelTab::Overview, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "channel-settings-overview-voice",
            about: "Channel Settings → Overview of the Lounge voice channel: bitrate (fixture)",
            build: |window, cx| channel_screen("lounge", ChannelTab::Overview, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "channel-settings-overview-category",
            about: "Channel Settings → Overview of the Text Channels category (fixture)",
            build: |window, cx| channel_screen("cat-text-channels", ChannelTab::Overview, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "channel-settings-permissions",
            about: "Channel Settings → Permissions of #general: @everyone's allow / inherit / deny (fixture)",
            build: |window, cx| channel_screen("general", ChannelTab::Permissions, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "channel-settings-permissions-sync",
            about: "Channel Settings → Permissions of the syncing Text Channels category (fixture)",
            build: |window, cx| channel_screen("cat-text-channels", ChannelTab::Permissions, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "channel-settings-retention",
            about: "Channel Settings → Retention of #clips: a custom value, the wipe (fixture)",
            build: |window, cx| channel_screen("clips", ChannelTab::Retention, window, cx, |_, _, _| {}),
        },
        Screen {
            name: "picture-editor",
            about: "Server Settings → Overview with the server picture editor open (generated picture)",
            build: |window, cx| {
                server_screen(TabId::Overview, window, cx, |tab, window, cx| {
                    if let Tab::Overview(t) = tab {
                        t.update(cx, |t, cx| {
                            let picture = decibell_picture_editor::picture::from_rgba(
                                decibell_picture_editor::picture::sample_rgba(1600, 900),
                                "sample.png",
                                decibell_picture_editor::PictureShape::ServerTile.aspect(),
                            );
                            let editor = cx.new(|cx| {
                                PictureEditor::new(PictureSource::Bytes { bytes: Vec::new(), name: String::new() }, EditorOptions::server("Daily Gaming", Some("298 members".into())), window, cx)
                                    .with_picture(picture)
                            });
                            t.fixture_editor(editor, cx);
                        });
                    }
                })
            },
        },
    ]
}

fn roles(tab: &Tab, role: i64, section: crate::server::roles::Section, unsaved: bool, cx: &mut App) {
    if let Tab::Roles(t) = tab {
        t.update(cx, |t, cx| t.fixture_select(role, section, unsaved, cx));
    }
}
