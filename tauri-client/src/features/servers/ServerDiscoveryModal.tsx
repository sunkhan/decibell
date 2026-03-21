import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

function stringToColor(str: string): string {
  const colors = ["#2CA3E8", "#E8752C", "#8B5CF6", "#43B581", "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4"];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

export default function ServerDiscoveryModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const [search, setSearch] = useState("");
  const [isLoadingList, setIsLoadingList] = useState(false);
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [directHost, setDirectHost] = useState("");
  const [directPort, setDirectPort] = useState("");
  const [showDirect, setShowDirect] = useState(false);

  useEffect(() => {
    if (activeModal === "server-discovery") {
      setIsLoadingList(true);
      invoke("request_server_list").catch(console.error).finally(() => setIsLoadingList(false));
    }
  }, [activeModal]);

  if (activeModal !== "server-discovery") return null;

  const filtered = servers.filter(
    (s) => s.name.toLowerCase().includes(search.toLowerCase()) || s.description.toLowerCase().includes(search.toLowerCase())
  );

  const handleConnect = async (serverId: string, host: string, port: number) => {
    setConnectingId(serverId);
    setError(null);
    try {
      await invoke("connect_to_community", { serverId, host, port });
      setActiveServer(serverId);
      setActiveView("server");
      closeModal();
    } catch (err) {
      setError(String(err));
    } finally {
      setConnectingId(null);
    }
  };

  const handleDirectConnect = async () => {
    const port = parseInt(directPort, 10);
    if (!directHost || isNaN(port)) {
      setError("Enter a valid host and port");
      return;
    }
    const serverId = `direct-${directHost}-${port}`;
    await handleConnect(serverId, directHost, port);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" onClick={closeModal}>
      <div className="flex max-h-[80vh] w-full max-w-[600px] flex-col rounded-xl bg-bg-secondary shadow-2xl" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between border-b border-border px-6 py-4">
          <div>
            <h2 className="text-lg font-bold text-text-primary">Discover Servers</h2>
            <p className="text-sm text-text-muted">Browse available community servers</p>
          </div>
          <button onClick={closeModal} className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-white/10 hover:text-text-primary">×</button>
        </div>
        <div className="px-6 pt-4">
          <input type="text" value={search} onChange={(e) => setSearch(e.target.value)} placeholder="🔍 Search servers..." className="w-full rounded-lg border border-border bg-bg-primary px-4 py-2.5 text-sm text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent" />
        </div>
        {error && <p className="px-6 pt-2 text-sm text-error">{error}</p>}
        <div className="flex-1 overflow-y-auto p-6">
          {isLoadingList && servers.length === 0 ? (
            <div className="grid grid-cols-2 gap-3">
              {[1, 2, 3, 4].map((i) => (
                <div key={i} className="flex animate-pulse flex-col items-center rounded-xl border border-border bg-bg-primary p-4">
                  <div className="mb-3 h-12 w-12 rounded-xl bg-bg-tertiary" />
                  <div className="mb-1 h-4 w-20 rounded bg-bg-tertiary" />
                  <div className="mb-2 h-3 w-28 rounded bg-bg-tertiary" />
                  <div className="h-3 w-16 rounded bg-bg-tertiary" />
                </div>
              ))}
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-3">
              {filtered.map((server) => {
                const isConnected = connectedServers.has(server.id);
                const isConnecting = connectingId === server.id;
                return (
                  <button key={server.id} onClick={() => !isConnected && handleConnect(server.id, server.hostIp, server.port)} disabled={isConnected || isConnecting} className="flex flex-col items-center rounded-xl border border-border bg-bg-primary p-4 text-center transition-colors hover:border-accent/50 disabled:opacity-50">
                    <div className="mb-3 flex h-12 w-12 items-center justify-center rounded-xl text-lg font-bold text-white" style={{ backgroundColor: stringToColor(server.name) }}>{server.name.charAt(0).toUpperCase()}</div>
                    <span className="mb-1 text-sm font-semibold text-text-primary">{server.name}</span>
                    <span className="mb-2 line-clamp-2 text-xs text-text-muted">{server.description}</span>
                    <span className="mt-auto text-xs text-text-muted">
                      {server.memberCount} members
                      {isConnected && <span className="ml-1 text-success">· Connected</span>}
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
              <button onClick={() => setShowDirect(!showDirect)} className="flex flex-col items-center rounded-xl border border-dashed border-border bg-bg-primary p-4 text-center transition-colors hover:border-accent/50">
                <div className="mb-3 flex h-12 w-12 items-center justify-center rounded-xl bg-bg-tertiary text-2xl text-success">+</div>
                <span className="text-sm font-semibold text-text-muted">Add by IP</span>
                <span className="text-xs text-text-muted">Connect directly</span>
              </button>
            </div>
          )}
          {showDirect && (
            <div className="mt-4 flex gap-2">
              <input type="text" value={directHost} onChange={(e) => setDirectHost(e.target.value)} placeholder="Host (e.g., 192.168.1.100)" className="flex-1 rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent" />
              <input type="text" value={directPort} onChange={(e) => setDirectPort(e.target.value)} placeholder="Port" className="w-20 rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent" />
              <button onClick={handleDirectConnect} className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover">Connect</button>
            </div>
          )}
          {filtered.length === 0 && !isLoadingList && <p className="mt-8 text-center text-sm text-text-muted">No servers found.</p>}
        </div>
      </div>
    </div>
  );
}
