# Custom status messages

Status: implemented 2026-10-12 (client + central; communities untouched). The owner asked for a
custom status message "like in most messaging apps, Discord as well", shown under the user's name in
lists and in their profile popup and profile page. Choices made without a separate owner decision are
marked **(default)**; they follow Discord.

## What the user gets

- The status menu on the user panel gets a first row: **Set a custom status**. Once a message is set,
  that row shows the message itself (click to edit) with a × to clear it.
- The dialog has an emoji button (the app's emoji picker), a one-line text field (128 characters,
  counter shown), and **Clear after**: Today (local midnight), 4 hours, 1 hour, 30 minutes, Don't clear.
  The default is Today for a new message and Don't clear when editing one that never clears **(default)**.
- Where it shows:

| Place | How |
|---|---|
| Server member list | second line under the name |
| Friends rail (DM home) | second line under the name |
| Friends page | replaces the "Online" / "Away" line |
| User panel (own) | replaces the status label |
| Profile popup | under the status pill, wrapped |
| Profile page (modal) | under the status pill, wrapped |
| DM header | after the status pill, one line |

- **DM list rows keep the last-message preview** and don't show the status **(default)**. Discord's DM
  list has no message previews; ours does, and that line is more useful there. The DM header shows the
  peer's status instead.
- Others see the message **only while the user is visible** (online, away, do not disturb), like Discord
  **(default)**. Offline and invisible users show none. The user always sees their own, even while
  invisible.

## Wire and central

- `UserPresence.status_text` / `status_emoji`: the message rides presence. It goes in the snapshot
  entries and in every `PRESENCE_DELTA`. Because invisible users are never on the wire, Invisible needs
  no new path and stays airtight.
- `SET_STATUS_MESSAGE_REQ {text, emoji, clear_after}` (client→central, type 153): seconds until it
  clears, 0 = never, clamped to 7 days. Empty text and emoji clears it. Shares the status rate bucket
  (10 burst, 1 per 2 s).
- Central normalises the text with `normalize_status_line`:
  - control characters, C1 and line/paragraph separators become spaces;
  - bidi embeddings, overrides and isolates are dropped;
  - malformed UTF-8 is dropped;
  - the ends are trimmed and at most 128 code points are kept.
  The emoji is cleaned the same way and dropped if it is longer than 32 bytes. The client applies the
  identical rules to its own optimistic copy (`normalizeStatusText`); both were checked against the same
  15 tricky inputs.
- Columns `users.status_text`, `status_emoji` and `status_expires_at` (unix seconds, 0 = never) are
  added at start. They are read at login with the rest of the presence profile. A message that expired
  while its owner was offline is cleared by that read.
- The 30 s stale-session sweep also runs `expire_status_messages()`. It clears and saves each expired
  message, then sends a delta if others could see it.
- `PresenceView` now includes the text and emoji, so a change to either sends a delta only while the
  user is visible.
- `LoginResponse.status_text` / `status_emoji` / `status_clear_in`: the saved message and the seconds
  left. They are relative, so the client's timer for its own copy doesn't depend on the two clocks
  agreeing.

## Client

- `presenceStore`:
  - `messages` holds others' messages. It is kept apart from `statuses` so an edit doesn't regroup
    member lists.
  - `ownMessage` and `ownMessageClearsAt` hold the user's own message, with a timer that drops it when
    central does.
  - `useStatusMessage(username)` returns stable refs.
- `components/StatusMessageText.tsx` renders the emoji and then the plain text. Emoji inside the text
  render as Twemoji; there is no markdown or shortcodes, so the message reads exactly as typed. It
  truncates with a hover title, or wraps in the profile cards.
- `features/presence/CustomStatusModal.tsx` is the dialog. `saveStatusMessage` is optimistic, like
  `chooseStatus`. The emoji picker gained an `at` (fixed viewport position) mode. The dialog portals
  the picker and places it below the field, else above it, else as low as the window allows.
- Row heights don't change when a message appears:
  - member rows have an 18px name and 16px status line, the same as the 34px avatar (48px row);
  - friends rail rows have 18px + 14px lines, the same as the 32px avatar;
  - the friends page and user panel already had two lines.
  All were measured with and without a message.

## Rollout

Ship central with (or before) the 0.8.10 client. On an older central the dialog still opens, but the
message is never saved or relayed. Under the pre-1.0 rule there is no capability flag for that. The
central adds its three columns on start.

## Open

- Live two-account test (set, edit, clear, expiry seen by the other side, invisible hides it).
