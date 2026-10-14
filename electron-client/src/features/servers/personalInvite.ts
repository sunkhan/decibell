import { invoke, listen } from "../../lib/ipc";
import type { ServerInvite } from "../../types";
import { buildInviteLink } from "./inviteLink";

interface InviteCreateRespondedPayload {
  serverId: string;
  success: boolean;
  message: string;
  invite: ServerInvite | null;
  requestId: number;
}

/// The community answers INVITE_CREATE_REQ asynchronously; a request id
/// (echoed on the response) ties each answer to its caller, so several
/// invites can be in flight at once and a rate-limited request that never
/// answers can't hand its reply to the next one.
let nextRequestId = 1;
const NO_ANSWER_MS = 8000;

/// A personal invite: single-use, never expires, only `username` can
/// redeem it. The server hands back their live one when it exists, so
/// inviting the same friend twice doesn't pile up invites.
export function createPersonalInvite(serverId: string, username: string): Promise<ServerInvite> {
  const requestId = nextRequestId++;
  return new Promise<ServerInvite>((resolve, reject) => {
    let settled = false;
    let unlisten: (() => void) | null = null;
    const settle = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unlisten?.();
      fn();
    };
    const timer = setTimeout(
      () => settle(() => reject(new Error("The server didn't answer."))),
      NO_ANSWER_MS,
    );
    listen<InviteCreateRespondedPayload>("invite_create_responded", (e) => {
      const p = e.payload;
      if (p.serverId !== serverId || Number(p.requestId) !== requestId) return;
      settle(() =>
        p.success && p.invite
          ? resolve(p.invite)
          : reject(new Error(p.message || "Couldn't create the invite.")),
      );
    })
      .then((fn) => {
        if (settled) {
          fn();
          return;
        }
        unlisten = fn;
        return invoke("create_invite", {
          serverId,
          expiresAt: 0,
          maxUses: 1,
          forUser: username,
          requestId,
        });
      })
      .catch((err) => settle(() => reject(err instanceof Error ? err : new Error(String(err)))));
  });
}

/// Invite a friend: their personal invite, sent to them as a DM. The link
/// renders as an invite card on their side.
export async function inviteFriend(serverId: string, username: string): Promise<void> {
  const invite = await createPersonalInvite(serverId, username);
  await invoke("send_private_message", {
    recipient: username,
    message: buildInviteLink(invite.code),
  });
}
