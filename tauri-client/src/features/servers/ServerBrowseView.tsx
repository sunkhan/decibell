import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToColor } from "../../utils/colors";

export default function ServerBrowseView() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const [search, setSearch] = useState("");
  const [isLoadingList, setIsLoadingList] = useState(false);
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

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

  const handleConnect = async (
    serverId: string,
    host: string,
    port: number
  ) => {
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

  return (
    <div className="flex flex-1 flex-col overflow-hidden bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-8 py-6">
        <h1 className="mb-1 text-xl font-bold text-text-primary">
          Discover Servers
        </h1>
        <p className="mb-4 text-sm text-text-muted">
          Browse available community servers or search for one
        </p>
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search servers..."
          className="w-full max-w-md rounded-lg border border-border bg-bg-primary px-4 py-2.5 text-sm text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent"
        />
      </div>

      {error && <p className="px-8 pt-3 text-sm text-error">{error}</p>}

      {/* Server grid */}
      <div className="flex-1 overflow-y-auto px-8 py-6">
        {isLoadingList && servers.length === 0 ? (
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
            {[1, 2, 3, 4, 5, 6, 7, 8].map((i) => (
              <div
                key={i}
                className="flex animate-pulse flex-col items-center rounded-xl border border-border bg-bg-primary p-5"
              >
                <div className="mb-3 h-14 w-14 rounded-lg bg-bg-tertiary" />
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
                  className="flex flex-col items-center rounded-xl border border-border bg-bg-primary p-5 text-center transition-colors hover:border-accent/50 disabled:opacity-60"
                >
                  <div
                    className="mb-3 flex h-14 w-14 items-center justify-center rounded-lg text-xl font-bold text-white"
                    style={{ backgroundColor: stringToColor(server.name) }}
                  >
                    {server.name.charAt(0).toUpperCase()}
                  </div>
                  <span className="mb-1 text-sm font-semibold text-text-primary">
                    {server.name}
                  </span>
                  <span className="mb-2 line-clamp-2 text-xs text-text-muted">
                    {server.description}
                  </span>
                  <span className="mt-auto text-xs text-text-muted">
                    {server.memberCount} members
                    {isConnected && (
                      <span className="ml-1 text-success">· Connected</span>
                    )}
                  </span>
                  {isConnecting && (
                    <svg
                      className="mt-2 h-4 w-4 animate-spin text-accent"
                      viewBox="0 0 24 24"
                      fill="none"
                    >
                      <circle
                        cx="12"
                        cy="12"
                        r="10"
                        stroke="currentColor"
                        strokeWidth="3"
                        className="opacity-25"
                      />
                      <path
                        d="M4 12a8 8 0 018-8"
                        stroke="currentColor"
                        strokeWidth="3"
                        strokeLinecap="round"
                        className="opacity-75"
                      />
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
