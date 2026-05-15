import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import type { CommunityServer } from "../../types";

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

interface MembershipsReceivedPayload {
  memberships: Array<{
    id: number;
    name: string;
    description: string;
    hostIp: string;
    port: number;
    memberCount: number;
    pictureVersion: string;
  }>;
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
    // Auto-rejoin: LoginResponse.memberships → placeholder tiles in
    // ServerBar. Native auto-fires connect_to_community for each, so
    // we just need to (a) merge any unseen entries into chatStore.servers
    // (b) mark them as "connecting…" until community_auth_responded
    // lands per server.
    const unlistenMemberships = listen<MembershipsReceivedPayload>(
      "memberships_received",
      (event) => {
        const servers: CommunityServer[] = event.payload.memberships.map((s) => ({
          id: String(s.id),
          name: s.name,
          description: s.description,
          hostIp: s.hostIp,
          port: s.port,
          memberCount: s.memberCount,
        }));
        const chat = useChatStore.getState();
        chat.mergeServers(servers);
        chat.setPendingMemberships(servers.map((s) => s.id));
        // Propagate picture_version so the ServerBar tile knows up-front
        // whether to lazy-fetch a picture.
        for (const m of event.payload.memberships) {
          chat.setServerPictureVersion(String(m.id), m.pictureVersion ?? "");
        }
      },
    );

    const unlistenAuth = listen<CommunityAuthRespondedPayload>(
      "community_auth_responded",
      (event) => {
        const p = event.payload;
        if (!p.success) {
          // Auto-rejoin: if this was a pending auto-rejoin attempt
          // (entered the pendingMembershipServerIds set via the
          // memberships_received listener above) and we got rejected,
          // the user was kicked/banned while offline. Drop the stale
          // central row + toast once.
          const chat = useChatStore.getState();
          if (chat.pendingMembershipServerIds.has(p.serverId)) {
            chat.removePendingMembership(p.serverId);
            invoke("request_drop_membership", { serverId: p.serverId }).catch(
              (err) =>
                console.error("[auto-rejoin] request_drop_membership:", err),
            );
            const serverName =
              chat.servers.find((s) => s.id === p.serverId)?.name ??
              "a community server";
            toast.error(
              "Membership revoked",
              `You're no longer a member of ${serverName}.`,
            );
            return;
          }
          // Surface the rejection so ServerBrowseView's invite-redeem
          // flow can render the error inline.
          useUiStore.getState().setAuthError({
            serverId: p.serverId,
            message: p.message,
            errorCode: p.errorCode,
          });
          return;
        }
        // Auto-rejoin: successful auth — drop the pending placeholder
        // (next paint of ServerBar will pick up the connectedServers
        // entry below and render the real tile state).
        useChatStore.getState().removePendingMembership(p.serverId);
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

    const unlistenChannelDeleteRes = listen<{
      success: boolean;
      message: string;
      serverId: string;
      channelId: string;
      messageId: number;
    }>("channel_message_delete_responded", (event) => {
      const p = event.payload;
      const chat = useChatStore.getState();
      if (!p.success) {
        // Server rejected (403/404). Restore the bubble + surface
        // the server's reason as a toast.
        chat.restorePendingDeletion(p.channelId, p.messageId);
        toast.error(
          "Couldn't delete message",
          p.message || "Server rejected the request.",
        );
        return;
      }
      // Success: clear the pending entry. The broadcast (or already-
      // optimistic-remove) keeps the bubble gone.
      chat.clearPendingDeletion(p.channelId, p.messageId);
    });

    const unlistenChannelDeleted = listen<{
      serverId: string;
      channelId: string;
      messageId: number;
      deletedAt: number;
      deletedBy: string;
    }>("channel_message_deleted", (event) => {
      const { channelId, messageId } = event.payload;
      const chat = useChatStore.getState();
      // Idempotent: removeMessage on an already-gone id is a no-op.
      // Same handler for "my delete succeeded" (already removed
      // optimistically) and "someone else deleted this message".
      chat.removeMessage(channelId, messageId);
      chat.clearPendingDeletion(channelId, messageId);
    });

    const unlistenServerPictureUpdateRes = listen<{
      success: boolean;
      message: string;
      serverId: string;
      version: string;
    }>("server_picture_update_responded", (event) => {
      const p = event.payload;
      if (!p.success) {
        toast.error("Couldn't update server picture", p.message);
        return;
      }
      // Success: the broadcast (server_picture_changed) updates the
      // version + invalidates cached bytes; next tile render lazy-
      // fetches. Modal closes implicitly via the upload handler.
    });

    const unlistenServerPictureChanged = listen<{
      serverId: number;
      version: string;
    }>("server_picture_changed", (event) => {
      const { serverId, version } = event.payload;
      useChatStore
        .getState()
        .setServerPictureVersion(String(serverId), version);
    });

    const unlistenServerPictureReceived = listen<{
      serverId: number;
      version: string;
      data: string;
    }>("server_picture_received", (event) => {
      const { serverId, version, data } = event.payload;
      // Empty data means the server has no picture set (or unknown
      // server_id). setServerPictureData also drops mismatched
      // versions, so a stale fetch landing after a newer
      // version-changed event is a no-op.
      if (!data) return;
      useChatStore
        .getState()
        .setServerPictureData(String(serverId), version, data);
    });

    return () => {
      unlistenMemberships.then((fn) => fn());
      unlistenAuth.then((fn) => fn());
      unlistenChannelDeleteRes.then((fn) => fn());
      unlistenChannelDeleted.then((fn) => fn());
      unlistenServerPictureUpdateRes.then((fn) => fn());
      unlistenServerPictureChanged.then((fn) => fn());
      unlistenServerPictureReceived.then((fn) => fn());
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
