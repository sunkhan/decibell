import { createPortal } from "react-dom";
import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import ConfirmModal from "../../components/ConfirmModal";
import { PERM, useChannelPermission } from "./permissions";
import type { ChannelInfo } from "../../types";
import { Icon, SaveBar, type IconName } from "./tabs/settingsUi";
import { channelIcon } from "./channelSettings/ui";
import { changesOf, draftOf, type ChannelDraft } from "./channelSettings/draft";
import OverviewTab from "./channelSettings/OverviewTab";
import RetentionTab from "./channelSettings/RetentionTab";
import PermissionsTab, { usePermissionDraft } from "./channelSettings/PermissionsTab";
import { useEchoSave } from "./saveEcho";
import {
  SETTINGS_BACKDROP,
  SETTINGS_BODY,
  SETTINGS_FRAME,
  SETTINGS_PANE,
  SettingsNav,
  SettingsNavFoot,
  SettingsNavItem,
  SettingsPaneHeader,
} from "../../components/SettingsWindow";

type Tab = "overview" | "retention" | "permissions";

const TABS: Record<Tab, { label: string; icon: IconName }> = {
  overview: { label: "Overview", icon: "sliders" },
  retention: { label: "Retention", icon: "clock" },
  permissions: { label: "Permissions", icon: "shield" },
};

const TYPE_LABEL: Record<ChannelInfo["type"], string> = {
  text: "Text channel",
  voice: "Voice channel",
  category: "Category",
  unknown: "Channel",
};

