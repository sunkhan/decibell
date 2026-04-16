import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";

interface ServerInfoPayload {
  id: number;
  name: string;
  description: string;
  hostIp: string;
  port: number;
  memberCount: number;
}

interface CommunityAuthPayload {
  serverId: string;
  success: boolean;
  message: string;
  channels: { id: string; name: string; type: string; voiceBitrateKbps: number }[];
}

export function useServerEvents() {
  useEffect(() => {
    const unlistenServers = listen<{ servers: ServerInfoPayload[] }>(
      "server_list_received",
      (event) => {
        const servers = event.payload.servers.map((s) => ({
          id: String(s.id),
          name: s.name,
          description: s.description,
          hostIp: s.hostIp,
          port: s.port,
          memberCount: s.memberCount,
        }));
        useChatStore.getState().setServers(servers);
      }
    );

    const unlistenAuth = listen<CommunityAuthPayload>(
      "community_auth_responded",
      (event) => {
        const { serverId, success, channels } = event.payload;
        if (success) {
          useChatStore.getState().addConnectedServer(serverId);
          const typedChannels = channels.map((ch) => ({
            id: ch.id,
            name: ch.name,
            type: ch.type as "text" | "voice",
            voiceBitrateKbps: ch.voiceBitrateKbps > 0 ? ch.voiceBitrateKbps : undefined,
          }));
          useChatStore.getState().setChannelsForServer(serverId, typedChannels);
          const firstText = typedChannels.find((ch) => ch.type === "text");
          if (firstText) {
            useChatStore.getState().setActiveChannel(firstText.id);
            invoke("join_channel", { serverId, channelId: firstText.id }).catch(console.error);
          }
        }
      }
    );

    return () => {
      unlistenServers.then((fn) => fn());
      unlistenAuth.then((fn) => fn());
    };
  }, []);
}
