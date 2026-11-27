# Parity audit A: §1 accounts, §2 people, §3 DMs and calls (2026-11-27)

What the native client lacks or leaves untested in `docs/FEATURES.md` §1–§3, against the Electron client.
The checklist rows are in `gpui-client/PARITY.md` (1.1–3.7).

- **Build audited**: the frozen release build in `~/.cache/decibell-dev/parity-root/target/release/` (`dev-local`,
  built 12:06 from the branch head `51be7d71`, assumed: the directory carries no commit). Code was read at
  `51be7d71`. Work on `wip/integrate-d` (15 commits past the head) is cited where it already closes a gap. It is
  not merged, so it doesn't count.
- **Method**: each row was split into its rules. For each rule the renderer's code was read, the Rust port found,
  and the test that asserts it read through. Rows 2.2, 2.5 and 2.8 were compared rule by rule by a delegated
  reader; its main claims were checked against the code (the popup's anchor, the one-line status, the
  system-font emoji, the lost quick DM, the missing member lookups).
- **Self-tests**: every test that is evidence for a row ran on its own against the local stack. The first round
  called the binary directly. Every failure was then rerun through `gpui-client/tools/selftest.sh`, which holds
  one test window on the machine at a time. Unit tests are cited from the code and were **not rerun**: the audit
  builds nothing. The branch's last full run passed them (HANDOFF "Resume here").
- **The frozen build has no `tools/perf` above it.** In `parity-root`, `attention` SKIPs: it needs the KWin bridge
  `tools/perf/linux/kwin.py`. `dm-read-rules` skips its minimised case for the same reason (`winctl.sh`). Both
  still exit 0, and `selftest.sh` then prints PASS. For the real result I ran the same binary (a hard link,
  same inode) from `~/.cache/decibell-dev/scratch/parity-a/root/`, which has `tools -> <worktree>/tools`.

## Summary

| Section | done | needs owner | partial |
|---|---|---|---|
| §1 Accounts, sign-in and devices (9 rows) | 0 | 0 | 9 |
| §2 People (8 rows) | 0 | 0 | 8 |
| §3 DMs and calls (7 rows) | 1 (3.5) | 0 | 6 |

Most rows are well ported: the renderer's strings, rules and edge cases are in the Rust, often line for line.
They stay `partial` for one of three reasons. Four rows have a real product gap, below. Several differ from
Electron in small ways nobody recorded. Many rules have no test, although one against the local stack is
possible.

### Self-test results (2026-11-27)

| Test | Result | Rows |
|---|---|---|
| shell-signin, shell-signout, shell-no-keyring, keyring-label | PASS | 1.2, 1.3, 1.8, 3.4 |
| settings-devices-signout, settings-password, settings-rename, settings-e2ee, settings-privacy-dms | PASS | 1.4–1.7, 3.3, 3.4 |
| picture-editor, profile-popup, status-custom, join-status-picker, user-menu-moderation | PASS | 2.1, 2.2, 2.5, 2.8 |
| friends-flow, friends-page, home-idle | PASS | 2.7 |
| dm-open-history, dm-send-receive, dm-read-sync, dm-e2ee | PASS | 3.1–3.5 |
| dm-read-rules | PASS: direct (minimised case skipped) and wrapped from the mirror (all four cases) | 3.1, 3.5 |
| call-ring | PASS | 2.3, 3.6 |
| attention | parity-root: SKIP (no KWin bridge), twice. Mirror: FAIL once in a direct run ("the attention request: not within 10 s"), then PASS ×3 (2 direct, 1 wrapped) | 2.3, 3.6 |
| idle-x11 | FAIL once in a direct run ("0 x11-idle threads while on"), then PASS ×3 (2 direct, 1 wrapped) | 2.4 |
| **join-dm-composer** | **FAIL ×4** (3 direct, 1 wrapped): "type-to-focus with nothing focused" (a key with nothing focused: the box holds "", focused false) | 3.2 |

