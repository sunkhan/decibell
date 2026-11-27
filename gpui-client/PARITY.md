# GPUI client parity checklist

One row per row of `docs/FEATURES.md` §1–§9 (the Electron client at 0.9.4 is the reference). A row is **done**
only when it was run in this client against the local stack (`tools/dev/local-stack.sh`), with a self-test where
one is possible; **needs owner** means it is built and everything that can be checked on one Linux machine
passed, and the row says exactly what is left for the owner to test (two machines, other hardware, the eye).

Status values: `not started` · `in progress` · `done` · `needs owner`.
Evidence: the self-test (`decibell --selftest <name>`), unit test or capture that shows it; commit where useful.
"Mirrors" keeps the catalogue's shorthand: `R/` = `electron-client/src/`, `M/` = `electron-client/electron/main/`,
`N/` = the native code (now `crates/decibell-core/src/` for `net/ media/ e2ee/ hotkeys/ config.rs presence.rs
system_idle.rs state.rs`, `electron-client/native/src/` for `commands/`), `C/` = `src/community/`, `Z/` = `src/server/`.


## 1. Accounts, sign-in and devices (central)

Audited 2026-11-27 against the branch head `51be7d71` (the frozen build in `~/.cache/decibell-dev/parity-root`);
gaps and their sizes: `docs/parity-audit-a.md`. Status here is `done` / `needs owner` / `partial`. Unit tests are
cited from the code and were not rerun (no builds in the audit); self-test results are this audit's runs.

