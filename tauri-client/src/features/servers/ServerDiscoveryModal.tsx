import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function ServerDiscoveryModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const [host, setHost] = useState("");
  const [port, setPort] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);

  if (activeModal !== "direct-connect") return null;

  const handleConnect = async () => {
    const portNum = parseInt(port, 10);
    if (!host.trim() || isNaN(portNum)) {
      setError("Enter a valid host and port");
      return;
    }
    setConnecting(true);
    setError(null);
    const serverId = `direct-${host.trim()}-${portNum}`;
    try {
      await invoke("connect_to_community", {
        serverId,
        host: host.trim(),
        port: portNum,
      });
      setActiveServer(serverId);
      setActiveView("server");
      closeModal();
      setHost("");
      setPort("");
    } catch (err) {
      setError(String(err));
    } finally {
      setConnecting(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={closeModal}
    >
      <div
        className="w-full max-w-sm rounded-2xl border border-border bg-bg-secondary p-6 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold text-text-bright">
            Direct Connect
          </h2>
          <button
            onClick={closeModal}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            ×
          </button>
        </div>
        <p className="mb-4 text-sm text-text-secondary">
          Connect to a server by IP address and port
        </p>
        {error && <p className="mb-3 text-sm text-error">{error}</p>}
        <div className="mb-3 flex gap-2">
          <input
            type="text"
            value={host}
            onChange={(e) => setHost(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleConnect()}
            placeholder="Host (e.g., 192.168.1.100)"
            className="flex-1 rounded-lg border border-border bg-bg-primary px-3 py-2.5 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          />
          <input
            type="text"
            value={port}
            onChange={(e) => setPort(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleConnect()}
            placeholder="Port"
            className="w-20 rounded-lg border border-border bg-bg-primary px-3 py-2.5 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          />
        </div>
        <button
          onClick={handleConnect}
          disabled={connecting}
          className="w-full rounded-lg bg-accent px-4 py-2.5 text-sm font-bold text-white transition-colors hover:bg-accent-hover disabled:opacity-50"
        >
          {connecting ? "Connecting..." : "Connect"}
        </button>
      </div>
    </div>
  );
}
