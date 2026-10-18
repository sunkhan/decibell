# Devices and passwords

2026-10-19. Follows multi-device sign-in (`2026-10-18-multi-device-design.md`, review B3) and closes the
pre-launch item "password change + log out other devices". Owner request: a device list where specific
devices can be signed out, not just "all other devices", and a password change.

## Why now

Before multi-device, an account had one connection: signing in yourself kicked any other session off.
Since B3, up to 8 devices stay online together, so someone who learns the password (or copies a saved
reconnect token) can sit beside the owner's devices unnoticed, receiving plain DMs, read marks, call rings
and status changes. Nothing showed the devices, and nothing could remove one.

## What a device is

An install (`LoginRequest.device_id`, made once per install, multi-device design) that signed in to the
account by password and hasn't been signed out since. Central keeps one row per device (`devices`, central
schema 6):

| Column | |
|---|---|
| `id` | central's handle, shown to the account's devices and used to sign one out; also the `sid` claim in its session tokens |
| `uid`, `device_id` | unique together; the install's own id never leaves central's answers |
| `name`, `platform`, `app_version` | what the install reports (`LoginRequest.device_name` / `device_platform` / `app_version`) |
| `address` | where it last signed in or reconnected from |
| `signed_in_at` | its last password sign-in: its resume token carries the same time |
| `last_active_at` | updated at sign-in, resume, every token renewal (6-hourly) and disconnect |

**The row is what lets a device come back without the password.** A resume token is valid only for the device
it was issued to (`did` claim) and the password sign-in it came from (`iat` = `signed_in_at`). A saved
password signs in automatically (`LoginRequest.automatic`) only on a device the account has. A typed password
always works and records (or refreshes) the device.

**Device name.** The operating system's name: Linux reads os-release's `PRETTY_NAME` ("CachyOS", "Ubuntu
24.04.1 LTS"), else "Windows" / "macOS". Not the computer's hostname, which often carries a real name
("Ayse's laptop") and would be stored on central: Discord and Telegram show the OS and an address too. The
list also shows the app version, the address, when it signed in and when it was last active, so two
machines on one OS are told apart by address and activity.

## Signing devices out

`DEVICE_SIGN_OUT_REQ {ids | all_others | this_device}`:

1. Central deletes the rows and records each in `revoked_devices (uid, device_id, sid, revoked_at)`.
2. It closes those devices' connections with `SESSION_CLOSED {SIGNED_OUT}` (all other connections for
   `all_others`, listed or not). The client signs out, forgets its saved sign-in, and the sign-in page says
   "You were signed out on this device".
3. A device that was offline finds out when it comes back: its resume is refused with
   `LoginResponse.signed_out`, and so is its saved password (automatic). The client signs out the same way.
   Typing the password signs it in again, as a new device (new row, new id).
4. **Communities** drop its sessions within a minute: the next heartbeat answer
   (`ServerHeartbeatRes.revoked`) lists the member's devices signed out in the last 25 hours (a session token
   lives 24). The community refuses tokens with that `(uid, sid)` until then and closes live sessions holding
   one. A token renewal (`SESSION_TOKEN_UPDATE`) can't move a session onto a revoked device's token.
   Backstop: a community session now closes when its token expires without a renewal (clients renew every 6
   hours, 18 before expiry), so a device that can't renew is out within a day even if the heartbeat can't
   reach central. Consequence, accepted: if central is unreachable for more than ~18 hours, community
   sessions end as their tokens run out (a reconnect with them was already refused).
5. The account's other connections get `DEVICES_CHANGED` and reload an open list.

The device asking can't sign itself out by id (`this_device` is for logging out, below), and ids of another
account's devices match nothing. A request names at most 128 devices.

**Logging out** sends `this_device` first (best effort, 3 s): central forgets the device, so the list doesn't
show it and its (now cleared) saved password couldn't sign it back in. An offline logout still works locally;
the row then ages out or is signed out from another device.

## Password change

`PASSWORD_CHANGE_REQ {current_password, new_password}` (password-confirmed, like a rename):

- Refused over a 10 burst / 1 per 30 s bucket per connection, and while the account's failed sign-ins from
  that address are over the limit. A wrong current password counts as a failed sign-in (per account and
  address, `abuse_limits.hpp`), so a stolen session can't guess the password through here.
