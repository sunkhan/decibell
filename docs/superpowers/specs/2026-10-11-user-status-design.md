# User statuses — online / away / do not disturb / invisible

Status: implemented 2026-10-12 (client, central, community). Owner decisions are marked **(owner)**.

## What the user gets

- A status menu on their own avatar/name in the bottom-left user panel: **Online**, **Away**,
  **Do Not Disturb**, **Invisible**. The panel shows the status dot and its name.
- Status dots everywhere presence shows (member list, DM list, friends rail and page, DM header,
  profile popup and card, server settings → Members). The shape carries the status as well as colour:
  online is a filled disc, away a crescent moon, do-not-disturb a disc with a bar, offline/invisible a
  hollow ring (`components/StatusDot.tsx`, ring + cut-outs in the surface colour, like every avatar dot).
- **Away is automatic** after a user-chosen idle time (Settings → Privacy, default 10 minutes, or
  Never) **(owner)**, and right away on screen lock or sleep. Only while the chosen status is Online.
- **Do Not Disturb** silences incoming DM calls: the call still appears, without the ring or taskbar
  flash, and becomes a missed call if unanswered **(owner)**. (The client has no desktop notifications
  or message sounds yet; when it gets them, DND should silence those too.)
- **Invisible** looks offline to everyone, airtight against modified clients **(owner)**. Calls to an
  invisible user get the normal "offline" answer.
- Statuses are visible to every signed-in user, like online/offline was **(owner)**.

## Source of truth: central

- `users.presence_status` (SMALLINT, the `PresenceStatus` enum value) holds the chosen status; it
  survives restarts and devices. Read at login together with `avatar_version` and `dm_friends_only`
  (`AuthManager::getPresenceProfile`), **before** anyone is told about the login, so an invisible user
  never flashes online.
- Auto-away is a session flag (`SET_IDLE_REQ`); effective status = AWAY while an ONLINE user is idle.
  Native re-sends it after a central reconnect.
- What others see is a `PresenceView` (visible?, effective status). A change to it is the only thing
  that produces traffic: `PRESENCE_DELTA` (one user's full current state, idempotent) to every
  authenticated session. Invisible users are never on the wire. An invisible login or disconnect sends
  nothing (an "offline" delta at login would itself reveal it).
- A session gets one `PRESENCE_UPDATE` snapshot at login. Older clients (no
  `LoginRequest.presence_deltas`) keep getting a full snapshot on every change, as before.
- Presence now reaches **authenticated sessions only** (it used to go to every socket, including
  pre-login and community-sync connections).
- A reconnect's kick of the stale session no longer announces the user offline in between: the new
  session's username is set before the kick, and `leave()` only announces the user's last *visible*
  session going away.
- `LoginResponse.presence_statuses` = this central understands statuses (the client hides the menu
  otherwise); `LoginResponse.status` echoes the saved choice.
- Rate limit: status / idle / DM-privacy changes share a 10-burst, 1-per-2s bucket per session.

### Invisible: every path that could reveal a connection

| Path | Behaviour |
|---|---|
| Presence snapshot / deltas | invisible users omitted / nothing sent |
| Friend list `FriendInfo.status` | `is_user_visible` (was "has a session") |
| `CALL_SIGNAL` INVITE | `PEER_OFFLINE` when the callee isn't visible (other call signals still route, so an invisible user can place calls) |
| DM friends-only gate | the setting is now saved on the account (`users.dm_friends_only`). It used to live only in the session, so a non-friend's DM was rejected while the recipient was connected and accepted while not — a probe that saw through Invisible. It also reset on every reconnect; the client re-pushes its local value after each login. |
| Community rosters | see below |

Actions a user takes while invisible (sending, reading, joining voice, streaming) are visible as such —
Invisible hides passive presence, as in other chat apps.

## Communities

Central can't reach a community (communities only open short outbound connections to central), and
each community reports who is connected to it. So the client tells each community itself:

- `CommunityAuthRequest.presence_hidden` — set at connect while invisible, so the roster never shows
  the member online, not even for an instant.
- `PRESENCE_VISIBILITY {hidden}` — sent to every connected community when the user enters or leaves
  Invisible.
- The community reports `MemberInfo.is_online` = "has a non-hidden session"
  (`user_visible_locked`, `get_visible_usernames`). Upserts fire only on a visible change; an invisible
  member's connect/disconnect produces no roster traffic (a brand-new or renamed member still gets an
  offline upsert so rosters list them). `get_online_usernames` (raw connections) stays for internal
  routing such as channel-key fillers.

The client displays central's status for community members (`usePresence`), falling back to the
community's own `is_online` only before central's first snapshot (central unreachable). Status is never
told to communities beyond "hidden".

## Client pieces

- `stores/presenceStore.ts` — statuses map (absent = offline), `loaded`, `supported`, own status,
  idle; `usePresence(username, communityOnline?)`, `useOwnDisplayStatus`, `resolvePresence`.
  `chatStore.onlineUsers` is gone. Cleared on logout and on central connection loss.
- `features/presence/usePresenceEvents.ts` — `user_list_updated` (snapshot), `presence_delta`,
  `own_presence` (+ friends-only re-push), central `connection_lost`. Friend rows' online/offline flag
  follows presence (pending/blocked untouched).
- `features/presence/StatusMenu.tsx`, `useAutoAway.ts` (polled every 15 s while active, every 1 s while away so input brings the user back at once). Idle comes from native
  `system_idle.rs` first: on Linux Wayland Electron's powerMonitor reads no idle time (0 s / "active"
  forever on KDE, natively and through XWayland; KDE's ScreenSaver refuses GetSessionIdleTime), so the
  compositor's ext-idle-notify-v1 (v2 input-idle when offered) announces idle/resumed; GNOME answers
  org.gnome.Mutter.IdleMonitor; lock is the ScreenSaver's GetActive. "unsupported" (Windows, macOS,
  X11) falls back to `electron/main/idle.ts` (`powerMonitor.getSystemIdleState`, lock/suspend events).
- Native: `presence.rs` (lock-free hidden/idle flags), `commands/presence.rs` (`set_status`,
  `set_idle`), events `presence_delta`, `own_presence`.

## Rollout

Server must ship with or before the client, but each side degrades gracefully:
- New client + old central: `presence_statuses` false → no status menu; snapshots decode as Online.
- Old client + new central: gets full snapshots (invisible users omitted, statuses ignored).
- New client + old community: rosters still flag invisible members as connected; the stock client shows
  central's status anyway, but a modified client could see them until the community is updated.

Central adds its two columns on start (`ADD COLUMN IF NOT EXISTS`).

## Open

- Live two-account test (status changes, auto-away, invisible against a second client, DND call).
- GNOME Wayland path (Mutter IdleMonitor) is untested; KDE Wayland verified 2026-10-12.
