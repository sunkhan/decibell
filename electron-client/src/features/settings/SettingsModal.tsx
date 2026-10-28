import { createPortal } from "react-dom";
import { useUiStore } from "../../stores/uiStore";
import AccountTab from "./tabs/AccountTab";
import DevicesTab from "./tabs/DevicesTab";
import { useAuthStore } from "../../stores/authStore";
import AppearanceTab from "./tabs/AppearanceTab";
import PrivacyTab from "./tabs/PrivacyTab";
import AudioTab from "./tabs/AudioTab";
import KeybindsTab from "./tabs/KeybindsTab";
import NetworkTab from "./tabs/NetworkTab";
import CodecsTab from "./tabs/CodecsTab";
import AboutTab from "./tabs/AboutTab";
import { useEffect, useState, useCallback } from "react";
import {
  SETTINGS_BACKDROP,
  SETTINGS_BODY,
  SETTINGS_FRAME,
  SETTINGS_PANE,
  SettingsNav,
  SettingsNavItem,
  SettingsNavLabel,
  SettingsPaneHeader,
} from "../../components/SettingsWindow";

const TABS = [
  {
    id: "account",
    label: "Account",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <circle cx="12" cy="8" r="4" />
        <path d="M20 21a8 8 0 10-16 0" />
      </svg>
    ),
    component: AccountTab,
  },
  {
    id: "devices",
    label: "Devices",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="2" y="4" width="20" height="13" rx="2" />
        <path d="M8 21h8M12 17v4" />
      </svg>
    ),
    component: DevicesTab,
  },
  {
    id: "appearance",
    label: "Appearance",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="9" />
        <path d="M12 3a9 9 0 0 0 0 18 4.5 4.5 0 0 0 0-9 4.5 4.5 0 0 1 0-9z" />
      </svg>
    ),
    component: AppearanceTab,
  },
  {
    id: "privacy",
    label: "Privacy",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <rect x="3" y="11" width="18" height="11" rx="2" />
        <path d="M7 11V7a5 5 0 0110 0v4" />
      </svg>
    ),
    component: PrivacyTab,
  },
  {
    id: "audio",
    label: "Audio",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <path d="M9 18V5l12-2v13" />
        <circle cx="6" cy="18" r="3" />
        <circle cx="18" cy="16" r="3" />
      </svg>
    ),
    component: AudioTab,
  },
  {
    id: "keybinds",
    label: "Keybinds",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="2" y="6" width="20" height="12" rx="2" />
        <path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M8 14h8" />
      </svg>
    ),
    component: KeybindsTab,
  },
  {
    id: "network",
    label: "Network",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M5 12h14" />
        <path d="M12 5v14" />
        <path d="M5 5l14 14" />
        <path d="M19 5L5 19" />
      </svg>
    ),
    component: NetworkTab,
  },
  {
    id: "codecs",
    label: "Codecs",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="3" y="6" width="18" height="12" rx="2" />
        <path d="M9 10l4 2-4 2z" fill="currentColor" stroke="none" />
        <path d="M16 10v4" />
      </svg>
    ),
    component: CodecsTab,
  },
  {
    id: "about",
    label: "About",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="16" x2="12" y2="12" />
        <line x1="12" y1="8" x2="12.01" y2="8" />
      </svg>
    ),
    component: AboutTab,
  },
];

export default function SettingsModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const settingsTab = useUiStore((s) => s.settingsTab);
  const setSettingsTab = useUiStore((s) => s.setSettingsTab);
  // The Devices tab needs a central that serves it (LoginResponse.devices).
  const devices = useAuthStore((s) => s.devices);
  const tabs = devices ? TABS : TABS.filter((t) => t.id !== "devices");

  const isOpen = activeModal === "settings";
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);

  // Mount → animate in
  useEffect(() => {
    if (isOpen) {
      setMounted(true);
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
    } else {
      setVisible(false);
    }
  }, [isOpen]);

  // After close animation finishes, unmount
  const handleTransitionEnd = useCallback(() => {
    if (!visible) setMounted(false);
  }, [visible]);

  useEffect(() => {
    if (!mounted) return;
    // A dialog over the window (the picture editor, rename, password) takes
    // its own Esc and marks it handled.
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) closeModal();
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [mounted, closeModal]);

  if (!mounted) return null;

  const activeTab = tabs.find((t) => t.id === settingsTab) ?? tabs[0];
  const TabComponent = activeTab.component;

  return createPortal(
    <div
      className={SETTINGS_BACKDROP}
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={closeModal}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className={SETTINGS_FRAME}
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <SettingsNav>
          <SettingsNavLabel>Settings</SettingsNavLabel>
          {tabs.map((tab) => (
            <SettingsNavItem
              key={tab.id}
              icon={tab.icon}
              label={tab.label}
              active={settingsTab === tab.id}
              onClick={() => setSettingsTab(tab.id)}
            />
          ))}
        </SettingsNav>

        <div className={SETTINGS_PANE}>
          <SettingsPaneHeader title={activeTab.label} onClose={closeModal} />
          <div className={SETTINGS_BODY}>
            <TabComponent />
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
}
