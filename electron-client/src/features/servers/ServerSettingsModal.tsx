import { createPortal } from "react-dom";
import { useEffect, useMemo, useState, useCallback } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { PERM, usePermission } from "./permissions";
import OverviewTab from "./tabs/OverviewTab";
import MembersTab from "./tabs/MembersTab";
import RolesTab from "./tabs/RolesTab";
import BansTab from "./tabs/BansTab";
import AuditLogTab from "./tabs/AuditLogTab";
import StorageTab from "./tabs/StorageTab";
import { LeaveGuardContext } from "./leaveGuard";
import {
  SETTINGS_BACKDROP,
  SETTINGS_BODY,
  SETTINGS_FRAME,
  SETTINGS_PANE,
  SettingsNav,
  SettingsNavFoot,
  SettingsNavItem,
  SettingsNavLabel,
  SettingsPaneHeader,
} from "../../components/SettingsWindow";

interface Props {
  serverId: string;
}

/// Unified server-settings screen (Discord-style), in the shared settings
/// window (components/SettingsWindow.tsx: tabbed sidebar, an icon rail
/// below 1024 px, fade-in scale-95→1, Esc closes, backdrop click closes,
/// portal to body).
/// A tab with unsaved changes holds the window: closing it or changing
/// tab shakes that tab's save bar instead (leaveGuard.ts).
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
  const [tabDirty, setTabDirty] = useState(false);
  const [nudge, setNudge] = useState(0);

  const server = useChatStore((s) => s.servers.find((x) => x.id === serverId));
  const serverName = useChatStore(
    (s) => s.serverMeta[serverId]?.name ?? server?.name ?? "Server",
  );
  const owner = useChatStore((s) => s.serverOwner[serverId]);
  const currentUser = useAuthStore((s) => s.username);
  const isOwner = !!currentUser && !!owner && owner === currentUser;
  const canManageRoles = usePermission(serverId, PERM.MANAGE_ROLES);
  const canBan = usePermission(serverId, PERM.BAN_MEMBERS);
  const canAudit = usePermission(serverId, PERM.VIEW_AUDIT_LOG);
  const canManageServer = usePermission(serverId, PERM.MANAGE_SERVER);

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
    ...(canAudit
      ? [
          {
            id: "audit",
            label: "Audit Log",
            icon: (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                <polyline points="14 2 14 8 20 8" />
                <line x1="16" y1="13" x2="8" y2="13" />
                <line x1="16" y1="17" x2="8" y2="17" />
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
    ...(canManageServer
      ? [
          {
            id: "storage",
            label: "Storage",
            icon: (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <ellipse cx="12" cy="5" rx="9" ry="3" />
                <path d="M3 5v14a9 3 0 0 0 18 0V5" />
                <path d="M3 12a9 3 0 0 0 18 0" />
              </svg>
            ),
          },
        ]
      : []),
  ];

  useEffect(() => {
    if (isOpen) {
      setMounted(true);
      // Double rAF (matching SettingsModal): a single rAF fires before
      // the next paint, so the opacity-0/scale-95 starting frame never
      // renders and the open transition pops instead of animating.
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
    } else {
      setVisible(false);
      setLeaveConfirm(false);
    }
  }, [isOpen]);

  // Fresh roster + roles whenever the screen opens — every tab feeds
  // off these, and the pushed broadcasts keep them live afterwards.
  useEffect(() => {
    if (!isOpen) return;
    invoke("list_members", { serverId, after: "", limit: 100 }).catch(console.error);
    invoke("list_bans", { serverId }).catch(() => {});
    invoke("list_roles", { serverId }).catch(() => {});
  }, [isOpen, serverId]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible) setMounted(false);
  }, [visible]);

  // A nudge belongs to the changes it guarded; the next ones start calm.
  useEffect(() => {
    if (!tabDirty) setNudge(0);
  }, [tabDirty]);
  const guard = useMemo(() => ({ nudge, setDirty: setTabDirty }), [nudge]);

  /// Leave the open tab (close, or switch tabs), unless it has unsaved
  /// changes: then shake its save bar.
  const leaveTab = useCallback(
    (go: () => void) => {
      if (tabDirty) setNudge((n) => n + 1);
      else go();
    },
    [tabDirty],
  );

  useEffect(() => {
    if (!mounted) return;
    const onKey = (e: KeyboardEvent) => {
      // A dialog inside the window (ConfirmModal) took this Esc.
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (leaveConfirm) setLeaveConfirm(false);
      else leaveTab(closeModal);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mounted, leaveConfirm, leaveTab, closeModal]);

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
      className={SETTINGS_BACKDROP}
      style={{
        backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)",
      }}
      onClick={(e) => {
        // Guard: only close on a real backdrop click, not on clicks
        // bubbling up from nested overlays (cropper, confirm dialogs).
        if (e.target === e.currentTarget) leaveTab(closeModal);
      }}
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
          <SettingsNavLabel title={serverName}>{serverName}</SettingsNavLabel>
          {tabs.map((tab) => (
            <SettingsNavItem
              key={tab.id}
              icon={tab.icon}
              label={tab.label}
              active={effectiveTab === tab.id}
              onClick={() => tab.id !== effectiveTab && leaveTab(() => setActiveTab(tab.id))}
            />
          ))}

          {/* Leave server — pinned at the bottom, non-owners only (the
              owner can't leave their own server). */}
          {!isOwner && (
            <SettingsNavFoot>
              <SettingsNavItem
                danger
                label="Leave Server"
                onClick={() => setLeaveConfirm(true)}
                icon={
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                    <polyline points="16 17 21 12 16 7" />
                    <line x1="21" y1="12" x2="9" y2="12" />
                  </svg>
                }
              />
            </SettingsNavFoot>
          )}
        </SettingsNav>

        <div className={SETTINGS_PANE}>
          <SettingsPaneHeader title={activeTabDef.label} context={serverName} onClose={() => leaveTab(closeModal)} />
          <div className={SETTINGS_BODY}>
            <LeaveGuardContext.Provider value={guard}>
              {effectiveTab === "overview" && <OverviewTab serverId={serverId} />}
              {effectiveTab === "members" && <MembersTab serverId={serverId} />}
              {effectiveTab === "roles" && <RolesTab serverId={serverId} />}
              {effectiveTab === "bans" && <BansTab serverId={serverId} />}
              {effectiveTab === "audit" && <AuditLogTab serverId={serverId} />}
              {effectiveTab === "storage" && <StorageTab serverId={serverId} />}
            </LeaveGuardContext.Provider>
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