## 1. Product gaps (fix first)

**G1. The sign-in and sign-up page takes no typing (1.1, 1.2). Size: M.**
- **Rule.** You type a username, an email and a password; sign-up lower-cases the username as you type; Enter submits.
- **Electron.** `R/features/auth/LoginPage.tsx:72-112`: three `<input>`s, sign-up lower-cases in `onChange` (:78-80), submit at :43-47.
- **Native.** `decibell-shell/src/auth.rs:130-143`: `field()` builds `TextField::new(id).focus(focus).value(value)` with no `.editor(…)`, and nothing calls `AuthView::set_field`. The file's own header (:3-6) still says the editor is to come. Clicking a field focuses it, and typing does nothing. Only a saved sign-in or the development `--login` can sign in, so a new user can't register and a signed-out user can't sign back in.
- **To close.** Three `InputState`s (`InputState::password` for the password) in the kit's `TextField::editor`, the register-mode lower-casing as `set_field` does it, and `submit` reading the states. Add a self-test that types into both forms: perf1 with a wrong password (the error line), perf1 right, a throwaway registration (the success line).

**G2. Settings → Account's picture buttons do nothing (2.1). Size: S, by merging.**
- **Rule.** "Change picture" / "Add a picture", the picture button, or a picture dropped on the card opens the picture editor; Save uploads the picture.
- **Electron.** `R/features/settings/tabs/AccountTab.tsx:35-37, 144`, `AvatarCropperModal`, `usePictureDrop`.
- **Native.** At the head, `decibell-settings/src/tabs/account.rs:104-107` emits `PictureEvent::ChangeAvatar` on `PictureRequests`, and nothing subscribes.
- **To close.** `a37e00a4` on `wip/integrate-d` (the join, the upload action, the drop veil, self-test `avatar-editor`) closes it once merged.

**G3. Typing anywhere doesn't reach the DM composer (3.2). Size: S, by merging.**
- **Rule.** With nothing focused, a printable key goes into the open conversation's message box.
- **Electron.** The composer's type-to-focus.
- **Native.** `join-dm-composer` fails 4 of 4: the root view now holds the focus, so the composer's interceptor never runs.
- **To close.** `316b5adb` on `wip/integrate-d`, unmerged; then rerun `join-dm-composer` and `composer-type-to-focus`.

**G4. On Wayland, auto-away comes up to one threshold late (2.4). Size: M.**
- **Rule.** Away after the threshold without input. Electron reads the idle state every 15 s, so it is at most 15 s late (`R/features/presence/useAutoAway.ts:16-20, 54-75`).
- **Native.** `decibell-model/src/actions/presence.rs:87-101`: `CoreIdleSource` answers `Active { idle_for: None }`, and `pushes_resume` is false. `live/presence/auto_away.rs:208` then reads again one full threshold later. If you stop just after a read, Away shows after up to twice the threshold: 20 min for the default 10. The core already pushes the compositor's idle edges (`api::presence::subscribe_system_idle`, `system_idle.rs:19-36`), but the model never subscribes and drops `SystemIdleChanged` (`events/mod.rs:95-97`, "doesn't call yet"). This is the owner's KDE Wayland session.
- **To close.** Subscribe with the threshold whenever it changes. Turn `SystemIdleChanged` into `AutoAway` readings, idle → Away at once and resumed → poke. Make the source `pushes_resume` when subscribed. Unit-test it with a fake pushing source (the rig exists). The live check needs a compositor with ext-idle-notify: the owner's session, or a nested KWin / Sway in a self-test.

