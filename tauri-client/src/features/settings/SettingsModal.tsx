import { createPortal } from "react-dom";
import { useUiStore } from "../../stores/uiStore";
import AccountTab from "./tabs/AccountTab";
import PrivacyTab from "./tabs/PrivacyTab";
import AudioTab from "./tabs/AudioTab";
import NetworkTab from "./tabs/NetworkTab";
import AboutTab from "./tabs/AboutTab";
import { useEffect, useState, useCallback } from "react";

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
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [mounted, closeModal]);

  if (!mounted) return null;

  const activeTab = TABS.find((t) => t.id === settingsTab) ?? TABS[0];
  const TabComponent = activeTab.component;

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={closeModal}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="flex h-[560px] w-[820px] overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)] transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Left sidebar */}
        <div className="flex w-[210px] shrink-0 flex-col gap-0.5 border-r border-border-divider bg-bg-darkest px-3 py-6">
          <div className="mb-2 px-3 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Settings
          </div>
          {TABS.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setSettingsTab(tab.id)}
              className={`flex items-center gap-2.5 rounded-[10px] px-3 py-[9px] text-[14px] transition-colors ${
                settingsTab === tab.id
                  ? "bg-accent-soft font-medium text-text-primary"
                  : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              <span className={settingsTab === tab.id ? "text-accent-bright" : "text-text-muted"}>
                {tab.icon}
              </span>
              {tab.label}
            </button>
          ))}
        </div>

        {/* Right content */}
        <div className="flex flex-1 flex-col overflow-y-auto scrollbar-thin">
          <div className="flex items-center justify-between px-8 pt-7 pb-5">
            <h2 className="font-display text-xl font-semibold text-text-primary">
              {activeTab.label}
            </h2>
            <button
              onClick={closeModal}
              className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
          <div className="flex-1 px-8 pb-7">
            <TabComponent />
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
}
