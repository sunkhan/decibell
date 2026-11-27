//! The tabs (`features/settings/tabs/`), each its own view: built when it shows, dropped when the
//! window moves to another tab or closes (React's mount / unmount: the Devices tab's list load,
//! the Audio tab's meter and the Keybinds recorder live exactly as long), drawn cached at the
//! pane's size, watching exactly what it reads.

pub mod about;
pub mod account;
pub mod appearance;
pub mod audio;
pub mod codecs;
pub mod devices;
pub mod keybinds;
pub mod network;
pub mod privacy;

use decibell_ui::{IconName, ScrollHandle, SettingsPane};
use gpui::{AnyView, App, AppContext as _, Entity, Window};

use crate::window::CloseLink;

/// A tab's pane: its title over the body, scrolling together; ✕ asks the window to close.
pub(crate) fn pane(tab: TabId, scroll: &ScrollHandle, close: &CloseLink) -> SettingsPane {
    let close = close.clone();
    SettingsPane::new(("settings-pane", tab as usize), scroll)
        .title(tab.label())
        .on_close(move |_, _, cx| close.request(cx))
}

/// The tabs in the sidebar's order (`SettingsModal.tsx` `TABS`), with the ids `uiStore.settingsTab`
/// uses (the model's `nav.settings_tab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabId {
    Account,
    Devices,
    Appearance,
    Privacy,
    Audio,
    Keybinds,
    Network,
    Codecs,
    About,
}

impl TabId {
    pub const ALL: [TabId; 9] = [
        TabId::Account,
        TabId::Devices,
        TabId::Appearance,
        TabId::Privacy,
        TabId::Audio,
        TabId::Keybinds,
        TabId::Network,
        TabId::Codecs,
        TabId::About,
    ];

    pub fn id(self) -> &'static str {
        match self {
            TabId::Account => "account",
            TabId::Devices => "devices",
            TabId::Appearance => "appearance",
            TabId::Privacy => "privacy",
            TabId::Audio => "audio",
            TabId::Keybinds => "keybinds",
            TabId::Network => "network",
            TabId::Codecs => "codecs",
            TabId::About => "about",
        }
    }

    pub fn from_id(id: &str) -> Option<TabId> {
        TabId::ALL.into_iter().find(|t| t.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            TabId::Account => "Account",
            TabId::Devices => "Devices",
            TabId::Appearance => "Appearance",
            TabId::Privacy => "Privacy",
            TabId::Audio => "Audio",
            TabId::Keybinds => "Keybinds",
            TabId::Network => "Network",
            TabId::Codecs => "Codecs",
            TabId::About => "About",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            TabId::Account => IconName::SettingsTabAccount,
            TabId::Devices => IconName::SettingsTabDevices,
            TabId::Appearance => IconName::SettingsTabAppearance,
            TabId::Privacy => IconName::SettingsTabPrivacy,
            TabId::Audio => IconName::SettingsTabAudio,
            TabId::Keybinds => IconName::SettingsTabKeybinds,
            TabId::Network => IconName::SettingsTabNetwork,
            TabId::Codecs => IconName::SettingsTabCodecs,
            TabId::About => IconName::SettingsTabAbout,
        }
    }
}

/// The open tab's view.
pub enum TabView {
    Account(Entity<account::AccountTab>),
    Devices(Entity<devices::DevicesTab>),
    Appearance(Entity<appearance::AppearanceTab>),
    Privacy(Entity<privacy::PrivacyTab>),
    Audio(Entity<audio::AudioTab>),
    Keybinds(Entity<keybinds::KeybindsTab>),
    Network(Entity<network::NetworkTab>),
    Codecs(Entity<codecs::CodecsTab>),
    About(Entity<about::AboutTab>),
}

impl TabView {
    pub fn build(tab: TabId, close: CloseLink, window: &mut Window, cx: &mut App) -> TabView {
        match tab {
            TabId::Account => TabView::Account(cx.new(|cx| account::AccountTab::new(close, window, cx))),
            TabId::Devices => TabView::Devices(cx.new(|cx| devices::DevicesTab::new(close, window, cx))),
            TabId::Appearance => TabView::Appearance(cx.new(|cx| appearance::AppearanceTab::new(close, window, cx))),
            TabId::Privacy => TabView::Privacy(cx.new(|cx| privacy::PrivacyTab::new(close, window, cx))),
            TabId::Audio => TabView::Audio(cx.new(|cx| audio::AudioTab::new(close, window, cx))),
            TabId::Keybinds => TabView::Keybinds(cx.new(|cx| keybinds::KeybindsTab::new(close, window, cx))),
            TabId::Network => TabView::Network(cx.new(|cx| network::NetworkTab::new(close, window, cx))),
            TabId::Codecs => TabView::Codecs(cx.new(|cx| codecs::CodecsTab::new(close, window, cx))),
            TabId::About => TabView::About(cx.new(|cx| about::AboutTab::new(close, window, cx))),
        }
    }

    pub fn tab(&self) -> TabId {
        match self {
            TabView::Account(_) => TabId::Account,
            TabView::Devices(_) => TabId::Devices,
            TabView::Appearance(_) => TabId::Appearance,
            TabView::Privacy(_) => TabId::Privacy,
            TabView::Audio(_) => TabId::Audio,
            TabView::Keybinds(_) => TabId::Keybinds,
            TabView::Network(_) => TabId::Network,
            TabView::Codecs(_) => TabId::Codecs,
            TabView::About(_) => TabId::About,
        }
    }

    pub fn view(&self) -> AnyView {
        match self {
            TabView::Account(v) => v.clone().into(),
            TabView::Devices(v) => v.clone().into(),
            TabView::Appearance(v) => v.clone().into(),
            TabView::Privacy(v) => v.clone().into(),
            TabView::Audio(v) => v.clone().into(),
            TabView::Keybinds(v) => v.clone().into(),
            TabView::Network(v) => v.clone().into(),
            TabView::Codecs(v) => v.clone().into(),
            TabView::About(v) => v.clone().into(),
        }
    }

    /// Leaving the tab (another tab, closing): what it holds unapplied applies first, as the blur
    /// the renderer's leaving press causes (only Network's custom speed box holds anything: every
    /// other control applies at once).
    pub fn commit_typed(&self, cx: &mut App) {
        if let TabView::Network(v) = self {
            v.update(cx, |v, cx| v.commit_all(cx));
        }
    }
}