**G5. Names in a community fall back to the account name where the member isn't loaded (2.8). Size: M.**
- **Rule.** Every name inside a server shows the server nickname, looking the member up if needed.
- **Electron.** `R/hooks/useDisplayName.ts:42-79`; every caller looks the member up.
- **Native.** `display_name` without `want_member` in search results (`decibell-search/src/results.rs:803-804`), the audit log (`decibell-server-settings/src/server/audit.rs:238-240`), reply lines (`decibell-chat/src/list/view.rs:539`), voice tiles, stream view and mini-player (`decibell-voice/src/tiles.rs:109, 337, 785`, `focus.rs:365`, `mini.rs:314`, `pane.rs:400`). An offline member of a big community shows their account name there. Also, a reply line isn't redrawn when the replied-to author's nickname changes: the roster observer at `view.rs:264-283` re-checks only the header names.
- **To close.** A lookup at those sites, the way `view.rs:525` does it for authors, and the reply line in the roster observer. Add a self-test: search for an offline member with a nickname in Daily Gaming.

**G6. Smaller differences, none recorded in `docs/electron-quirks.md` or CLAUDE.md. Each is S unless marked.**
- **Profile popup anchor (2.2).** From a chat author or reply name, the popout opens at the pointer + 8 px (`decibell-chat/src/list/view.rs:1536-1543`). Electron opens it at the clicked element's right edge (`R/features/chat/MessageBubble.tsx:349-357`; `ReplyPreview.tsx`). The comment in the Rust claims Electron's rule.
- **Quick DM from the popout (2.2).** `decibell-home/src/profile/popup.rs:175-189` ignores `send_dm`'s outcome. When that conversation is scrolled back in history, `SendOutcome::JumpedToPresent` (`actions/dm.rs:294-297`) sends nothing, yet the field clears and the popup closes: the message is lost. Electron sends without a bubble and, on failure, keeps the popup open with its text (`UserProfilePopup.tsx:222-246`).
- **Custom status, one line (2.2, 2.5).** It is cut to one line in the popout and the profile screen (`decibell-common/src/person.rs:307-313` `.truncate()`). Electron wraps it there (`StatusMessageText.tsx:36`), and where it is cut, shows the whole text on hover (`title`, :38). Native has no hover text.
- **Emoji font (2.5, 3.1).** Status emoji and DM-list previews are drawn by the system emoji font, not Twemoji (`person.rs:257-262` says so; `decibell-shell/src/dm_rows.rs:116` draws the preview as a plain `Label`). Electron draws Twemoji in both (`StatusMessageText`, `MessageText preview`). Size: M (inline emoji in `Label`), or record it as a decision.
- **Selectable text (2.2, 3.4).** On the profile screen, the About values, the safety number and the fingerprints can't be selected. Electron's are `select-text` (`UserProfileModal.tsx:287, 493, 553`), which is the only way to copy a fingerprint there.
- **Native-only menu entries (2.2, 2.7, 2.8).** The right-click user menu adds Profile, Message, the friend actions incl. **Block**, and "Set nickname" on yourself (`decibell-home/src/menu/mod.rs`). Electron's `UserContextMenu.tsx` holds voice entries only, and Electron's renderer can't block anyone at all (FEATURES 2.7 says "block / unblock"). Block in the menu is already an open owner decision (HANDOFF); the rest isn't listed: add it there.
- **Certificate prompt (1.9).** An invite card's Join doesn't join again after "Trust new certificate". `decibell-chat-content/src/embeds.rs:843` calls `actions::servers::redeem_invite`, which offers the prompt with no retry (`decibell-model/src/actions/servers.rs:117`). Electron's `InviteEmbed.tsx:109` passes `join`. The failure toast says "Couldn't trust the certificate" (`servers.rs:174`), where Electron's says "Couldn't re-trust certificate" (`CertMismatchModal.tsx:24`).
- **Refusals from an older central (3.3).** A refusal without a nonce is a plain bubble of ours. Electron draws the "Can't reach this user." / "User is offline." card (`DmChatPanel.tsx:41-44, 761-781`). `35a5dee0` on `wip/integrate-d` draws the card. Refusals with a nonce, the current central's, already match: bubble withdrawn, "Message not sent" toast, in both clients.
- **"Today" in the custom status (2.5).** Where local midnight falls in a DST gap, "Today" becomes 24 h (`decibell-home/src/status.rs:59-64` with `regional` `earliest()`).