| # | Feature | Status | Evidence / what is left | Windows | Mirrors |
|---|---|---|---|---|---|
| 1.1 | Register | partial | **The sign-up form can't be typed into**: `AuthView`'s fields have no editor (`decibell-shell/src/auth.rs` `field()`; nothing calls `set_field`), so nobody can register from the app. The action, the result line and the lower-casing exist (`actions::auth::register`, unit test `sign_in_page_states`); every throwaway account of `settings-password` / `settings-rename` / `settings-e2ee` / `friends-flow` / `dm-*` registered through the action (all PASS 2026-11-27). No self-test drives the page | owner: no step yet | `R/features/auth/`, `N/commands/auth.rs`, `Z/auth_manager.cpp` |
| 1.2 | Sign in | partial | **The sign-in page can't be typed into** (same cause as 1.1): only a saved sign-in or the development `--login` signs in. Errors under the form, "Can't reconnect" toast when signed in, the re-trust prompt on a pin failure: implemented (`events/auth.rs`, `actions::auth::sign_in`). Self-tests sign in through the action or `--login`: `shell-signin` PASS 2026-11-27; none types into the page | owner: no step yet | `N/net/central.rs`, `Z/abuse_limits.hpp` |
| 1.3 | Stay signed in | partial | Saved sign-in sealed under the keychain key and the automatic sign-in at start: `shell-no-keyring` (no Secret Service: saved and used on the next start) PASS, `keyring-label` (label `Decibell`, private keyring) PASS, `shell-signout` (cleared) PASS, all 2026-11-27. Untested: resuming after a dropped central connection (the core's resume token; the model's `again` path has unit test `a_new_sign_in_drops_loaded_conversations_but_keeps_unconfirmed_sends`), and a signed-out device's saved password refused at its next start | owner: checks §7 (Keyring) | `N/config.rs`, `M/e2eeLocalKey.ts`; spec `2026-10-15-central-hardening-design.md` |
| 1.4 | Several devices | partial | Two perf1 devices online together (`settings-devices-signout` PASS), read state across devices (`dm-read-sync`, `dm-read-rules` PASS), all 2026-11-27. Untested: the 8-device refusal (`device_limit` → error / "Can't reconnect"), status / custom status / friends-only and own DM echoes reaching the account's other device, friend-list and membership pushes, own rename / avatar on the other device, `REPLACED` | owner: no step yet | spec `2026-10-18-multi-device-design.md` |
| 1.5 | Device list | partial | `settings-devices-signout` PASS 2026-11-27: a second perf1 device appears, Sign out → confirm (Enter) → it leaves the list and the signed-out client shows the sign-in notice. Untested: "Sign out of all other devices" (needs two others), the "New sign-in to your account" toast and its Review devices action (unit test `a_new_device_is_announced_and_sign_out_clears_people` only), a signed-out device's community sessions ending | owner: no step yet | `R/features/settings/tabs/DevicesTab.tsx`, `N/commands/devices.rs`; spec `2026-10-19-devices-and-password-design.md` |
| 1.6 | Change password | partial | `settings-password` PASS 2026-11-27: typed through the dialog, the length check, the other device signed out with its notice, this one stays, the new password signs in; checks unit-tested (`titles_and_password_checks`). Untested: a wrong current password refused (central's text in the dialog) | owner: no step yet | `R/features/settings/ChangePasswordModal.tsx` |
| 1.7 | Change username | partial | `settings-rename` PASS 2026-11-27: the same-name check, lower-cased as typed, the account, names store and shell follow; rules unit-tested (`dialogs/rename.rs`). Untested: central's refusals (two a day, a taken name), the old name still signing in, the rename reaching a joined community live (`SESSION_TOKEN_UPDATE`) and the account's other device / watchers (`USER_RENAMED`) | owner: no step yet | `R/features/settings/RenameUsernameModal.tsx`; spec `2026-10-15-uid-identity-design.md` |
| 1.8 | Log out | partial | `shell-signout` PASS 2026-11-27 (back on the sign-in page, saved sign-in gone), but it calls `actions::auth::sign_out`, not Settings → Account's Log Out (`actions::auth::log_out`, which hangs up a call first). Untested: central forgetting the device, a call / voice / stream stopped by Log Out | owner: no step yet | `N/commands/devices.rs::forget_this_device` |
| 1.9 | Certificate pinning | partial | "Certificate changed" dialog over the sign-in page and the main layout (`decibell-shell/src/overlays.rs`), offered from sign-in, the join dialog and Browse; unit tests `cert_mismatch_is_taken_apart`, `fingerprints_read_like_the_renderer`. No self-test drives a mismatch. Differs: an invite card's Join doesn't join again after trusting (no retry, `actions::servers::redeem_invite`); the failure toast says "Couldn't trust the certificate" (Electron "Couldn't re-trust certificate") | owner: no step yet | `N/net/pins.rs`, `N/net/tls.rs`, `R/components/CertMismatchModal.tsx` |

## 2. People: profiles, presence, friends

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 2.1 | Profile picture | not started | | `R/components/pictureEditor/`, `R/stores/avatarStore.ts`; spec `2026-10-28-picture-editor-design.md` |
| 2.2 | Profile popup / screen | not started | | `R/features/dm/UserProfilePopup.tsx`, `R/features/profile/` |
| 2.3 | Status | not started | | `R/features/presence/`; spec `2026-10-11-user-status-design.md` |
| 2.4 | Auto-away | not started | | `R/features/presence/useAutoAway.ts`, `N/system_idle.rs` |
| 2.5 | Custom status message | not started | | spec `2026-10-12-status-messages-design.md` |
| 2.6 | Presence delivery | not started | | `R/features/presence/presenceInterest.ts` |
| 2.7 | Friends | not started | | `R/features/friends/`, `N/commands/friends.rs` |
| 2.8 | Server nicknames | not started | | `R/hooks/useDisplayName.ts` |

## 3. Direct messages and calls (central)

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 3.1 | Persistent DMs | not started | | `R/features/dm/`, `R/stores/dmStore.ts`; spec `2026-05-14-persistent-dms-design.md` |
| 3.2 | Send / edit / delete / reply | not started | | `N/commands/dm.rs` |
| 3.3 | Friends-only DMs | not started | | `R/features/settings/tabs/PrivacyTab.tsx` |
| 3.4 | End-to-end encryption | not started | | `N/e2ee/`, `R/features/e2ee/`; spec `2026-09-03-e2ee-dms-design.md` |
| 3.5 | Unread-DM tiles | not started | | `R/features/servers/ServerBar.tsx` |
| 3.6 | Voice calls | not started | | `R/features/call/`, `N/commands/call.rs`, `N/media/{media_socket,call_crypto,stun,punch}.rs`; specs `2026-08-28-p2p-dm-calls-design.md`, `2026-09-03-authenticated-p2p-calls-design.md` |
| 3.7 | Screen share in calls | not started | | `R/features/call/CallStage*`; live test pending |

## 4. Communities (community server)

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 4.1 | Self-hosting | not started | | `C/`, `ARCHITECTURE.md` §1 |
| 4.2 | Joining | not started | | `R/features/servers/ServerBrowseView.tsx`, `DeepLinkJoinModal.tsx` |
| 4.3 | Memberships | not started | | spec `2026-05-14-auto-rejoin-communities-design.md` |
| 4.4 | Server bar | not started | | `R/features/servers/ServerBar.tsx` |
| 4.5 | Server picture | not started | | spec `2026-05-15-custom-server-pictures-design.md` |
| 4.6 | Invites | not started | | `R/features/servers/InviteModal.tsx`, `InviteFriendsModal.tsx`, `inviteLink.ts`, `R/features/chat/InviteEmbed.tsx`; spec `2026-10-14-welcome-banner-actions-design.md` |
| 4.7 | Welcome banner | not started | | `R/features/chat/WelcomeState.tsx` |
| 4.8 | Leave | not started | | `R/components/LeaveServerConfirmModal.tsx` |
| 4.9 | Server settings | not started | | `R/features/servers/ServerSettingsModal.tsx`, `R/features/servers/tabs/` |
| 4.10 | Transfer ownership | not started | | `R/features/servers/tabs/OverviewTab.tsx` |
| 4.11 | Moderation | not started | | `C/main.cpp`, spec `2026-08-22-server-management-moderation-design.md` |
| 4.12 | Member list | not started | | `R/features/members/`; spec `2026-10-18-roster-paging-design.md` |

## 5. Channels, roles and permissions

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 5.1 | Channel types | not started | | `R/features/channels/` |
| 5.2 | Channel settings | not started | | `R/features/servers/ChannelSettingsModal.tsx`, `channelSettings/` |
| 5.3 | Topics | not started | | `R/features/servers/channelTopic.ts` |
| 5.4 | Encrypted text channels | not started | | `N/e2ee/channel_keys.rs`; spec `2026-09-04-encrypted-text-channels-design.md` |
| 5.5 | Retention | not started | | `C/db.cpp` |
| 5.6 | Roles | not started | | spec `2026-10-09-permissions-v3-design.md` |
| 5.7 | Permission bits | not started | | `C/authz.hpp`, `R/features/servers/permissions.ts` |
| 5.8 | Channel overwrites | not started | | spec `2026-08-22-permissions-v2-design.md` |
| 5.9 | Category sync | not started | | spec `2026-10-09-permissions-v3-design.md` |

## 6. Text chat

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 6.1 | Message list | not started | | `R/features/chat/RealMessageList.tsx`; spec `2026-08-25-real-dom-message-list-plan.md` |
| 6.2 | Formatting | not started | | `R/features/chat/richText.ts`, `CodeBlock.tsx`, `MathTex.tsx`, `RichComposer.tsx` |
| 6.3 | Emoji | not started | | `R/features/chat/EmojiPicker.tsx`, `R/components/emoji/` |
| 6.4 | GIFs | not started | | `R/features/chat/GifPicker.tsx`, `M/gifs.ts` |
| 6.5 | Link previews | not started | | `M/linkPreview.ts`, `R/features/chat/LinkEmbeds.tsx` |
| 6.6 | Attachments | not started | | `R/features/chat/uploadAttachment.ts`, `AttachmentList.tsx`, `C/attachment_http.cpp` |
| 6.7 | Media playback | built, not yet matched against captures (`docs/media-design.md` §1 lists what follows the renderer and §8 what is missing: the controls' fade, the last-frame poster, 10-bit video, rotation) | `decibell-media`, `decibell-chat-content/src/playback.rs` | `R/features/chat/PersistentVideoLayer.tsx`, `PersistentAudioLayer.tsx` |
| 6.8 | Image viewer | not started | | `R/features/chat/imageViewer/`, `ImageViewer.tsx` |
| 6.9 | Transfers panel | not started | | `R/features/transfers/`, `M/downloads.ts`, `M/downloadPacer.ts`; spec `2026-10-04-transfers-panel-design.md` |
| 6.10 | Edit / delete | not started | | spec `2026-05-15-message-deletion-design.md` |
| 6.11 | Replies | not started | | `R/features/chat/ReplyPreview.tsx` |
| 6.12 | Send pacing | not started | | `R/features/chat/sendPacing.ts` |
| 6.13 | Slowmode | not started | | `R/features/chat/slowmode.ts`, `SlowmodeIndicator.tsx` |
| 6.14 | Drafts | not started | | `R/stores/draftsStore.ts` |
| 6.15 | Message search | not started | | `R/features/search/`, `C/search.cpp`; spec `2026-10-14-message-search-design.md` |
| 6.16 | Time format | not started | | `R/lib/timeFormat.ts` |

## 7. Voice channels

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 7.1 | Join / leave / switch | not started | | `R/features/voice/`, `N/commands/voice.rs` |
| 7.2 | Sidebar rosters | not started | | `R/features/channels/` |
| 7.3 | Voice view | not started | | `R/features/voice/stage/`; spec `2026-10-04-voice-view-redesign.md` |
| 7.4 | Audio processing | not started | | `N/media/pipeline.rs`, `jitter.rs` |
| 7.5 | Input modes | not started | | `N/media/voice_gate.rs`, `R/features/settings/tabs/AudioTab.tsx` |
| 7.6 | Devices | not started | | `N/media/audio_device.rs`, `default_device_watch.rs` |
| 7.7 | Per-user controls | not started | | `R/features/voice/` |
| 7.8 | Self / server state | not started | | `C/main.cpp` |
| 7.9 | Encryption | not started | | `N/e2ee/group.rs`, `N/media/frame_crypto.rs`; spec `2026-09-03-mls-voice-channel-encryption-design.md` |
| 7.10 | Connection stats | not started | | `R/features/voice/ConnectionStatsPopover.tsx`, `R/stores/voiceStatsStore.ts` |
| 7.11 | Global hotkeys | not started | | `N/hotkeys/`, `M/hotkeys.ts`, `R/features/hotkeys/`; spec `2026-10-10-global-hotkeys-design.md` |

## 8. Screen sharing (streams)

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 8.1 | Go Live | not started | | `R/features/voice/CaptureSourcePicker.tsx`, `goLive/` |
| 8.2 | Capture + encode | not started | | `N/media/capture_*.rs`, `encoder*.rs`, `R/features/voice/streaming/StreamCapture.ts` |
| 8.3 | Stream audio | not started | | `N/media/capture_audio_{pipewire,wasapi}.rs`; spec `2026-09-02-multi-app-audio-picker-design.md` |
| 8.4 | Watching | not started | | `R/features/voice/StreamVideoPlayer.tsx`, `stage/streamActions.ts` |
| 8.5 | Mini-player | not started | | `R/features/voice/StreamPipManager.tsx`, `streamPipHost.ts`, `MiniStreamPlayer.tsx` |
| 8.6 | Thumbnails | not started | | `N/media/thumb_encode.rs` |
| 8.7 | Take stream with me | not started | | `R/features/settings/tabs/CodecsTab.tsx` |
| 8.8 | Encryption | not started | | `N/media/frame_crypto.rs` |

## 9. App, settings and shell

| # | Feature | Status | Evidence / what is left | Mirrors |
|---|---|---|---|---|
| 9.1 | Settings → Account | not started | | `R/features/settings/tabs/AccountTab.tsx` |
| 9.2 | Settings → Devices | not started | | `DevicesTab.tsx` |
| 9.3 | Settings → Appearance | not started | | `AppearanceTab.tsx`, `R/styles/globals.css` |
| 9.4 | Settings → Privacy | not started | | `PrivacyTab.tsx` |
| 9.5 | Settings → Audio | not started | | `AudioTab.tsx` |
| 9.6 | Settings → Keybinds | not started | | `KeybindsTab.tsx` |
| 9.7 | Settings → Network | not started | | `NetworkTab.tsx` |
| 9.8 | Settings → Codecs | not started | | `CodecsTab.tsx` |
| 9.9 | Settings → About | not started | | `AboutTab.tsx` |
| 9.10 | Settings windows | not started | | `R/components/SettingsWindow.tsx` |
| 9.11 | Auto-update | not started | | `M/update.ts` |
| 9.12 | Deep links | not started | | `M/index.ts`, `R/features/servers/DeepLinkJoinModal.tsx` |
| 9.13 | Taskbar flash | not started | | `M/window.ts` |
| 9.14 | Crash reports | not started | | `M/sentry.ts`, `R/lib/sentry.ts` |

94 rows.
