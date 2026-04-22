import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import type { ServerInvite, ServerMember } from "../../types";

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
  errorCode: string;
  serverName: string;
  serverDescription: string;
  ownerUsername: string;
}

interface InviteCreateRespondedPayload {
  serverId: string;
  success: boolean;
  message: string;
  invite: ServerInvite | null;
}

interface InviteListReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  invites: ServerInvite[];
}

interface InviteRevokeRespondedPayload {
  serverId: string;
  success: boolean;
  message: string;
  code: string;
}

interface MemberListReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  members: ServerMember[];
  bans: string[];
}

interface ModActionRespondedPayload {
  serverId: string;
  success: boolean;
  message: string;
  username: string;
  action: string;
}

interface MembershipRevokedPayload {
  serverId: string;
  action: string;
  reason: string;
  actor: string;
}

interface DeepLinkPayload { url: string }

interface ParsedInviteLink { host: string; port: number; code: string }

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
        const { serverId, success, message, channels, errorCode, serverName, serverDescription, ownerUsername } = event.payload;
        const store = useChatStore.getState();
        if (success) {
          store.addConnectedServer(serverId);
          store.setServerOwner(serverId, ownerUsername);
          store.setServerMeta(serverId, { name: serverName, description: serverDescription });
          const typedChannels = channels.map((ch) => ({
            id: ch.id,
            name: ch.name,
            type: ch.type as "text" | "voice",
            voiceBitrateKbps: ch.voiceBitrateKbps > 0 ? ch.voiceBitrateKbps : undefined,
          }));
          store.setChannelsForServer(serverId, typedChannels);
          const firstText = typedChannels.find((ch) => ch.type === "text");
          if (firstText) {
            store.setActiveChannel(firstText.id);
          }
          // Populate the members sidebar — every channel view uses it.
          invoke("list_members", { serverId }).catch(console.error);
        } else {
          // Surface the rejection to the UI — the invite modal and
          // ServerBrowseView listen for this error_code to render inline.
          useUiStore.getState().setAuthError?.({ serverId, message, errorCode });
        }
      }
    );

    const unlistenInviteCreate = listen<InviteCreateRespondedPayload>(
      "invite_create_responded",
      (event) => {
        const { serverId, success, invite } = event.payload;
        if (success && invite) {
          useChatStore.getState().upsertInvite(serverId, invite);
        }
      }
    );

    const unlistenInviteList = listen<InviteListReceivedPayload>(
      "invite_list_received",
      (event) => {
        const { serverId, success, invites } = event.payload;
        if (success) {
          useChatStore.getState().setInvitesForServer(serverId, invites);
        }
      }
    );

    const unlistenInviteRevoke = listen<InviteRevokeRespondedPayload>(
      "invite_revoke_responded",
      (event) => {
        const { serverId, success, code } = event.payload;
        if (success) {
          useChatStore.getState().removeInvite(serverId, code);
        }
      }
    );

    const unlistenMembers = listen<MemberListReceivedPayload>(
      "member_list_received",
      (event) => {
        const { serverId, success, members, bans } = event.payload;
        if (success) {
          useChatStore.getState().setMembersForServer(serverId, members, bans);
        }
      }
    );

    const unlistenMod = listen<ModActionRespondedPayload>(
      "mod_action_responded",
      (event) => {
        const { serverId, success, action } = event.payload;
        if (!success) return;
        // Refresh authoritative state after a successful kick/ban/leave.
        if (action === "leave") {
          useChatStore.getState().removeConnectedServer(serverId);
          useUiStore.getState().setActiveView?.("browse");
        } else {
          invoke("list_members", { serverId }).catch(console.error);
        }
      }
    );

    const unlistenRevoked = listen<MembershipRevokedPayload>(
      "membership_revoked",
      (event) => {
        const { serverId, action, reason, actor } = event.payload;
        useChatStore.getState().removeConnectedServer(serverId);
        useUiStore.getState().setMembershipRevocationNotice?.({
          serverId,
          action,
          reason,
          actor,
        });
      }
    );

    const unlistenDeepLink = listen<DeepLinkPayload>(
      "deep_link_received",
      async (event) => {
        try {
          const parsed = await invoke<ParsedInviteLink>("parse_invite_link", {
            url: event.payload.url,
          });
          useChatStore.getState().setPendingInvite(parsed);
        } catch (err) {
          console.warn("Failed to parse deep link", event.payload.url, err);
        }
      }
    );

    return () => {
      unlistenServers.then((fn) => fn());
      unlistenAuth.then((fn) => fn());
      unlistenInviteCreate.then((fn) => fn());
      unlistenInviteList.then((fn) => fn());
      unlistenInviteRevoke.then((fn) => fn());
      unlistenMembers.then((fn) => fn());
      unlistenMod.then((fn) => fn());
      unlistenRevoked.then((fn) => fn());
      unlistenDeepLink.then((fn) => fn());
    };
  }, []);
}