## 2. Missing tests (possible on this machine, against the local stack)

The headless client already has the steps most of these need: `register`, `login-as`, `dm-send`, `dm-delete`,
`status`, `invisible`, `dnd`, `away`, `friend-*`, `call`, `answer`, `hangup`, `voice-join`, `stream-file`,
`device-sign-out`, `devices`.

| # | Row | What to test | Size |
|---|---|---|---|
| T1 | 1.1, 1.2 | The page typed into (part of G1) | with G1 |
| T2 | 1.3 | Resume after a dropped central connection. The client goes through a loopback TCP proxy that the test cuts; assert it signs back in without the password and the open conversation reloads | M |
| T3 | 1.3, 1.5 | A device signed out by another, restarted on its profile: lands on the sign-in page, its saved password refused (`signed_out`) | S |
| T4 | 1.4 | The 9th device of a throwaway account (8 headless + the app): the refusal text on the sign-in page; as a reconnect, the "Can't reconnect" toast | M |
| T5 | 1.4 | A second (headless) device of the account changes status, custom status and friends-only, sends a DM, adds a friend, joins a community: the app follows each | M |
| T6 | 1.5 | "Sign out of all other devices" with two others: confirm, toast "2 other devices were signed out.", both gone | S |
| T7 | 1.5 | A headless password sign-in on a new device while the app is open: the "New sign-in to your account" toast; Review devices opens Settings → Devices | S |
| T8 | 1.6 | A wrong current password: central's refusal in the dialog, nothing signed out | S |
| T9 | 1.7 | A third rename in a day refused; a taken name refused; the old name still signs in; a community and a watcher see the new name live | M |
| T10 | 1.8 | Settings → Account's Log Out button: the device leaves the account's list (headless `devices`); a running call is hung up and the peer sees it end | S |
| T11 | 1.9 | A wrong pin for a community in the profile's `cert_pins.json`: connect / join → the prompt; Cancel stays off; Trust connects (and the join retries) | M |
| T12 | 2.1 | Remove picture; another client sees the change (after G2, `avatar-editor` covers setting one) | S |
| T13 | 2.2 | The popout's live-stream section (voice-watch's streamer), its Watch; the avatar → profile screen; the screen's friend actions; the Privacy tab with two E2EE accounts (safety number, Copy) | M |
| T14 | 2.3 | Each status chosen in the menu; a headless watcher sees Away / DND / offline for Invisible; Invisible listed offline in a community roster; calling an invisible user → "Not available" | M |
| T15 | 2.4 | Lock / unlock through a private bus's `org.freedesktop.ScreenSaver` `ActiveChanged` → Away / back; Away shown to others only when both devices idle (headless `away`) | M |
| T16 | 2.5 | The 128-character cap and counter; the status menu's edit and clear; a 60 s clear-after reaching a watcher; hidden while invisible | S |
| T17 | 2.6 | A member row drawn → that user is watched; their status change redraws the dot; scrolled away → released (the interest set read in the app) | S |
| T18 | 2.7 | Decline, Cancel, Unblock, Message by click; the inline error for an unknown name and for yourself, gone after 5 s; the account's other device follows | S |
| T19 | 2.8 | alice sets her own nickname in Server Settings → Members (Daily Gaming), the entry absent where @everyone lacks Change Nickname (Rustaceans); a non-owner with Manage Nicknames; a refusal | M |
| T20 | 3.1 | The DM list's drawn preview text and order after a new message | S |
| T21 | 3.2 | Delete through the row's menu (confirm) and with Shift; the peer's copy disappears; an edit refused by central toasts | S |
| T22 | 3.3 | A DM to a friends-only stranger: the bubble withdrawn, "Message not sent" with central's text | S |
| T23 | 3.4 | Change passphrase; Reset keys → the account's other device locked, the peer's "keys changed" notice; dismiss the set-up nudge; a pinned peer whose keys go never gets plaintext (the send refused) | M |
| T24 | 3.6 | An outgoing call from the DM header answered by a headless peer; BUSY; the 45 s timeout end to end; a second device of the callee showing "Answered on another device"; a call ending a voice session and a voice join ending a call | M |
| T25 | 3.7 | A headless peer shares in a call (check `stream-file` works in a call; add it if not); the app watches; focus, theater, fullscreen, the mini-player; stop → tile gone | M–L |

