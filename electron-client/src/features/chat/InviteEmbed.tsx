import { useEffect, useMemo, useState } from "react";
import { invoke } from "../../lib/ipc";
import { handleCertMismatch } from "../../lib/certMismatch";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useInviteResolveStore } from "../../stores/inviteResolveStore";
import { parseInviteLink } from "../servers/inviteLink";
import { useFetchServerPictureIfMissing } from "../servers/useServerPicture";
import { stringToGradient } from "../../utils/colors";

// The invite card under a message that links a `decibell://invite/…`
// URL — Discord's "You've been invited to join a server": picture,
// name, member count, description, and a Join button that redeems the
// invite right there (the card is the preview, so no second confirm;
// the plain link text still goes through DeepLinkJoinModal).
//
// The frame renders immediately from the link alone — host:port is
// enough to join — and fills in as central resolves the code
// (inviteResolveStore → resolve_invite_code). Central says unknown /
// expired → "Invalid invite", Join disabled. Central unreachable →
// host:port stays as the title and Join still works (the community
// checks the code itself).

/// Discord's 432px column.
const CARD_MAX_WIDTH_PX = 432;
/// Give up on "Joining…" if neither an auth response nor an error lands.
const JOIN_TIMEOUT_MS = 15_000;

export default function InviteEmbed({ href, sender }: { href: string; sender: string }) {
  const parsed = useMemo(() => parseInviteLink(href), [href]);
  const code = parsed ? parsed.code.toUpperCase() : "";
  const hostKey = parsed ? `${parsed.host}:${parsed.port}` : "";

  const entry = useInviteResolveStore((s) => (code ? s.entries[code] : undefined));
  useEffect(() => {
    if (code) useInviteResolveStore.getState().request(code);
  }, [code]);
  const resolved = entry?.status === "done" ? entry.resolved : null;
  const invalid = entry?.status === "done" && entry.invalid;
  const serverId = resolved && resolved.serverId > 0 ? String(resolved.serverId) : "";
  const pictureVersion = serverId ? resolved!.pictureVersion : "";

  // Register the version so the shared fetch path (and picture-changed
  // events) treat this server like any tile; no-op when unchanged.
  useEffect(() => {
    if (serverId && pictureVersion) {
      useChatStore.getState().setServerPictureVersion(serverId, pictureVersion);
    }
  }, [serverId, pictureVersion]);
  const pictureDataUrl = useChatStore((s) => (serverId ? s.serverPictures[serverId] : undefined));
  useFetchServerPictureIfMissing(serverId, pictureVersion, pictureDataUrl);

  const me = useAuthStore((s) => s.username);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const pendingIds = useChatStore((s) => s.pendingMembershipServerIds);
  const authError = useUiStore((s) => s.authError);
  // An invite join connects under "host:port" and is re-keyed onto the
  // central id once the auth response names it — check both.
  const joined =
    (serverId !== "" && connectedServers.has(serverId)) || connectedServers.has(hostKey);
  const rejoining = (serverId !== "" && pendingIds.has(serverId)) || pendingIds.has(hostKey);

  const [joining, setJoining] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The auth response settles the join one way or the other.
  useEffect(() => {
    if (!joining) return;
    if (joined) {
      setJoining(false);
      return;
    }
    if (authError && (authError.serverId === hostKey || authError.serverId === serverId)) {
      setError(authError.message || authError.errorCode || "Couldn't join");
      setJoining(false);
      return;
    }
    const t = window.setTimeout(() => setJoining(false), JOIN_TIMEOUT_MS);
    return () => window.clearTimeout(t);
  }, [joining, joined, authError, hostKey, serverId]);

  if (!parsed) return null;

  const join = async () => {
    if (joined || joining || invalid) return;
    setJoining(true);
    setError(null);
    useUiStore.getState().setAuthError(null);
    try {
      await invoke("redeem_invite", {
        serverId: hostKey,
        host: parsed.host,
        port: parsed.port,
        inviteCode: parsed.code,
      });
      useChatStore.getState().setActiveServer(hostKey);
      useUiStore.getState().setActiveView("server");
    } catch (err) {
      setJoining(false);
      if (!handleCertMismatch(err, join)) setError(String(err));
    }
  };

  const open = () => {
    const id = serverId && connectedServers.has(serverId) ? serverId : hostKey;
    useChatStore.getState().setActiveServer(id);
    useUiStore.getState().setActiveView("server");
  };

  const name = resolved?.serverName || "";
  const title = invalid
    ? "Invalid invite"
    : name || (entry && entry.status === "loading" ? "Resolving invite…" : hostKey);
  const description = invalid ? "This invite is unknown, expired, or used up." : resolved?.serverDescription ?? "";
  const members = resolved && resolved.memberCount > 0 ? resolved.memberCount : null;
  const label =
    me && sender === me
      ? "You sent an invite to join a server"
      : "You've been invited to join a server";
  const busy = joining || rejoining;

  return (
    <div className="mt-1 rounded-md bg-bg-dark p-4" style={{ maxWidth: CARD_MAX_WIDTH_PX }}>
      <div className="font-meta text-micro font-semibold uppercase tracking-section text-text-muted">
        {label}
      </div>
      <div className="mt-3 flex items-center gap-3">
        <div className="h-12 w-12 shrink-0 overflow-hidden rounded-xl bg-bg-secondary">
          {pictureDataUrl ? (
            <img src={pictureDataUrl} alt="" className="h-full w-full object-cover" draggable={false} />
          ) : (
            <div
              className="flex h-full w-full items-center justify-center text-lg font-semibold"
              style={{
                background: invalid ? undefined : stringToGradient(name || hostKey),
                color: "var(--color-av-fg)",
              }}
            >
              {invalid ? "?" : (name || parsed.host).charAt(0).toUpperCase()}
            </div>
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div
            className={`truncate text-body font-semibold ${
              invalid ? "text-text-muted" : "text-text-bright"
            }`}
          >
            {title}
          </div>
          {members !== null && !invalid && (
            <div className="mt-0.5 flex items-center gap-1.5 font-meta text-meta text-text-muted">
              <span className="inline-block h-2 w-2 rounded-full bg-text-muted" />
              {members} {members === 1 ? "member" : "members"}
            </div>
          )}
          {description && (
            <div className="mt-0.5 line-clamp-2 text-meta leading-body text-text-secondary [overflow-wrap:anywhere]">
              {description}
            </div>
          )}
        </div>
        {joined ? (
          <button
            type="button"
            onClick={open}
            title="Open server"
            className="shrink-0 rounded-sm bg-success px-4 py-2 text-[13px] font-semibold text-on-accent hover:opacity-90"
          >
            Joined
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void join()}
            disabled={busy || invalid}
            className="shrink-0 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy ? "Joining…" : "Join"}
          </button>
        )}
      </div>
      {error && <div className="mt-2 text-meta text-error">{error}</div>}
    </div>
  );
}
