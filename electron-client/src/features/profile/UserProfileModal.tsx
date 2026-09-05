import { useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useE2eeStore } from "../../stores/e2eeStore";
import { toast } from "../../stores/toastStore";
import { useDisplayName } from "../../hooks/useDisplayName";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import { usePeerE2ee } from "../e2ee/usePeerE2ee";
import { formatJoined } from "../servers/tabs/helpers";
import { stringToGradient } from "../../utils/colors";
import { UserAvatar } from "../../components/UserAvatar";
import { LockGlyph } from "../chat/MessageBubble";
import { RoleChips } from "./RoleChips";
import type { FriendInfo, ServerMember, ServerRole } from "../../types";

// The full profile screen: a centred modal reached by clicking the avatar
// in the anchored popup (UserProfilePopup), the way Discord's popout opens
// its full profile. Opened via useUiStore.openUserProfile(username,
// serverId?) and rides `activeModal === "user-profile"`.
//
// Two tabs. Profile: identity, friendship, membership and roles on the
// server it was opened from. Privacy: everything end-to-end encryption
// (the safety number to compare out of band, the peer's fingerprint, key
// changes, pin state) — moved here from the popup, where it took half the
// card. Our own profile shows our own fingerprint and points at Settings.

type Tab = "profile" | "privacy";

const TABS: { id: Tab; label: string }[] = [
  { id: "profile", label: "Profile" },
  { id: "privacy", label: "Privacy" },
];

// protobuf FriendActionType (see FriendActionButton / FriendsPage).
const FRIEND_ACTION = { ADD: 0, REMOVE: 1, ACCEPT: 3, REJECT: 4 } as const;

export default function UserProfileModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const username = useUiStore((s) => s.userProfileUser);
  const serverId = useUiStore((s) => s.userProfileServerId);
  const closeModal = useUiStore((s) => s.closeModal);
  const open = activeModal === "user-profile" && username !== null;
  useEscapeToClose(closeModal, open);
  if (!open || !username) return null;

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/65"
      onClick={closeModal}
    >
      {/* Keyed by user so a different profile starts on the Profile tab. */}
      <ProfileScreen key={username} username={username} serverId={serverId} onClose={closeModal} />
    </div>,
    document.body,
  );
}