## 3. Checks only the owner can make

- **3.6** A call between two machines on different networks: STUN, hole punch, the `srflx` path. Listen to the audio for clarity and echo, and hang up on either side.
- **3.7** A screen share in a call between two machines: the picture by eye, theater / fullscreen, the stream's sound.
- **2.4** Auto-away on a real X11 session (HANDOFF §6 already lists it), and on GNOME (Mutter's IdleMonitor). On KDE Wayland once G4 is fixed: Away within a few seconds of the threshold.
- **2.5, 3.1** Whether the system emoji font in statuses and DM previews is acceptable (G6), or must become Twemoji.
- **Rows with an owner step on Windows** (`docs/windows-owner-checks.md`): 1.3, 3.4 (§7 Keyring), 2.3 (§7 Attention, §8 Calls), 2.4 (§7 Screen lock and auto-away), 3.6 (§8 Calls). The Windows CI run (`ci/win-gpui-check-log`, `86c1b34`) ran no test of these rows: they all need the stack.

## 4. Rows with no Windows owner step

1.1, 1.2, 1.4, 1.5, 1.6, 1.7, 1.8, 1.9, 2.1, 2.2, 2.5, 2.6, 2.7, 2.8, 3.1, 3.2, 3.3, 3.5, 3.7.

These are platform-neutral UI over the core, but no Windows run of them exists. One step would cover most of
them: §2's stack from WSL2, then run their self-tests on Windows: `shell-*`, `settings-devices-signout`,
`settings-password`, `settings-rename`, `settings-privacy-dms`, `picture-editor`, `profile-popup`,
`status-custom`, `friends-*`, `user-menu-moderation`, `dm-*`. 2.1 also needs the Windows file dialog for a
picture. 3.7 needs a call share from Windows, which §6 (Go Live in a channel) doesn't cover.

## 5. Noticed outside these rows

- **The wrapper counts a SKIP as a PASS.** `gpui-client/tools/selftest.sh` greps `FAIL|panicked|SKIPPED`, but the tests print `SKIP:`, so a skipped test is listed as PASS. Combined with the frozen build's missing `tools/perf`, `attention` "passes" without running. Suggest: report a log line `SKIP` as SKIP, and link `tools` into `parity-root` (the other auditors' KWin / `winctl.sh` tests skip there too: `single-instance`, `content-copy-image`'s restore step, …).
- **The checklist header.** `PARITY.md`'s "Status values" line still lists `not started · in progress · done · needs owner`, not `partial`. It wasn't changed here because the other sections share it.
- **A mirrored Electron bug, unrecorded.** The popout's CodecBadge is never visible in either client (`UserProfilePopup.tsx:376-385` clips it; mirrored at `popup.rs:281-282`). Record it in `electron-quirks.md`.
- **The popup's live state.** It keeps polling the first community's poster if the person's stream moves while the popup is open (`popup.rs:148-150`), and its "Message @name" placeholder doesn't follow a rename (`popup.rs:109`).
- **Stale comments.** `decibell-app/src/audio.rs:153` still says "TODO(attention) … GPUI has no API" although `decibell-platform::attention` does the flash. `decibell-shell/src/auth.rs:3-6` describes the missing editor as pending.
- **FEATURES row 2.7.** It promises block / unblock, which the Electron UI doesn't offer (only Unblock of an already blocked row).
