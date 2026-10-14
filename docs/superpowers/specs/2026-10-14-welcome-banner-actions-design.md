# Welcome banner actions: Invite friends, channel topics, Manage channel

2026-10-14. The empty-channel welcome banner showed three buttons that did nothing.

## Invite friends

Owner's ask: list the user's friends who aren't members. Pressing Invite (for users with the invite
permission) creates a single-use invite with no time limit and sends it to that friend as a DM. Use the
existing invites.

It reuses the existing invites, with three additions:

- **Personal invites.** `InviteCreateRequest.for_user` binds the invite to one username. It is always
  single-use. Redeeming it as anyone else fails ("This invite was sent to someone else.") and does not
  use it up. Without this, a never-expiring single-use link sitting in a DM works for whoever gets hold
  of it. Usernames, not uids, because the server can't know the uid of someone who isn't a member yet.
  A rename at central breaks the invite, and inviting the friend again under their new name fixes it.
- **Reuse.** Asking again for the same friend returns the creator's live personal invite, so repeated
  clicks never pile up invites that never expire. The server refuses a personal invite for an existing
  member or for yourself.
- **Request ids.** `request_id` is echoed on `InviteCreateResponse`. Responses used to be matched by
  arrival order, and the admin bucket silently drops rate-limited requests, so with several invites in
  flight a reply could go to the wrong friend. Requests with an id report errors to their caller. The
  global toast is only for requests without one.

**Who is already a member** is judged on the roster the client already has. The owner decided this
for privacy: the friend list never goes to a community, whose host could log it, and the server isn't
asked to filter it. (A first cut sent the usernames through `MemberSearchRequest`, and it was
withdrawn before release.) The roster holds every online member but only the first offline page, so on
a big server an offline member can still be listed. Inviting them is refused with
`InviteCreateResponse.already_member`, and the row then reads "Already here" instead of showing an
error. The server only learns a friend's name when the user invites that friend, which is inherent to an
invite bound to them.

The DM is the bare `decibell://invite/<CODE>` link, so the friend sees the usual invite card. It goes
through `send_private_message`, so E2EE applies when both sides have it.

`InviteInfo.for_user` shows in the Invites list (→ name), and the audit log gets a "For …" chip.

Gate: CREATE_INVITE or MANAGE_INVITES, mirroring the server and the sidebar.

## Channel topics

- `ChannelInfo.topic` (15) and `ChannelUpdateRequest.topic` (10, `optional`, absent = unchanged).
  Text channels only. MANAGE_CHANNELS in the channel. Stored in `channels.topic` (schema v13).
- One line of plain text, ≤ 1024 UTF-8 bytes. The server clamps on a character boundary, drops control
  characters (`clamp_utf8`) and trims blanks. The client turns line breaks into spaces and cuts at the
  byte cap itself, so the server never truncates what the user saw.
- The header shows it after the name, behind a divider, on one truncated line with the full text in a
  tooltip. It is edited in the Channel topic field in channel settings. The banner had an Edit
  topic button with its own dialog at first; the owner dropped it on 2026-10-14, since the banner's
  Manage channel opens settings anyway. The `channel_updated` broadcast keeps everyone current, and
  audit details say `topic`.
- Topics are **not encrypted**, even in encrypted channels: the server stores and sends them in the
  clear, as it does channel names. Owner decision: they label the channel, not the conversation.
  The topic field warns about this on encrypted channels ("Topics aren't encrypted. The server host
  can read them."). See the encrypted-channels spec's accepted metadata.

## Manage channel

Renamed from "Set rules". It opens the existing channel settings for the channel and shows for
MANAGE_CHANNELS or MANAGE_ROLES, like the sidebar gear. The banner leaves out any action the user
can't take, so it now has two at most: Invite friends and Manage channel.

## Rollout

Every wire change is additive. The community server must be rebuilt along with the client, which is
the usual pre-1.0 rule. An older server ignores `for_user` / `request_id` / `topic`. Inviting then
times out ("The server didn't answer.") and topic edits don't stick. Central is unchanged.
