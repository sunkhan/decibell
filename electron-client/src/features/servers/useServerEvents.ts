import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

interface AttachmentTargetResult {
  host: string;
  port: number;
  jwt: string;
  maxAttachmentBytes: number;
}
import type {
  ChannelUpdatedPayload,
  ConnectionEventPayload,
  CommunityAuthRespondedPayload,
  ServerMember,
} from "../../types";

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

// Subscribes to server-lifecycle events:
//   • community_auth_responded — populate channels + server meta on
//     successful join, mark the server connected.
//   • connection_lost / connection_restored — flip connectedServers
//     so the UI can render an offline indicator on the relevant server.
//   • channel_updated — apply retention edits or rename pushes to the
//     local channel record.
export function useServerEvents() {
  useEffect(() => {
    const unlistenAuth = listen<CommunityAuthRespondedPayload>(
      "community_auth_responded",
      (event) => {
        const p = event.payload;
        if (!p.success) {
          // Surface the rejection so ServerBrowseView's invite-redeem
          // flow can render the error inline.
          useUiStore.getState().setAuthError({
            serverId: p.serverId,
            message: p.message,
            errorCode: p.errorCode,
          });
          return;
        }
        useChatStore.getState().setChannelsForServer(p.serverId, p.channels);
        useChatStore.getState().setServerMeta(p.serverId, {
          name: p.serverName,
          description: p.serverDescription,
        });
        useChatStore.getState().setServerOwner(p.serverId, p.ownerUsername);
        useChatStore.getState().setServerAttachmentConfig(
          p.serverId,
          p.attachmentPort,
          p.maxAttachmentBytes,
        );
        useChatStore.getState().addConnectedServer(p.serverId);
        // Push host/port/jwt into main's attachment registry so the
        // decibell-attachment:// protocol handler can authenticate
        // GETs and uploadAttachment can resolve targets without
        // sending the JWT through every IPC call.
        if (p.attachmentPort > 0) {
          invoke<AttachmentTargetResult | null>("get_attachment_target", {
            serverId: p.serverId,
          })
            .then((target) => {
              if (target) {
                window.decibell.attachmentRegistry.set(p.serverId, {
                  host: target.host,
                  port: target.port,
                  jwt: target.jwt,
                });
              }
            })
            .catch((err) => console.error("get_attachment_target:", err));
        }
        // Auto-select the first text channel so the user lands somewhere
        // after joining. Matches tauri-client behaviour.
        const firstText = p.channels.find((ch) => ch.type === "text");
        if (firstText) {
          useChatStore.getState().setActiveChannel(firstText.id);
        }
        // Populate the members sidebar — every server view uses it.
        invoke("list_members", { serverId: p.serverId }).catch(console.error);
      },
    );

    const unlistenMembers = listen<MemberListReceivedPayload>(
      "member_list_received",
      (event) => {
        const { serverId, success, members, bans } = event.payload;
        if (success) {
          useChatStore.getState().setMembersForServer(serverId, members, bans);
        }
      },
    );

    const unlistenMod = listen<ModActionRespondedPayload>(
      "mod_action_responded",
      (event) => {
        const { serverId, success, action } = event.payload;
        if (!success) return;
        if (action === "leave") {
          useChatStore.getState().removeConnectedServer(serverId);
          useUiStore.getState().setActiveView("browse");
        } else {
          // Refresh authoritative roster after kick/ban.
          invoke("list_members", { serverId }).catch(console.error);
        }
      },
    );

    const unlistenRevoked = listen<MembershipRevokedPayload>(
      "membership_revoked",
      (event) => {
        const { serverId } = event.payload;
        useChatStore.getState().removeConnectedServer(serverId);
        useUiStore.getState().setMembershipRevocationNotice({
          serverId,
          action: event.payload.action,
          reason: event.payload.reason,
          actor: event.payload.actor,
        });
        if (useChatStore.getState().activeServerId === serverId) {
          useChatStore.getState().setActiveServer(null);
          useChatStore.getState().setActiveChannel(null);
          useUiStore.getState().setActiveView("home");
        }
      },
    );

    const unlistenInviteList = listen<{
      serverId: string;
      success: boolean;
      message: string;
      invites: import("../../types").ServerInvite[];
    }>("invite_list_received", (event) => {
      const { serverId, success, invites } = event.payload;
      if (success) {
        useChatStore.getState().setInvitesForServer(serverId, invites);
      }
    });

    const unlistenInviteCreated = listen<{
      serverId: string;
      success: boolean;
      message: string;
      invite: import("../../types").ServerInvite | null;
    }>("invite_create_responded", (event) => {
      const { serverId, success, invite } = event.payload;
      if (success && invite) {
        useChatStore.getState().upsertInvite(serverId, invite);
      }
    });

    const unlistenInviteRevoked = listen<{
      serverId: string;
      success: boolean;
      message: string;
      code: string;
    }>("invite_revoke_responded", (event) => {
      const { serverId, success, code } = event.payload;
      if (success) {
        useChatStore.getState().removeInvite(serverId, code);
      }
    });

    // Deep-link `decibell://invite/<host>:<port>/<code>` URLs forwarded
    // from main. Parse renderer-side — the format is fixed and JS's
    // built-in URL parser doesn't help here (the hostname is the literal
    // string "invite" and the actual host:port lives in the path),
    // so a regex is simpler than fighting the URL constructor.
    const unlistenDeepLink = listen<{ url: string }>(
      "deep_link_received",
      (event) => {
        const m = event.payload.url.match(
          /^decibell:\/\/invite\/([^:/]+):(\d+)\/([A-Za-z0-9]+)$/i,
        );
        if (!m) {
          console.warn("[deep-link] could not parse:", event.payload.url);
          return;
        }
        useChatStore.getState().setPendingInvite({
          host: m[1],
          port: parseInt(m[2], 10),
          code: m[3],
        });
      },
    );

    const unlistenLost = listen<ConnectionEventPayload>(
      "connection_lost",
      (event) => {
        const { serverType, serverId } = event.payload;
        if (serverType === "community" && serverId) {
          useChatStore.getState().removeConnectedServer(serverId);
          window.decibell.attachmentRegistry
            .clear(serverId)
            .catch((err) => console.error("attachmentRegistry.clear:", err));
        }
      },
    );

    const unlistenRestored = listen<ConnectionEventPayload>(
      "connection_restored",
      (event) => {
        const { serverType, serverId } = event.payload;
        if (serverType === "community" && serverId) {
          useChatStore.getState().addConnectedServer(serverId);
        }
      },
    );

    const unlistenChannelUpdated = listen<ChannelUpdatedPayload>(
      "channel_updated",
      (event) => {
        const { serverId, success, channel } = event.payload;
        if (success && channel) {
          useChatStore.getState().upsertChannel(serverId, channel);
        }
      },
    );

    return () => {
      unlistenAuth.then((fn) => fn());
      unlistenLost.then((fn) => fn());
      unlistenRestored.then((fn) => fn());
      unlistenChannelUpdated.then((fn) => fn());
      unlistenMembers.then((fn) => fn());
      unlistenMod.then((fn) => fn());
      unlistenRevoked.then((fn) => fn());
      unlistenInviteList.then((fn) => fn());
      unlistenInviteCreated.then((fn) => fn());
      unlistenInviteRevoked.then((fn) => fn());
      unlistenDeepLink.then((fn) => fn());
    };
  }, []);
}
