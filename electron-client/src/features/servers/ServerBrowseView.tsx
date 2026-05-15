import { useState, useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";
import type { CommunityServer } from "../../types";

interface ParsedInviteLink {
  host: string;
  port: number;
  code: string;
}

interface ResolvedInvite {
  host: string;
  port: number;
  code: string;
}

// Dedupes in-flight picture fetches across re-renders. Keyed by
// "<serverId>:<version>" so a new version triggers a fresh fetch.
// Module-level so it survives component remounts within a session.
const inflightFetches = new Set<string>();

function useFetchServerPictureIfMissing(
  serverId: string,
  version: string,
  cachedDataUrl: string | undefined,
) {
  useEffect(() => {
    if (!version || cachedDataUrl) return;
    const key = `${serverId}:${version}`;
    if (inflightFetches.has(key)) return;
    inflightFetches.add(key);
    invoke("fetch_server_picture", { serverId: parseInt(serverId, 10) })
      .catch(console.error)
      .finally(() => inflightFetches.delete(key));
  }, [serverId, version, cachedDataUrl]);
}

interface ServerBrowseCardProps {
  server: CommunityServer;
  isConnected: boolean;
  isConnecting: boolean;
  onConnect: () => void;
}

function ServerBrowseCard({
  server,
  isConnected,
  isConnecting,
  onConnect,
}: ServerBrowseCardProps) {
  const pictureVersion = useChatStore(
    (s) => s.serverPictureVersions[server.id] ?? "",
  );
  const pictureDataUrl = useChatStore((s) => s.serverPictures[server.id]);
  const hasPicture = pictureVersion !== "";
  useFetchServerPictureIfMissing(server.id, pictureVersion, pictureDataUrl);

  return (
    <button
      onClick={() => !isConnected && onConnect()}
      disabled={isConnected || isConnecting}
      className="flex flex-col overflow-hidden rounded-2xl border border-border bg-bg-dark text-left transition-all hover:border-accent/40 hover:-translate-y-0.5 disabled:opacity-60 disabled:hover:translate-y-0"
    >
      {/* Banner image (wide rectangle, fixed height) */}
      {hasPicture ? (
        <img
          src={pictureDataUrl ?? ""}
          alt={server.name}
          className="h-24 w-full object-cover"
        />
      ) : (
        <div
          className="flex h-24 w-full items-center justify-center text-4xl font-bold text-white"
          style={{ background: stringToGradient(server.name) }}
        >
          {server.name.charAt(0).toUpperCase()}
        </div>
      )}
      {/* Info block */}
      <div className="flex flex-1 flex-col gap-1 p-4">
        <div className="flex items-start justify-between gap-2">
          <span className="truncate text-sm font-bold text-text-bright">
            {server.name}
          </span>
          {isConnecting && (
            <svg
              className="h-4 w-4 shrink-0 animate-spin text-accent"
              viewBox="0 0 24 24"
              fill="none"
            >
              <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" className="opacity-25" />
              <path d="M4 12a8 8 0 018-8" stroke="currentColor" strokeWidth="3" strokeLinecap="round" className="opacity-75" />
            </svg>
          )}
        </div>
        <span className="line-clamp-2 text-xs text-text-secondary">
          {server.description}
        </span>
        <span className="mt-0.5 text-xs text-text-muted">
          {server.memberCount} members
          {isConnected && (
            <span className="ml-1 text-success">· Connected</span>
          )}
        </span>
      </div>
    </button>
  );
}

export default function ServerBrowseView() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const authError = useUiStore((s) => s.authError);
  const setAuthError = useUiStore((s) => s.setAuthError);

  const [search, setSearch] = useState("");
  const [isLoadingList, setIsLoadingList] = useState(false);
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [inviteInput, setInviteInput] = useState("");
  const [inviteHost, setInviteHost] = useState("");
  const [invitePort, setInvitePort] = useState("");
  const [redeeming, setRedeeming] = useState(false);
  const [inviteError, setInviteError] = useState<string | null>(null);
  const [redeemTarget, setRedeemTarget] = useState<string | null>(null);
  const [showManualHost, setShowManualHost] = useState(false);

  // Surface backend auth failures from an in-progress invite redemption.
  useEffect(() => {
    if (!redeemTarget || !authError) return;
    if (authError.serverId === redeemTarget) {
      setInviteError(authError.message || authError.errorCode);
      setRedeeming(false);
      setRedeemTarget(null);
    }
  }, [authError, redeemTarget]);

  useEffect(() => {
    setIsLoadingList(true);
    invoke("request_server_list")
      .catch(console.error)
      .finally(() => setIsLoadingList(false));
  }, []);

  const filtered = servers.filter(
    (s) =>
      s.name.toLowerCase().includes(search.toLowerCase()) ||
      s.description.toLowerCase().includes(search.toLowerCase()),
  );

  const handleConnect = async (serverId: string, host: string, port: number) => {
    setConnectingId(serverId);
    setError(null);
    try {
      await invoke("connect_to_community", { serverId, host, port });
      setActiveServer(serverId);
      setActiveView("server");
    } catch (err) {
      setError(String(err));
    } finally {
      setConnectingId(null);
    }
  };

  const handleRedeemInvite = async () => {
    const trimmed = inviteInput.trim();
    if (!trimmed) {
      setInviteError("Enter an invite code or link");
      return;
    }
    setInviteError(null);
    setAuthError(null);

    let host = inviteHost.trim();
    let port = parseInt(invitePort, 10);
    let code = trimmed;
    const isLink = trimmed.toLowerCase().startsWith("decibell:");
    const hasManualHost = host.length > 0 && !isNaN(port) && port > 0;

    setRedeeming(true);
    try {
      if (isLink) {
        const parsed = await invoke<ParsedInviteLink>("parse_invite_link", {
          url: trimmed,
        });
        host = parsed.host;
        port = parsed.port;
        code = parsed.code;
      } else if (!hasManualHost) {
        try {
          const resolved = await invoke<ResolvedInvite>("resolve_invite_code", {
            code: trimmed,
          });
          host = resolved.host;
          port = resolved.port;
          code = resolved.code;
        } catch (err) {
          setInviteError(
            `${err}. Enter the server's host and port, or paste a full decibell:// link.`,
          );
          setRedeeming(false);
          return;
        }
      }

      const serverId = `${host}:${port}`;
      setRedeemTarget(serverId);
      await invoke("redeem_invite", {
        serverId,
        host,
        port,
        inviteCode: code,
      });
      setActiveServer(serverId);
      setActiveView("server");
      setInviteInput("");
      setInviteHost("");
      setInvitePort("");
    } catch (err) {
      setInviteError(String(err));
      setRedeeming(false);
      setRedeemTarget(null);
    }
  };

  return (
    <div className="flex flex-1 flex-col overflow-hidden bg-bg-mid">
      <div className="border-b border-border px-8 py-6">
        <h1 className="mb-1 text-xl font-semibold text-text-bright">
          Discover Servers
        </h1>
        <p className="mb-4 text-sm text-text-secondary">
          Browse available community servers or search for one
        </p>
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search servers..."
          className="w-full max-w-md rounded-xl border border-border bg-bg-dark px-4 py-2.5 text-sm text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent focus:shadow-[0_0_0_2px_var(--color-accent-soft)]"
        />

        {(() => {
          const isLink = inviteInput.trim().toLowerCase().startsWith("decibell:");
          const canJoin = !redeeming && inviteInput.trim().length > 0;
          return (
            <div className="mt-5 flex max-w-2xl items-start gap-2 rounded-2xl border border-border bg-bg-dark p-3">
              <div className="flex-1">
                <input
                  type="text"
                  value={inviteInput}
                  onChange={(e) => setInviteInput(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && handleRedeemInvite()}
                  placeholder="Have an invite? Paste a code or decibell:// link"
                  className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 font-mono text-sm text-text-primary outline-none transition-colors placeholder:font-sans placeholder:text-text-muted focus:border-accent"
                />
                {!isLink && showManualHost && (
                  <div className="mt-2 flex flex-col gap-2 sm:flex-row">
                    <input
                      type="text"
                      value={inviteHost}
                      onChange={(e) => setInviteHost(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && handleRedeemInvite()}
                      placeholder="host (e.g. 203.0.113.5)"
                      className="flex-1 rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
                    />
                    <input
                      type="text"
                      value={invitePort}
                      onChange={(e) => setInvitePort(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && handleRedeemInvite()}
                      placeholder="port"
                      className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent sm:w-24"
                    />
                  </div>
                )}
                <div className="mt-1.5 flex items-center gap-3">
                  {!isLink && (
                    <button
                      onClick={() => setShowManualHost((v) => !v)}
                      className="text-[11px] text-text-muted transition-colors hover:text-text-secondary"
                    >
                      {showManualHost
                        ? "Hide host & port"
                        : "Unlisted server? Enter host & port"}
                    </button>
                  )}
                  {inviteError && (
                    <p className="flex-1 text-xs text-error">{inviteError}</p>
                  )}
                </div>
              </div>
              <button
                onClick={handleRedeemInvite}
                disabled={!canJoin}
                className="shrink-0 rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {redeeming ? "Joining..." : "Join"}
              </button>
            </div>
          );
        })()}
      </div>

      {error && <p className="px-8 pt-3 text-sm text-error">{error}</p>}

      <div className="flex-1 overflow-y-auto px-8 py-6">
        {isLoadingList && servers.length === 0 ? (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {[1, 2, 3, 4, 5, 6].map((i) => (
              <div
                key={i}
                className="flex animate-pulse flex-col overflow-hidden rounded-2xl border border-border bg-bg-dark"
              >
                <div className="h-24 w-full bg-bg-tertiary" />
                <div className="flex flex-col gap-2 p-4">
                  <div className="h-4 w-32 rounded bg-bg-tertiary" />
                  <div className="h-3 w-48 rounded bg-bg-tertiary" />
                  <div className="h-3 w-20 rounded bg-bg-tertiary" />
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {filtered.map((server) => (
              <ServerBrowseCard
                key={server.id}
                server={server}
                isConnected={connectedServers.has(server.id)}
                isConnecting={connectingId === server.id}
                onConnect={() =>
                  handleConnect(server.id, server.hostIp, server.port)
                }
              />
            ))}
          </div>
        )}
        {filtered.length === 0 && !isLoadingList && (
          <p className="mt-12 text-center text-sm text-text-muted">
            No servers found.
          </p>
        )}
      </div>
    </div>
  );
}
