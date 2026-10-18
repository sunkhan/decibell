# Multi-device sign-in

2026-10-18. Review B3 (`docs/reviews/2026-10-17-pre-1.0-review.md` §1c). Owner decision: an account is
signed in on several devices at once, all online together, as on Discord. Not "last sign-in wins".

## What was wrong

Central kept one connection per account. A login replaced the account's previous connection silently, so
two devices on one account kicked each other forever: the replaced client reconnected at once with its
resume token, replacing the other, about once a second, with an offline/online presence flap to every
watcher and a 5,000-uid re-subscribe per cycle. The one-connection rule was also baked into the rest of
the system: DMs, edits and deletes echoed only to the sending connection, the client never learned its own
status from anyone but itself, the friends-only setting was pushed from each device's local copy on every
login (the last device to sign in won), and a key rotation on one device had no way to reach the other.

## Design

### Device identity

- The client makes a random device id once per install (128 bits, hex) and keeps it in its config file
  next to the saved sign-in (`config.rs`, not in the settings the renderer rewrites). It goes in every
  `LoginRequest.device_id`, password sign-ins and resumes alike.
- **Same device id: the new login replaces the old connection.** That is the dead-TCP case the old
  replace existed for: a client whose connection died without a FIN comes back before the stale sweep
  noticed. The replaced connection is told `SESSION_CLOSED {REPLACED}` and closed once that is written.
  A live client that receives it is a copy of this install running elsewhere (a copied profile): it makes
  itself a new device id and reconnects as its own device, so two copies never ping-pong. A client that
  really lost its connection never reads the notice (its reader ended before it reconnected).
- **Different device ids coexist.** An empty device id counts as its own device each time (it replaces
  nothing and nothing replaces it); such a connection that dies is reaped by the 60 s stale sweep.
- **At most 8 devices per account** (`MAX_DEVICES`). Changed from the starting "~10": a community keeps at
  most 8 live sessions per account and evicts the oldest past that (`kMaxSessionsPerUser`), and an
  evicted session reconnects and evicts the next one. With central's cap at the community's, every
  signed-in device can hold a session in every community; above it, devices would evict each other there.
  Over the cap a login is refused (`LoginResponse.device_limit`, "This account is signed in on too many
  devices (8). Sign out on one of them first."); no other device is ever evicted, since evicting would
  bring back the ping-pong. A device whose *resume* is refused for the cap backs off and retries like a
  dropped connection (it gets in once another device leaves) and says so once; a typed sign-in just shows
  the message.
- `device_id` is at most 64 bytes; a longer one is refused like a malformed request.

### The directory (`SessionManager`)

- Every connection has a `Connection` record: the account it signed in as, its device id, its idle flag
  and its watch set. An `Account` holds its signed-in connections (in sign-in order) and its presence.
  `watchers_` maps an account to the connections watching it (not accounts): each device watches what it
  shows. The answer to a subscription goes to the connection that asked.
- `deliver_to_user(uid, …)` reaches every connection of the account: DMs, friend lists, the account's own
  state, server picture changes, membership changes. `deliver_to_user_except` skips the connection the
  change came from where that one learns it from its own reply.
- **Presence is per account.** Online while any device is connected: the online delta goes out when the
  first device signs in, the offline delta when the last one leaves. The chosen status, custom status
  message and friends-only setting are the account's. Auto-away is per connection (`SET_IDLE`), and the
  account is idle only while every connection is; a device signing in is active. Invisible stays
  account-wide and airtight: `is_user_visible` is unchanged and every presence-revealing path still asks
  it.
- **The account's own state reaches all its devices** (`OWN_PRESENCE`, central → every connection of the
  account): the chosen status, the status message with the seconds left, friends-only DMs, and whether the
  account is idle. Sent whenever any of them changes, from whichever device or the sweep, except to a
  device signing in (its `LOGIN_RES` carries the same). `LOGIN_RES` now carries the live account state
  rather than what the login's database read found (a change racing the sign-in can't be lost), plus
  `dm_friends_only`; the client adopts it instead of pushing its local copy (which made the last device
  to sign in win).