function ProfileScreen({
  username,
  serverId,
  onClose,
}: {
  username: string;
  serverId: string | null;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<Tab>("profile");
  const currentUsername = useAuthStore((s) => s.username);
  const isSelf = username === currentUsername;
  const displayName = useDisplayName(serverId, username);
  // `find` returns the stored object (or undefined) — a stable ref.
  const friend = useFriendsStore((s) => s.friends.find((f) => f.username === username));
  const onlineUsers = useChatStore((s) => s.onlineUsers);
  const isOnline = friend?.status === "online" || onlineUsers.includes(username);
  const memberEntry = useChatStore((s) =>
    serverId ? s.membersByServer[serverId]?.find((m) => m.username === username) : undefined,
  );
  const serverRoles = useChatStore((s) => (serverId ? s.rolesByServer[serverId] : undefined));
  const serverName = useChatStore((s) =>
    serverId ? s.servers.find((sv) => sv.id === serverId)?.name : undefined,
  );

  const openMessage = () => {
    useDmStore.getState().setActiveDmUser(username);
    useUiStore.getState().setActiveView("dm");
    onClose();
  };
  // Replaces this modal (one activeModal at a time) — intended: the
  // encryption controls and the avatar editor live there.
  const openSettings = (settingsTab: string) => {
    useUiStore.getState().setSettingsTab(settingsTab);
    useUiStore.getState().openModal("settings");
  };
  // A pending request either way, or a block, is not a conversation yet
  // (the friends page draws the same line).
  const canMessage =
    !isSelf && (!friend || friend.status === "online" || friend.status === "offline");

  return (
    <div
      className="flex h-[560px] w-[640px] max-w-[calc(100vw-32px)] flex-col overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal animate-[cardIn_0.2s_ease]"
      onClick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label={`${displayName}'s profile`}
    >
      {/* Banner */}
      <div className="relative h-[120px] shrink-0" style={{ background: stringToGradient(username) }}>
        <button
          onClick={onClose}
          aria-label="Close"
          className="absolute right-3 top-3 flex h-7 w-7 items-center justify-center rounded-sm bg-black/30 text-white/80 transition-colors hover:bg-black/50 hover:text-white"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <line x1="18" y1="6" x2="6" y2="18" />
            <line x1="6" y1="6" x2="18" y2="18" />
          </svg>
        </button>
      </div>

      {/* Identity row: avatar overlapping the banner, actions on the right */}
      <div className="relative shrink-0 px-6">
        <div className="absolute -top-12 left-6 rounded-2xl border-[6px] border-bg-dark bg-bg-dark">
          <UserAvatar username={username} size={96} />
          <div
            className={`absolute -bottom-0.5 -right-0.5 h-5 w-5 rounded-full border-[4px] border-bg-dark ${
              isOnline ? "bg-success" : "bg-text-muted"
            }`}
          />
        </div>
        <div className="flex min-h-[56px] items-start justify-end gap-2 pt-3">
          {isSelf ? (
            <SecondaryButton onClick={() => openSettings("account")}>Edit profile</SecondaryButton>
          ) : (
            <>
              <FriendActions username={username} friend={friend} />
              {canMessage && <PrimaryButton onClick={openMessage}>Message</PrimaryButton>}
            </>
          )}
        </div>
        <div className="pb-4 pt-1">
          <div className="font-display text-[20px] font-semibold leading-tight text-text-primary">
            {displayName}
          </div>
          {displayName !== username && (
            <div className="mt-0.5 text-[13px] text-text-muted">{username}</div>
          )}
          <div className="mt-2">
            <StatusPill online={isOnline} />
          </div>
        </div>
      </div>

      {/* Tabs */}
      <div className="flex shrink-0 gap-1 border-b border-border-divider px-6">
        {TABS.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={`-mb-px border-b-2 px-3 py-2.5 text-[13px] font-medium transition-colors ${
              tab === t.id
                ? "border-accent text-text-primary"
                : "border-transparent text-text-secondary hover:text-text-primary"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="flex-1 overflow-y-auto px-6 py-5 scrollbar-thin">
        {tab === "profile" ? (
          <ProfileTab
            username={username}
            isSelf={isSelf}
            isOnline={isOnline}
            friend={friend}
            memberEntry={memberEntry}
            serverRoles={serverRoles}
            serverId={serverId}
            serverName={serverName}
          />
        ) : isSelf ? (
          <OwnEncryptionCard onOpenSettings={() => openSettings("privacy")} />
        ) : (
          <PeerEncryption peer={username} onOpenSettings={() => openSettings("privacy")} />
        )}
      </div>
    </div>
  );
}

// ── Profile tab ──────────────────────────────────────────────────────

const FRIEND_LABEL: Record<FriendInfo["status"], string> = {
  online: "Friends",
  offline: "Friends",
  pending_incoming: "Wants to be friends",
  pending_outgoing: "Request sent",
  blocked: "Blocked",
};

function ProfileTab({
  username,
  isSelf,
  isOnline,
  friend,
  memberEntry,
  serverRoles,
  serverId,
  serverName,
}: {
  username: string;
  isSelf: boolean;
  isOnline: boolean;
  friend: FriendInfo | undefined;
  memberEntry: ServerMember | undefined;
  serverRoles: ServerRole[] | undefined;
  serverId: string | null;
  serverName: string | undefined;
}) {
  const nickname =
    memberEntry?.nickname && memberEntry.nickname !== username ? memberEntry.nickname : null;
  return (
    <div className="flex flex-col gap-3">
      <Card title="About">
        <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-[13px]">
          <Row label="Username" value={username} />
          {nickname && <Row label="Nickname" value={nickname} />}
          <Row label="Status" value={isOnline ? "Online" : "Offline"} />
          {!isSelf && <Row label="Friends" value={friend ? FRIEND_LABEL[friend.status] : "Not friends"} />}
          {memberEntry && memberEntry.joinedAt > 0 && (
            <Row
              label={serverName ? `Joined ${serverName}` : "Joined server"}
              value={formatJoined(memberEntry.joinedAt)}
            />
          )}
        </dl>
      </Card>
      {serverId && (
        <Card title={serverName ? `Roles in ${serverName}` : "Roles"}>
          <RoleChips
            roles={serverRoles}
            roleIds={memberEntry?.roleIds ?? []}
            isOwner={!!memberEntry?.isOwner}
          />
        </Card>
      )}
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-text-muted">{label}</dt>
      <dd className="min-w-0 select-text truncate text-text-primary">{value}</dd>
    </>
  );
}

function FriendActions({ username, friend }: { username: string; friend: FriendInfo | undefined }) {
  const [busy, setBusy] = useState(false);
  const act = async (action: number) => {
    setBusy(true);
    try {
      await invoke("send_friend_action", { action, targetUsername: username });
    } catch (err) {
      toast.error("Friend request failed", String(err));
    } finally {
      setBusy(false);
    }
  };
  switch (friend?.status) {
    case "pending_incoming":
      return (
        <>
          <SecondaryButton disabled={busy} onClick={() => act(FRIEND_ACTION.REJECT)}>
            Decline
          </SecondaryButton>
          <PrimaryButton disabled={busy} onClick={() => act(FRIEND_ACTION.ACCEPT)}>
            Accept
          </PrimaryButton>
        </>
      );
    case "pending_outgoing":
      return (
        <SecondaryButton disabled={busy} onClick={() => act(FRIEND_ACTION.REMOVE)}>
          Cancel request
        </SecondaryButton>
      );
    case "blocked":
      return (
        <SecondaryButton disabled={busy} onClick={() => act(FRIEND_ACTION.REMOVE)}>
          Unblock
        </SecondaryButton>
      );
    case "online":
    case "offline":
      return (
        <SecondaryButton disabled={busy} onClick={() => act(FRIEND_ACTION.REMOVE)}>
          Remove friend
        </SecondaryButton>
      );
    default:
      return (
        <SecondaryButton disabled={busy} onClick={() => act(FRIEND_ACTION.ADD)}>
          Add friend
        </SecondaryButton>
      );
  }
}

// ── Privacy tab ──────────────────────────────────────────────────────

function OwnEncryptionCard({ onOpenSettings }: { onOpenSettings: () => void }) {
  const supported = useE2eeStore((s) => s.supported);
  const status = useE2eeStore((s) => s.status);
  const fingerprint = useE2eeStore((s) => s.fingerprint);

  let badge: Badge = null;
  let description: string;
  if (!supported || status === "unavailable") {
    description = "Your central server doesn't offer end-to-end encrypted direct messages yet.";
  } else if (status === "not_set_up") {
    badge = { text: "Off", on: false };
    description =
      "Your direct messages aren't encrypted yet. Set up encryption in Settings to seal them on your device.";
  } else if (status === "locked") {
    badge = { text: "Locked", on: false };
    description =
      "Your keys are backed up, but this device hasn't unlocked them. Enter your passphrase in Settings to read and send encrypted messages here.";
  } else {
    badge = { text: "On", on: true };
    description =
      "Messages with anyone who has also set up encryption are sealed on your device; the server only ever stores encrypted data. Others compare the fingerprint below against what their app shows for you.";
  }

  return (
    <div className="flex flex-col gap-3">
      <Card title="End-to-end encryption" badge={badge} icon={<LockGlyph size={14} />}>
        <p className="text-[12px] leading-[1.55] text-text-muted">{description}</p>
        {status === "ready" && fingerprint && <Mono label="Your fingerprint" value={fingerprint} />}
        {supported && status !== "unavailable" && (
          <div className="mt-3 flex justify-end">
            <SecondaryButton onClick={onOpenSettings}>Manage in Settings</SecondaryButton>
          </div>
        )}
      </Card>
    </div>
  );
}

function PeerEncryption({ peer, onOpenSettings }: { peer: string; onOpenSettings: () => void }) {
  const supported = useE2eeStore((s) => s.supported);
  const status = useE2eeStore((s) => s.status);
  // The safety number needs both identities: only asked for once ours is
  // unlocked. usePeerE2ee re-fetches on our status and on key changes.
  const info = usePeerE2ee(status === "ready" ? peer : null);

  if (!supported || status === "unavailable") {
    return (
      <Card title="End-to-end encryption" icon={<LockGlyph size={14} />}>
        <p className="text-[12px] leading-[1.55] text-text-muted">
          Your central server doesn't offer end-to-end encrypted direct messages yet, so there is
          no safety number to compare.
        </p>
      </Card>
    );
  }
  if (status !== "ready") {
    const locked = status === "locked";
    return (
      <Card
        title="End-to-end encryption"
        badge={{ text: locked ? "Locked" : "Off", on: false }}
        icon={<LockGlyph size={14} />}
      >
        <p className="text-[12px] leading-[1.55] text-text-muted">
          {locked
            ? `Unlock your keys on this device to see the safety number you share with ${peer}.`
            : `Set up encryption to seal your messages with ${peer} and get a safety number you can compare.`}
        </p>
        <div className="mt-3 flex justify-end">
          <SecondaryButton onClick={onOpenSettings}>
            {locked ? "Unlock in Settings" : "Set up in Settings"}
          </SecondaryButton>
        </div>
      </Card>
    );
  }
  if (!info) {
    return (
      <Card title="End-to-end encryption" icon={<LockGlyph size={14} />}>
        <p className="text-[12px] text-text-muted">Checking {peer}'s keys…</p>
      </Card>
    );
  }
  if (!info.hasKeys) {
    return (
      <Card title="End-to-end encryption" badge={{ text: "Off", on: false }} icon={<LockGlyph size={14} />}>
        <p className="text-[12px] leading-[1.55] text-text-muted">
          {peer} hasn't set up encryption. Messages between you are sent in plaintext, and the
          conversation says so.
        </p>
      </Card>
    );
  }

  const copySafetyNumber = () => {
    navigator.clipboard.writeText(info.safetyNumber).then(
      () => toast.success("Safety number copied"),
      (err) => toast.error("Copy failed", String(err)),
    );
  };

  return (
    <div className="flex flex-col gap-3">
      <Card title="End-to-end encryption" badge={{ text: "On", on: true }} icon={<LockGlyph size={14} />}>
        <p className="text-[12px] leading-[1.55] text-text-muted">
          Messages between you and {peer} are sealed on your devices; the server only ever stores
          encrypted data.
        </p>
        {info.changedAt > 0 && (
          <div className="mt-2.5 rounded-sm border border-warning/40 bg-warning/10 px-3 py-2 text-[12px] leading-[1.5] text-warning">
            Keys changed {new Date(info.changedAt * 1000).toLocaleString()}. {peer} reset their
            encryption or signed in from a new device. Compare the safety number again before you
            trust what they send.
          </div>
        )}
        <p className="mt-2.5 text-[12px] leading-[1.55] text-text-muted">
          {info.pinned
            ? `Identity pinned on this device: messages to ${peer} are never sent unencrypted, and a different key is flagged.`
            : `Not pinned yet: the identity is pinned the first time an encrypted message passes between you.`}
        </p>
      </Card>

      <Card
        title="Safety number"
        action={
          <button
            onClick={copySafetyNumber}
            className="text-[11px] font-medium text-accent-bright transition-colors hover:text-accent"
          >
            Copy
          </button>
        }
      >
        <p className="text-[12px] leading-[1.55] text-text-muted">
          Identical on both your screens. Compare it with {peer} in person or on a call; a match
          means nobody is sitting between you.
        </p>
        <div className="mt-2.5 grid select-text grid-cols-4 gap-x-4 gap-y-1.5 rounded-sm bg-bg-dark px-4 py-3 text-center font-mono text-[14px] tracking-[0.08em] text-text-primary">
          {info.safetyNumber.split(" ").map((group, i) => (
            <span key={i}>{group}</span>
          ))}
        </div>
      </Card>

      <Card title="Fingerprint">
        <p className="text-[12px] leading-[1.55] text-text-muted">
          What {peer}'s own app shows under Settings → Privacy. Comparing fingerprints is the same
          check as the safety number, one identity at a time.
        </p>
        <Mono label={`${peer}'s fingerprint`} value={info.fingerprint} />
      </Card>
    </div>
  );
}

// ── Shared bits ──────────────────────────────────────────────────────

type Badge = { text: string; on: boolean } | null;

function Card({
  title,
  badge,
  icon,
  action,
  children,
}: {
  title: string;
  badge?: Badge;
  icon?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="rounded-md border border-border-divider bg-bg-light px-4 py-3.5">
      <div className="mb-2.5 flex items-center gap-2 text-[14px] font-medium text-text-primary">
        {icon && <span className="text-text-secondary">{icon}</span>}
        <span className="min-w-0 truncate">{title}</span>
        {badge && (
          <span
            className={`rounded-sm px-1.5 py-px font-channel text-[10px] font-semibold uppercase tracking-[0.06em] ${
              badge.on ? "bg-success/15 text-success" : "bg-text-muted/15 text-text-muted"
            }`}
          >
            {badge.text}
          </span>
        )}
        {action && <span className="ml-auto">{action}</span>}
      </div>
      {children}
    </section>
  );
}

function Mono({ label, value }: { label: string; value: string }) {
  return (
    <div className="mt-2.5">
      <div className="text-[10px] font-semibold uppercase tracking-[0.07em] text-text-muted">{label}</div>
      <div className="mt-1 select-text break-all font-mono text-[11px] tracking-[0.06em] text-text-primary">
        {value}
      </div>
    </div>
  );
}

function StatusPill({ online }: { online: boolean }) {
  return online ? (
    <span className="inline-flex items-center gap-[5px] rounded-sm bg-success/15 px-2 py-0.5 text-[11px] font-medium text-success">
      <span className="h-1.5 w-1.5 rounded-full bg-success" />
      Online
    </span>
  ) : (
    <span className="inline-flex items-center gap-[5px] rounded-sm bg-text-muted/15 px-2 py-0.5 text-[11px] font-medium text-text-muted">
      <span className="h-1.5 w-1.5 rounded-full bg-text-muted" />
      Offline
    </span>
  );
}

function PrimaryButton({
  children,
  onClick,
  disabled,
}: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-hover disabled:opacity-50"
    >
      {children}
    </button>
  );
}

function SecondaryButton({
  children,
  onClick,
  disabled,
}: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="rounded-sm border border-border px-4 py-2 text-[13px] font-medium text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
    >
      {children}
    </button>
  );
}