/// Channel and category settings, in the shared settings window
/// (components/SettingsWindow.tsx: sidebar of tabs, an icon rail below
/// 1024 px, 300ms fade + scale, portal to body). Overview,
/// Retention and Permissions edits are one draft; the save bar commits
/// all of it, and leaving with unsaved changes shakes the bar instead of
/// dropping them. Delete sits at the sidebar's foot, like Leave Server.
export default function ChannelSettingsModal() {
  const isOpen = useUiStore((s) => s.activeModal === "channel-settings");
  const closeModal = useUiStore((s) => s.closeModal);
  const activeServerId = useChatStore((s) => s.activeServerId);
  // Explicit target set by the per-row gear icon: the channel needn't be
  // the active one.
  const targetChannelId = useUiStore((s) => s.channelSettingsChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const channel: ChannelInfo | undefined = useMemo(() => {
    if (!activeServerId || !targetChannelId) return undefined;
    return channelsByServer[activeServerId]?.find((c) => c.id === targetChannelId);
  }, [activeServerId, targetChannelId, channelsByServer]);
  const serverName = useChatStore((s) =>
    activeServerId ? s.serverMeta[activeServerId]?.name ?? s.servers.find((x) => x.id === activeServerId)?.name : undefined,
  );

  // Server-side gates: rename / retention / slowmode / bitrate / topic /
  // wipe / delete are MANAGE_CHANNELS in this channel; overwrites are
  // MANAGE_ROLES in this channel.
  const canManage = useChannelPermission(activeServerId, targetChannelId, PERM.MANAGE_CHANNELS);
  const canRoles = useChannelPermission(activeServerId, targetChannelId, PERM.MANAGE_ROLES);
  const isText = channel?.type === "text";
  // The landing channel after auth must always exist: the server refuses
  // to delete the last text channel, so don't offer it.
  const isLastTextChannel =
    !!channel && isText && (channelsByServer[activeServerId ?? ""] ?? []).filter((c) => c.type === "text").length <= 1;

  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [tab, setTab] = useState<Tab>("overview");
  const [draft, setDraft] = useState<ChannelDraft | null>(null);
  const perms = usePermissionDraft(activeServerId ?? "", channel);
  const [nudge, setNudge] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<"wipe" | "delete" | null>(null);
  const [working, setWorking] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setMounted(true);
      // Double rAF: the opacity-0/scale-95 frame must paint first, or the
      // open pops instead of transitioning.
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
    } else {
      setVisible(false);
    }
  }, [isOpen]);

  // Fresh state whenever the window opens or points at another channel.
  // It stays mounted across opens, so every transient flag resets here.
  useEffect(() => {
    if (!isOpen || !channel) return;
    setDraft(null);
    perms.reset();
    setTab(canManage || !canRoles ? "overview" : "permissions");
    setNudge(0);
    stopSave();
    setError(null);
    setConfirm(null);
    setWorking(false);
  }, [isOpen, channel?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // CHANNEL_WIPE_RES → toast. The CHANNEL_WIPED broadcast that follows
  // clears the local message store via useChatEvents.
  useEffect(() => {
    if (!isOpen) return;
    const unlisten = listen<{
      serverId: string;
      channelId: string;
      success: boolean;
      message: string;
      deletedMessageCount: number;
      deletedAttachmentCount: number;
    }>("channel_wipe_responded", (event) => {
      if (event.payload.serverId !== activeServerId || event.payload.channelId !== channel?.id) return;
      setWorking(false);
      if (event.payload.success) {
        toast.success(
          "Channel history wiped",
          `Removed ${event.payload.deletedMessageCount.toLocaleString()} message(s) and ${event.payload.deletedAttachmentCount.toLocaleString()} attachment(s).`,
        );
        setConfirm(null);
      } else {
        toast.error("Wipe failed", event.payload.message || "Unknown error.");
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [isOpen, activeServerId, channel?.id]);

  const fields = draft ?? (channel ? draftOf(channel) : null);
  const changes = channel && fields ? changesOf(channel, fields) : null;
  const settingsDirty = !!changes?.any && canManage;
  const dirty = settingsDirty || perms.dirty;

  // After Save the drafts stay until the server's update matches them, so
  // the fields never flick back to the old values in between (saveEcho.ts).
  const { saving, start: startSave, stop: stopSave } = useEchoSave(
    () => !changes?.any && !perms.dirty,
    () => setDraft(null),
  );

  // A refusal ends the wait at once and leaves the draft. Renames and
  // overwrites answer with channel_action_responded (toasted globally);
  // the settings update answers with channel_updated, which nothing else
  // surfaces. Listening for as long as the window is open, so a quick
  // reply can't beat the listener.
  useEffect(() => {
    if (!isOpen) return;
    const unAction = listen<{ serverId: string; success: boolean; action: string }>(
      "channel_action_responded",
      (event) => {
        if (event.payload.serverId === activeServerId && !event.payload.success) stopSave();
      },
    );
    const unUpdated = listen<{ serverId: string; success: boolean; message: string }>("channel_updated", (event) => {
      if (event.payload.serverId !== activeServerId || event.payload.success) return;
      setError(event.payload.message || "Couldn't save the channel settings.");
      stopSave();
    });
    return () => {
      unAction.then((fn) => fn());
      unUpdated.then((fn) => fn());
    };
  }, [isOpen, activeServerId, stopSave]);

  // A nudge belongs to the changes it guarded; the next ones start calm.
  useEffect(() => {
    if (!dirty) setNudge(0);
  }, [dirty]);

  /// Close, unless there are unsaved changes: then shake the save bar.
  const requestClose = useCallback(() => {
    if (dirty) setNudge((n) => n + 1);
    else closeModal();
  }, [dirty, closeModal]);

  useEffect(() => {
    if (!mounted || confirm) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") requestClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mounted, confirm, requestClose]);

  if (!mounted || !channel || !activeServerId || !fields) return null;

  const tabs: Tab[] = [
    "overview",
    ...(isText ? (["retention"] as Tab[]) : []),
    ...(canManage || canRoles ? (["permissions"] as Tab[]) : []),
  ];
  const active = tabs.includes(tab) ? tab : "overview";
  const tabDirty: Record<Tab, boolean> = {
    overview: canManage && !!changes && (changes.name || changes.topic || changes.slowmode || changes.bitrate),
    retention: canManage && !!changes?.retention,
    permissions: perms.dirty,
  };
  const change = (patch: Partial<ChannelDraft>) => setDraft({ ...fields, ...patch });

  const reset = () => {
    setDraft(null);
    perms.reset();
    setError(null);
  };

  const save = async () => {
    if (!dirty || !changes || saving) return;
    startSave(true);
    setError(null);
    try {
      if (settingsDirty && changes.name) {
        await invoke("rename_channel", { serverId: activeServerId, channelId: channel.id, name: fields.name.trim() });
      }
      if (settingsDirty && (changes.retention || changes.topic || changes.bitrate || changes.slowmode)) {
        await invoke("update_channel_retention", {
          serverId: activeServerId,
          channelId: channel.id,
          ...fields.retention,
          // Explicit presence on the wire: undefined = leave unchanged.
          voiceBitrateKbps: channel.type === "voice" ? fields.bitrate : undefined,
          slowmodeSeconds: changes.slowmode ? fields.slowmode : undefined,
          topic: changes.topic ? fields.topic.trim() : undefined,
        });
      }
      if (perms.dirty) await perms.commit();
    } catch (err) {
      setError(String(err));
      stopSave();
    }
  };

  const runConfirm = async () => {
    if (working || !confirm) return;
    setWorking(true);
    try {
      if (confirm === "wipe") {
        await invoke("wipe_channel_history", { serverId: activeServerId, channelId: channel.id });
        // channel_wipe_responded resets `working` and closes the dialog.
      } else {
        await invoke("delete_channel", { serverId: activeServerId, channelId: channel.id });
        // Success is the CHANNEL_LIST_UPDATE broadcast; denials come back
        // as the global channel_action_responded toast.
        setWorking(false);
        setConfirm(null);
        closeModal();
      }
    } catch (err) {
      setWorking(false);
      toast.error(confirm === "wipe" ? "Wipe failed" : "Delete failed", String(err));
    }
  };

  const prefix = isText ? "#" : "";

  return createPortal(
    <div
      className={SETTINGS_BACKDROP}
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={(e) => {
        // Only a real backdrop click, not one bubbling from a nested
        // overlay (the confirm dialog, a popover).
        if (e.target === e.currentTarget) requestClose();
      }}
      onTransitionEnd={(e) => {
        if (e.target === e.currentTarget && !visible) setMounted(false);
      }}
    >
      <div
        className={SETTINGS_FRAME}
        style={{ opacity: visible ? 1 : 0, transform: visible ? "scale(1)" : "scale(0.95)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <SettingsNav>
          <div className="mb-3 flex items-center gap-2.5 px-3 max-lg:justify-center max-lg:px-0" title={channel.name}>
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-bg-light text-text-secondary shadow-raised">
              <Icon name={channelIcon(channel.type)} size={15} />
            </span>
            <div className="min-w-0 flex-1 max-lg:hidden">
              <div className="flex items-center gap-1.5">
                <span className="truncate text-[14px] font-semibold text-text-primary">{channel.name}</span>
                {channel.encrypted && <Icon name="lock" size={12} className="text-success" />}
              </div>
              <div className="truncate text-[12px] text-text-muted">
                {TYPE_LABEL[channel.type]}
                {serverName ? ` · ${serverName}` : ""}
              </div>
            </div>
          </div>
          {tabs.map((id) => (
            <SettingsNavItem
              key={id}
              icon={<Icon name={TABS[id].icon} size={16} />}
              label={TABS[id].label}
              active={active === id}
              dot={tabDirty[id]}
              onClick={() => setTab(id)}
            />
          ))}

          {canManage && !isLastTextChannel && (
            <SettingsNavFoot>
              <SettingsNavItem
                danger
                icon={<Icon name="trash" size={16} />}
                label={channel.type === "category" ? "Delete Category" : "Delete Channel"}
                onClick={() => setConfirm("delete")}
              />
            </SettingsNavFoot>
          )}
        </SettingsNav>

        <div className={SETTINGS_PANE}>
          <SettingsPaneHeader title={TABS[active].label} context={`${prefix}${channel.name}`} onClose={requestClose} />
          <div className={SETTINGS_BODY}>
            {error && (
              <div className="mb-6 flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3 py-2.5 text-[13px] text-error">
                <Icon name="alert" size={15} />
                {error}
              </div>
            )}
            {active === "overview" && (
              <OverviewTab channel={channel} draft={fields} change={change} editable={canManage} />
            )}
            {active === "retention" && (
              <RetentionTab
                channel={channel}
                draft={fields}
                change={change}
                editable={canManage}
                onWipe={() => setConfirm("wipe")}
              />
            )}
            {active === "permissions" && (
              <PermissionsTab serverId={activeServerId} channel={channel} draft={perms} />
            )}

            {(canManage || canRoles) && (
              <SaveBar
                show={dirty}
                nudge={nudge}
                saving={saving}
                onReset={reset}
                onSave={save}
                nudgeMessage="Save or reset your changes before closing."
              />
            )}
          </div>
        </div>
      </div>

      <ConfirmModal
        open={!!confirm}
        title={
          confirm === "wipe"
            ? `Wipe #${channel.name}?`
            : `Delete ${prefix}${channel.name}?`
        }
        confirmLabel={
          working
            ? confirm === "wipe" ? "Wiping…" : "Deleting…"
            : confirm === "wipe" ? "Wipe history" : channel.type === "category" ? "Delete category" : "Delete channel"
        }
        confirmDisabled={working}
        onConfirm={runConfirm}
        onCancel={() => !working && setConfirm(null)}
      >
        <p className="mt-2 text-[13px] leading-[1.55] text-text-secondary">
          {confirm === "wipe"
            ? "Every message and attachment in this channel is deleted for everyone. The channel, its settings and permissions stay. This can't be undone."
            : channel.type === "category"
              ? `Its channels stay and move up to the group above${channel.categorySync ? ", keeping the permissions they follow" : ""}.`
              : channel.type === "voice"
                ? "The channel must be empty first. This can't be undone."
                : "Every message and attachment in it goes too. This can't be undone."}
        </p>
      </ConfirmModal>
    </div>,
    document.body,
  );
}