- **Names.** A rename updates the directory, and every other connection of the account picks the new name
  up before its next handler (a global rename counter each session compares). DMs, edits and call signals
  from another device go out under the current name at once, and its token refresh names it.
- **Closing.** `retire(session, reason)` forgets a connection and closes it once `SESSION_CLOSED` is
  written (5 s at most). `close_other_sessions(uid, keep)` is the single operation "log out other
  devices" needs: every connection of the account but `keep` is retired with `SIGNED_OUT`, and the client
  signs out (forgets its saved sign-in) instead of reconnecting. It has no caller yet: the pre-launch item
  adds the request, a `sessions_valid_after` bump (so the closed devices' resume tokens stop working) and
  the UI.

### Own actions on the other devices

| Action on device A | What central does | Device B |
|---|---|---|
| Sends a DM | the stored message (with id and A's nonce) goes to every connection of the account | renders it as its own, sent (no pending bubble matches the nonce), in the right conversation, which moves up |
| Edits / deletes a DM | `DM_MESSAGE_EDITED` / `DM_MESSAGE_DELETED` to every connection of both accounts | applies it |
| Reads a conversation | `DM_READ_SYNC {peer_uid, up_to_id}` to the account's other connections | clears the unread count up to that message |
| Status / message / DM privacy / auto-away | `OWN_PRESENCE` to every connection | shows it; Invisible makes B tell its communities too |
| Avatar | `AVATAR_CHANGED` to watchers and the account's other connections | refetches its own picture |
| Rename | `USER_RENAMED` to watchers and the account's other connections | takes the name, asks for a fresh token, which goes to its communities |
| New E2EE identity | `E2EE_KEYS_CHANGED` to watchers and the account's other connections | locks and asks for the passphrase (the existing `on_keys_changed` rule, which could never fire before) |
| Friend request / accept / remove / block | `FRIEND_LIST_RES` to every connection of both accounts | replaces its list |
| Joins a community | the community registers the membership; when the row is new, central sends `MEMBERSHIP_ADDED {server}` to every connection | adds the tile and connects (after a 3 s grace, unless it is already connected, which is how the joining device ignores it) |
| Leaves / is kicked or banned | the community already closes every session of the account with `MEMBERSHIP_REVOKED` | unchanged |

`DM_READ_SYNC` and `DM_MESSAGE_DELETED` go through native's ordered DM worker like every DM packet, so a
read or delete can't overtake the message it is about while that message waits for a key fetch.

### Calls

Central now keeps a small call registry (`calls_`, by call id, with the pair of accounts, the caller's
connection and device, the callee connections the invite reached, and the answering connection).
Signals for a call route only between the connections in it.

- **INVITE** rings every connection of the callee (after the friends-only / block / visibility checks,
  unchanged). The caller's other devices never see it. No connection: `PEER_OFFLINE`, as before.
- **RINGING**: the first one goes to the caller; later ones are dropped.
- **BUSY** (a device already in a call, or ringing for another one, answers on its own): swallowed while
  any other device of the callee still rings; the caller hears `BUSY` only when every device the invite
  reached answered busy (or left).
- **ACCEPT**: the first device to answer takes the call. The others get `ANSWERED_ELSEWHERE`; an ACCEPT that
  lost the race gets it too.
- **REJECT** on one device: the caller gets it, the callee's other ringing devices get
  `DECLINED_ELSEWHERE`.
- **CANCEL** from the caller: to every ringing device.
- After the answer, **HANGUP / STREAM_START / STREAM_STOP** go only between the two connections in the call.
- **Reconnects.** A connection in a call that drops keeps its place by device id: when the same device
  comes back (same id) its signals are accepted again and the other side's reach it. A connection with
  another device id gets nothing.
- **S13 stays:** only an INVITE is ever answered (`PEER_OFFLINE` / `NOT_ALLOWED`); every other signal needs
  a call in the registry between those two accounts and is dropped silently otherwise (stronger than the
  block check alone). Clients still can't send central's own kinds.
- Bounds: a connection is the caller of at most 4 calls at once; an unanswered call is forgotten after
  90 s (both sides give up at 45 s), an answered one when it is hung up, when both sides are gone, or 120 s
  after one side left and didn't come back.
- A call and a voice channel stay mutually exclusive per device (client side, unchanged).

### E2EE

- A second device has the identity only after restoring the passphrase backup. `bootstrap` finds no
  local store, finds the backup on central and reports *locked* (the unlock prompt); `setup` refuses to
  publish over an existing backup. So signing in on a second device can't mint a new identity that breaks
  the first. Verified against the code, unchanged.
- Own DMs sent from another device open here: DM envelopes are sealed so that both parties open them with
  their static identity (the sender's own echo opens the same way), and the identity is the account's.
- A new identity made on one device (reset) now reaches the account's other devices
  (`E2EE_KEYS_CHANGED`), which lock and re-sync from the new backup. A device that was signed out at
  the time compares its store with central's current bundle when it loads it and locks the same way
  (`lock_if_rotated_elsewhere`), instead of sealing with the retired identity.
- Pins (TOFU) are per device: each device's key store pins peers on its own. Unchanged.

### Communities

Unchanged: they already allow 8 sessions per account. Each device keeps its own community connections,
its own token (renewed on its own, `SESSION_TOKEN_UPDATE` per connection) and its own resume token.
`sessions_valid_after` cancels every device's resume token at once, as before.

## Wire changes (`proto/messages.proto`)

- `LoginRequest.device_id` (5).
- `LoginResponse.dm_friends_only` (18), `LoginResponse.device_limit` (19).
- `CallSignal.Kind`: `ANSWERED_ELSEWHERE` (11), `DECLINED_ELSEWHERE` (12), central → the callee's other
  devices. Clients can't send them.
- New packets: `DM_READ_SYNC` (168, `DmReadSync`), `OWN_PRESENCE` (169, `OwnPresence`),
  `MEMBERSHIP_ADDED` (170, `MembershipAdded`), `SESSION_CLOSED` (171, `SessionClosed`); payload fields
  170–173.

Client and central ship together (pre-1.0): an older client against this central is one device per
install-less login (empty device id); this client against an older central gets the old replace
behaviour.

## Consumer audit: what assumed one central connection per account

### Central (`src/server/main.cpp`, `session_manager.hpp`, `auth_manager.cpp`)

| Where | Assumption | Now |
|---|---|---|
| `SessionManager::bind` | a login replaces the account's connection | replaces only the same device id; cap 8; returns the live account state |
| `Account::session`, `watching` | one connection, one watch set | `Account::sessions`; the watch set is per connection |
| `watchers_` | account → watching accounts | account → watching connections |
| `leave` | the account goes offline with its connection | offline when its last connection leaves; idle recomputed otherwise |
| `deliver_to_user(s)` | one target | every connection |
| `update_presence`, `SET_IDLE` | idle is the session's | idle per connection, account idle when all are; own state pushed |
| `watch` | `account.session != session` = replaced | per connection |
| `push_user_renamed / avatar / e2ee_keys` | watchers only (the account itself never watched) | plus the account's other connections |
| `expire_status_messages` | watchers only | plus the account's connections |
| `DIRECT_MSG` echo, `DM_EDIT` / `DM_DELETE` broadcasts | to the sending connection | to every connection of the sender |
| `DM_MARK_READ_REQ` | no answer | `DM_READ_SYNC` to the other connections |
| `FRIEND_ACTION_REQ` | requester's list to the requesting connection | to every connection |
| `CALL_SIGNAL` | relay to the peer's connection | registry routing above |
| `RENAME_REQ` comment ("this session is the only one to update") | one connection | others pick the name up (rename counter) |
| `MEMBERSHIP_REGISTER_REQ` | silent | `MEMBERSHIP_ADDED` when the row is new (`registerMembership` returns it) |
| `finish_login` / `LOGIN_RES` | presence from the database read | the directory's |

### Native (`electron-client/native/src`)

| Where | Assumption | Now |
|---|---|---|
| `net/central.rs` login / reconnect | no device id | `device_id` on every `LoginRequest` (`config::device_id`) |
| `net/central.rs` LoginRes refusal | every refused resume falls back to the password | a device-limit refusal doesn't; a refused resume backs off and retries |
| `net/central.rs` `UserRenamed` | our own rename only via `rename_account` | ours from another device: name, saved sign-in, `user_renamed {self}`, token refresh |
| `net/central.rs` `E2eeKeysChanged` → `on_keys_changed` | (already handled our own uid: dead code until now) | reachable |
| `e2ee/session.rs` `bootstrap` | a loaded store's current key is the account's | compared with central's current bundle; a later one locks (`lock_if_rotated_elsewhere`) |
| `presence.rs` hidden flag | only this device sets Invisible | `OWN_PRESENCE` sets it too and tells the communities (`commands/presence.rs::apply_hidden`) |
| `e2ee/session.rs` DM worker | — | `DmJob::ReadSync`, `DmJob::Deleted` |
| `commands/auth.rs` `upload_avatar` | announces our own picture locally (central never told us) | unchanged; other devices hear `AVATAR_CHANGED` from central |
| resume token, token refresh, `SESSION_TOKEN_UPDATE`, `sessions_valid_after` | per process | per device; nothing shared |
| `config.rs` | — | `device_id` in the config file, kept by every writer |

### Renderer (`electron-client/src`)

| Where | Assumption | Now |
|---|---|---|
| `usePresenceEvents` `own_presence` | pushes the local friends-only copy to central at every login | adopts the account's value; applies own-state pushes |
| `presenceStore` own display | Away while this device is idle | Away while the account is idle (`accountIdle`, what others see) |
| `useDmEvents` / `dmStore` | unread only clears here | `dm_read_synced` → `applyReadSync`; a live DM at or below the read cursor doesn't count as unread |
| `useDmEvents` own messages | every own DM is the echo of a local bubble | an own DM with no bubble is appended as sent (already the case; verified) |
| `useCallEvents`, `callStore`, `IncomingCallModal` | an incoming call ends by CANCEL / timeout / own answer | `ANSWERED_ELSEWHERE` / `DECLINED_ELSEWHERE` end it with a short "Answered on another device" / "Declined on another device" card |
| `useAuthEvents` `login_failed` | only on the sign-in page | while signed in (a refused reconnect), a toast |
| `useServerEvents` | memberships only at login | `membership_added` adds the tile |
| `useAvatarStore.seed` | nothing sends our own picture | other devices' avatar changes arrive as `avatar_changed` |

Per-device by nature and left so: drafts, theme and layout, audio devices, hotkeys, link-preview / GIF
filter toggles, auto-away threshold, the E2EE key store's pins, the open conversation and its scroll
position.

## Tests

Central e2e: two devices both get a DM; the sender's other device gets its own DM; read sync;
status / message / DM-privacy sync; same device id replaces (with `SESSION_CLOSED`), different ids coexist,
the ninth is refused with `device_limit`; online until the last device leaves, idle only when every
device is, invisible still airtight with two devices; INVITE rings both devices, answer on one stops the
other with `ANSWERED_ELSEWHERE`, later signals reach only the answering connection, decline on one stops the
other, BUSY only when every device is busy, signals without a call dropped. Native unit tests: device id
persistence and regeneration, read-sync / own-presence mapping. Renderer: pure helpers checked by script,
the call card in the UI preview harness.

## Not done

- "Log out other devices" UI and request (pre-launch item; the operation and the client's handling of
  `SIGNED_OUT` exist).
- A device list (names, last seen) in settings.
- Read state for channels (communities keep none).
- A community leave while another device's community connection is down is learned when that connection
  comes back (refused, the existing "Membership revoked" path), not pushed by central.
