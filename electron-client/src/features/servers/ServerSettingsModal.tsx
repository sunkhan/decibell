import { createPortal } from "react-dom";
import { useEffect, useState, useCallback } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { PERM, usePermission } from "./permissions";
import OverviewTab from "./tabs/OverviewTab";
import MembersTab from "./tabs/MembersTab";
import RolesTab from "./tabs/RolesTab";
import BansTab from "./tabs/BansTab";

interface Props {
  serverId: string;
}

/// Unified server-settings screen (Discord-style): mirrors
/// SettingsModal chrome 1:1 (820×560, tabbed sidebar, fade-in
/// scale-95→1, Esc closes, backdrop click closes, portal to body).
///
/// Tabs are permission-gated to match the server: Overview and Members
/// are for everyone (editing inside them is gated per-control), Roles
/// needs MANAGE_ROLES, Bans needs BAN_MEMBERS (the server only sends
/// the ban list to those holders anyway). Non-owners get a Leave
/// Server action pinned to the sidebar bottom.
export default function ServerSettingsModal({ serverId }: Props) {
  const isOpen = useUiStore((s) => s.activeModal === "server-settings");
  const closeModal = useUiStore((s) => s.closeModal);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [activeTab, setActiveTab] = useState<string>("overview");
  const [leaveConfirm, setLeaveConfirm] = useState(false);

  const server = useChatStore((s) => s.servers.find((x) => x.id === serverId));
  const serverName = useChatStore(
    (s) => s.serverMeta[serverId]?.name ?? server?.name ?? "Server",
  );
  const owner = useChatStore((s) => s.serverOwner[serverId]);
  const currentUser = useAuthStore((s) => s.username);
  const isOwner = !!currentUser && !!owner && owner === currentUser;
  const canManageRoles = usePermission(serverId, PERM.MANAGE_ROLES);
  const canBan = usePermission(serverId, PERM.BAN_MEMBERS);

  const tabs = [
    {
      id: "overview",
      label: "Overview",
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <rect x="3" y="3" width="18" height="18" rx="2" />
          <path d="M3 9h18M9 3v18" />
        </svg>
      ),
    },
    {
      id: "members",
      label: "Members",
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
          <circle cx="9" cy="7" r="4" />
          <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
          <path d="M16 3.13a4 4 0 0 1 0 7.75" />
        </svg>
      ),
    },
    ...(canManageRoles
      ? [
          {
            id: "roles",
            label: "Roles",
            icon: (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
              </svg>
            ),
          },
        ]
      : []),
    ...(canBan
      ? [
          {
            id: "bans",
            label: "Bans",
            icon: (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <circle cx="12" cy="12" r="10" />
                <line x1="4.93" y1="4.93" x2="19.07" y2="19.07" />
              </svg>
            ),
          },
        ]
      : []),
  ];

  useEffect(() => {
    if (isOpen) {
      setMounted(true);
      requestAnimationFrame(() => setVisible(true));
    } else {
      setVisible(false);
      setLeaveConfirm(false);
    }
  }, [isOpen]);

  // Fresh roster + roles whenever the screen opens — every tab feeds
  // off these, and the pushed broadcasts keep them live afterwards.
  useEffect(() => {
    if (!isOpen) return;
    invoke("list_members", { serverId }).catch(console.error);
    invoke("list_roles", { serverId }).catch(() => {});
  }, [isOpen, serverId]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible) setMounted(false);
  }, [visible]);

  useEffect(() => {
    if (!mounted) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mounted, closeModal]);

  // A live permission change can hide the tab we're on — fall back.
  const effectiveTab = tabs.some((t) => t.id === activeTab)
    ? activeTab
    : "overview";

  const runLeave = () => {
    // Mirrors the sidebar's Disconnect flow.
    invoke("leave_server", { serverId }).catch(console.error);
    const chat = useChatStore.getState();
    chat.removeConnectedServer(serverId);
    chat.removePendingMembership(serverId);
    chat.setActiveServer(null);
    chat.setActiveChannel(null);
    setActiveView("home");
    closeModal();
  };

  if (!mounted || !server) return null;

  const activeTabDef = tabs.find((t) => t.id === effectiveTab) ?? tabs[0];

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{
        backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)",
      }}
      onClick={(e) => {
        // Guard: only close on a real backdrop click, not on clicks
        // bubbling up from nested overlays (cropper, confirm dialogs).
        if (e.target === e.currentTarget) closeModal();
      }}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="flex h-[560px] w-[820px] overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Sidebar */}
        <div className="flex w-[210px] shrink-0 flex-col gap-0.5 border-r border-border-divider bg-bg-darkest px-3 py-6">
          <div
            className="mb-2 truncate px-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted"
            title={serverName}
          >
            {serverName}
          </div>
          {tabs.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex items-center gap-2.5 rounded-md px-3 py-[9px] text-[14px] transition-colors ${
                effectiveTab === tab.id
                  ? "bg-accent-soft font-medium text-text-primary"
                  : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              <span
                className={
                  effectiveTab === tab.id ? "text-accent-bright" : "text-text-muted"
                }
              >
                {tab.icon}
              </span>
              {tab.label}
            </button>
          ))}

          {/* Leave server — pinned at the bottom, non-owners only (the
              owner can't leave their own server). */}
          {!isOwner && (
            <>
              <div className="flex-1" />
              <div className="mx-1.5 mb-1 h-px bg-border-divider" />
              <button
                onClick={() => setLeaveConfirm(true)}
                className="flex items-center gap-2.5 rounded-md px-3 py-[9px] text-[14px] font-normal text-text-muted transition-colors hover:bg-error/10 hover:text-error"
              >
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                  <polyline points="16 17 21 12 16 7" />
                  <line x1="21" y1="12" x2="9" y2="12" />
                </svg>
                Leave Server
              </button>
            </>
          )}
        </div>

        {/* Content */}
        <div className="flex flex-1 flex-col overflow-y-auto scrollbar-thin">
          <div className="flex items-center justify-between px-8 pt-7 pb-5">
            <h2 className="font-display text-[18px] font-semibold text-text-primary">
              {activeTabDef.label}
            </h2>
            <button
              onClick={closeModal}
              className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
          <div className="flex-1 px-8 pb-7">
            {effectiveTab === "overview" && <OverviewTab serverId={serverId} />}
            {effectiveTab === "members" && <MembersTab serverId={serverId} />}
            {effectiveTab === "roles" && <RolesTab serverId={serverId} />}
            {effectiveTab === "bans" && <BansTab serverId={serverId} />}
          </div>
        </div>
      </div>

      {/* Leave confirmation */}
      {leaveConfirm && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50"
          onClick={(e) => {
            e.stopPropagation();
            setLeaveConfirm(false);
          }}
        >
          <div
            className="w-full max-w-sm animate-[cardIn_0.2s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 font-display text-[16px] font-semibold text-text-primary">
              Leave this server?
            </h3>
            <p className="mb-5 text-[13px] leading-[1.55] text-text-secondary">
              You will need a new invite to rejoin.
            </p>
            <div className="flex gap-2.5">
              <button
                onClick={() => setLeaveConfirm(false)}
                className="flex-1 rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
              >
                Cancel
              </button>
              <button
                onClick={runLeave}
                className="flex-1 rounded-md bg-error py-2.5 text-[13px] font-semibold text-white transition-colors hover:bg-error/85"
              >
                Leave Server
              </button>
            </div>
          </div>
        </div>
      )}
    </div>,
    document.body,
  );
}
