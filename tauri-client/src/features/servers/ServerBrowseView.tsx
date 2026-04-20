import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";

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
      s.description.toLowerCase().includes(search.toLowerCase())
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
        // Full decibell:// URL — parse via the backend and ignore the
        // separate host/port fields.
        const parsed = await invoke<ParsedInviteLink>("parse_invite_link", {
          url: trimmed,
        });
        host = parsed.host;
        port = parsed.port;
        code = parsed.code;
      } else if (!hasManualHost) {
        // Raw code only — ask central to resolve it to a host:port.
        try {
          const resolved = await invoke<ResolvedInvite>("resolve_invite_code", {
            code: trimmed,
          });
          host = resolved.host;
          port = resolved.port;
          code = resolved.code;
        } catch (err) {
          setInviteError(
            `${err}. Enter the server's host and port, or paste a full decibell:// link.`
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
      {/* Header */}
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

        {/* Join with invite */}
        {(() => {
          const isLink = inviteInput
            .trim()
            .toLowerCase()
            .startsWith("decibell:");
          const canJoin = !redeeming && inviteInput.trim().length > 0;
          return (
            <div className="mt-5 max-w-2xl rounded-2xl border border-border bg-bg-dark p-4">
              <h2 className="mb-1 text-sm font-semibold text-text-bright">
                Have an invite?
              </h2>
              <p className="mb-3 text-xs text-text-secondary">
                Paste an invite code on its own — we'll look up the server for
                you. You can also paste a full{" "}
                <span className="font-mono">decibell://</span> link, or enter a
                host and port manually for unlisted servers.
              </p>
              <label className="mb-1 block text-[11px] font-semibold uppercase tracking-wider text-text-muted">
                Invite link or code
              </label>
              <input
                type="text"
                value={inviteInput}
                onChange={(e) => setInviteInput(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleRedeemInvite()}
                placeholder="decibell://invite/... or KH72NQ4XR3"
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 font-mono text-sm text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent"
              />
              {!isLink && (
                <>
                  <label className="mt-3 mb-1 block text-[11px] font-semibold uppercase tracking-wider text-text-muted">
                    Server host &amp; port
                    <span className="ml-1 font-normal normal-case tracking-normal text-text-secondary">
                      (optional — only if the code can't be resolved)
                    </span>
                  </label>
                  <div className="flex flex-col gap-2 sm:flex-row">
                    <input
                      type="text"
                      value={inviteHost}
                      onChange={(e) => setInviteHost(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && handleRedeemInvite()}
                      placeholder="e.g. play.example.com or 203.0.113.5"
                      className="flex-1 rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
                    />
                    <input
                      type="text"
                      value={invitePort}
                      onChange={(e) => setInvitePort(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && handleRedeemInvite()}
                      placeholder="8082"
                      className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent sm:w-28"
                    />
                  </div>
                </>
              )}
              <button
                onClick={handleRedeemInvite}
                disabled={!canJoin}
                className="mt-3 rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {redeeming ? "Joining..." : "Join"}
              </button>
              {inviteError && (
                <p className="mt-2 text-xs text-error">{inviteError}</p>
              )}
            </div>
          );
        })()}
      </div>

      {error && <p className="px-8 pt-3 text-sm text-error">{error}</p>}

      {/* Server grid */}
      <div className="flex-1 overflow-y-auto px-8 py-6">
        {isLoadingList && servers.length === 0 ? (
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
            {[1, 2, 3, 4, 5, 6, 7, 8].map((i) => (
              <div
                key={i}
                className="flex animate-pulse flex-col items-center rounded-2xl border border-border bg-bg-dark p-5"
              >
                <div className="mb-3 h-14 w-14 rounded-xl bg-bg-tertiary" />
                <div className="mb-1.5 h-4 w-24 rounded bg-bg-tertiary" />
                <div className="mb-2 h-3 w-32 rounded bg-bg-tertiary" />
                <div className="h-3 w-16 rounded bg-bg-tertiary" />
              </div>
            ))}
          </div>
        ) : (
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
            {filtered.map((server) => {
              const isConnected = connectedServers.has(server.id);
              const isConnecting = connectingId === server.id;
              return (
                <button
                  key={server.id}
                  onClick={() =>
                    !isConnected &&
                    handleConnect(server.id, server.hostIp, server.port)
                  }
                  disabled={isConnected || isConnecting}
                  className="flex flex-col items-center rounded-2xl border border-border bg-bg-dark p-5 text-center transition-all hover:border-accent/40 hover:-translate-y-0.5 disabled:opacity-60 disabled:hover:translate-y-0"
                >
                  <div
                    className="mb-3 flex h-14 w-14 items-center justify-center rounded-xl text-xl font-semibold text-white"
                    style={{ background: stringToGradient(server.name) }}
                  >
                    {server.name.charAt(0).toUpperCase()}
                  </div>
                  <span className="mb-1 text-sm font-bold text-text-bright">
                    {server.name}
                  </span>
                  <span className="mb-2 line-clamp-2 text-xs text-text-secondary">
                    {server.description}
                  </span>
                  <span className="mt-auto text-xs text-text-muted">
                    {server.memberCount} members
                    {isConnected && (
                      <span className="ml-1 text-success">· Connected</span>
                    )}
                  </span>
                  {isConnecting && (
                    <svg className="mt-2 h-4 w-4 animate-spin text-accent" viewBox="0 0 24 24" fill="none">
                      <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" className="opacity-25" />
                      <path d="M4 12a8 8 0 018-8" stroke="currentColor" strokeWidth="3" strokeLinecap="round" className="opacity-75" />
                    </svg>
                  )}
                </button>
              );
            })}
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