- Rules: at least 8 characters (registration's), not the current one.
- The current password is checked against a plain read and the new one hashed with nothing locked; the write
  goes through only if the stored hash is still the one checked.
- In one transaction: new hash, `users.sessions_valid_after = now` (every older resume token stops working),
  every other device signed out and revoked as above, this device's `signed_in_at = now`.
- Other connections get `SESSION_CLOSED {PASSWORD_CHANGED}`; their sign-in page says the password was changed
  on another device. This device gets a new resume token, and native puts the new password in its saved
  sign-in (if one is saved) and in memory for reconnects.
- The E2EE passphrase is separate and unchanged (the dialog says so).

## Grace period for installs from before devices

Migration 6 records `central_schema.devices_since`. For 30 days after it, an automatic sign-in from a device
the account doesn't have yet is recorded instead of refused, unless that device id was signed out meanwhile
(`revoked_devices` keeps rows 31 days for this check). Without it, every saved sign-in would be refused right
after the deploy: resume tokens from before carry no `did` and fall back to the saved password, which would
find no device. After the grace period, a device without a row types the password once.

## Housekeeping and limits

- A device unused for 90 days is forgotten (hourly sweep): every token it held has long expired; its saved
  password no longer signs it in.
- At most 100 devices per account: a new one past that signs out the least recently used (each needs the
  password, so this only bounds made-up device ids).
- Revocations are kept 31 days; a heartbeat answer carries at most 2,000.
- A password sign-in now requires a device id (plain ASCII, at most 64 bytes): every signed-in install is a
  device the account can see. An older client without one is told to update.

## The new sign-in notice

A **typed** sign-in that creates a device row sends `DEVICES_CHANGED {new_device}` to the account's other
connections; the client shows "New sign-in to your account: macOS · 176.88.4.203. If this wasn't you, sign
that device out and change your password." with a "Review devices" button (20 s). Automatic sign-ins
(including the grace-period ones after the deploy) and typed sign-ins on a known device only refresh lists.

## Wire changes (`proto/messages.proto`)

- `LoginRequest`: `device_name` (6), `device_platform` (7), `app_version` (8), `automatic` (9).
- `LoginResponse`: `signed_out` (20), `devices` (21: this central serves all of this; the client hides the
  Devices tab and Change password without it).
- `SessionClosed.Reason`: `PASSWORD_CHANGED` (2).
- Packets 172–178: `DEVICE_LIST_REQ/RES`, `DEVICE_SIGN_OUT_REQ/RES`, `DEVICES_CHANGED`,
  `PASSWORD_CHANGE_REQ/RES` (payload fields 174–180; `DeviceInfo`, `DeviceListRequest/Response`,
  `DeviceSignOutRequest/Response`, `DevicesChanged`, `PasswordChangeRequest/Response`).
- `ServerHeartbeatRes.revoked` (2, `RevokedSession {uid, sid, until}`).
- Session tokens carry `sid` (the device's id); resume tokens carry `did` (its device id).
- Community: `DECIBELL_CENTRAL_PORT` (default 8080) and `DECIBELL_HEARTBEAT_INTERVAL_SECONDS` (default 60),
  for the test harness's stand-in central.

Ships with the multi-device central and client (pre-1.0, together). The community change is safe either
way: an older central sends no revocations and tokens without `sid`.

## Client

- Native: `ThisDevice` (net/central.rs) fills every `LoginRequest`; typed sign-ins send `automatic: false`,
  the saved sign-in and reconnect fallbacks `true`. `signed_out` on a refused login, `SIGNED_OUT` /
  `PASSWORD_CHANGED` close notices → `signed_out_elsewhere(reason)`: the full sign-out (as Log out) with the
  reason in `logged_out`. Commands `list_devices`, `sign_out_devices`, `change_password`
  (`commands/devices.rs`); `logout` asks central to forget this device first. Event `devices_changed`.
- Renderer: Settings → Devices (`tabs/DevicesTab.tsx`): this device, the others with Sign out each (plain
  `ConfirmModal`), "Sign out of all other devices" when there are two or more. Account → Change password
  (`ChangePasswordModal.tsx`, the rename modal's chrome). The sign-in page shows the reason it was reached.
  The toast store takes an optional action button.

## Tests

Central e2e: the list (both devices, current, online, platform, version, address; a disconnected device stays,
offline, seen now; another account sees only its own), the session token's `sid`, the new sign-in notice
(typed new device only; not to the device itself; not for automatic or known devices), a resume keeps its
device and refuses another device id, a renewal for a device no longer signed in closes it; signing out one
device (SIGNED_OUT, others stay and are told, gone from the list, its resume and saved password refused as
signed out, typing signs it in as a new device), not itself by id, not another account's, oversized
requests, all other devices, logging out; the grace period (automatic sign-in recorded during it, refused
after it, typed always fine, a wrong password stays a wrong password); password change (wrong / short / same
refused, others closed with PASSWORD_CHANGED, this one carries on, old password and old resume tokens fail,
the new ones work, other devices' resumes refused as signed out, ten wrong attempts hold sign-ins from that
address only); heartbeat revocations (members' communities only, `until` 25 h out, password changes too);
migration to schema 6. Community e2e (stand-in central on its own port, 1 s heartbeats): a revoked device's
session closes within a heartbeat and its tokens are refused, the account's other sessions (and a token
without `sid`) stay, a renewal onto a revoked token is ignored, a revocation lapses at `until`, an expired
token ends its session and a renewed one doesn't. Native unit tests: os-release parsing, `LoginRequest`
fields per kind of sign-in.

## Not done

- Renaming a device, or showing a location for the address (no GeoIP).
- Email notices or recovery: a thief who knows the password can still change it first. The new sign-in
  notice reaches the owner's devices only while one is signed in.
- Requiring the password to sign devices out (Discord doesn't; a stolen resume token can sign the owner's
  devices out, but the owner signs back in and changes the password, which the thief can't).
