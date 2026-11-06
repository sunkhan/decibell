# Decibell feature catalogue

Everything a user can do in Decibell as of **0.9.4 (2026-11-06)**, grouped by area, with the
rules that matter and where each piece lives. Use it as the parity checklist for any new client
(the GPUI exploration included) and as the map from a feature to its design record.

- **How it works on the wire**: `ARCHITECTURE.md`.
- **Why each piece is the way it is**: the design records in `docs/superpowers/specs/` (named per row
  below) and the running feature log `docs/reviews/2026-08-23-community-server-review.md`, which has
  one entry per shipped feature or fix, newest at the bottom.
- **Owner decisions that must not be "fixed"**: CLAUDE.md, "Standing decisions".

Path shorthand: `R/` = `electron-client/src/`, `M/` = `electron-client/electron/main/`,
`N/` = `electron-client/native/src/`, `C/` = `src/community/`, `Z/` = `src/server/` (central).
"Live test pending" marks features built and covered by tests but not yet exercised by real users on
real networks / hardware (see HANDOFF §7 for the full list).

---

## 1. Accounts, sign-in and devices (central)

| Feature | Behaviour | Where |
|---|---|---|
| Register | Username + email + password (bcrypt on central's CPU pool). Sign-ups are rate-limited per address. | `R/features/auth/`, `N/commands/auth.rs`, `Z/auth_manager.cpp` |
| Sign in | Password sign-in returns a 24 h **session token** (Ed25519 JWT, `sub` = name, `uid`, `sid` = device) and a 30-day **resume token** (central-only audience). Failed sign-ins are limited per account *and* per address (account-wide cap 100). | `N/net/central.rs`, `Z/abuse_limits.hpp` |
| Stay signed in | The client saves the password sealed under the OS keychain key (Electron `safeStorage`) and signs in automatically (`LoginRequest.automatic`) — only on a device the account still has. A dropped connection resumes with the resume token, never the password. | `N/config.rs`, `M/e2eeLocalKey.ts`; spec `2026-10-15-central-hardening-design.md` |
| Several devices | Up to 8 installs online together (random `device_id` per install). Same id → replaces its own older connection. Over 8 → refused (`device_limit`), never evicts. What one device does reaches the others (status, DMs, read state, friends, memberships, profile). | spec `2026-10-18-multi-device-design.md` |
| Device list | Settings → Devices: OS name, app version, address, last activity; sign out one device or all others. A signed-out device is closed, refused on resume, and dropped by communities via central's heartbeat (`revoked`). New typed sign-ins notify the other devices. Devices unused for 90 days are forgotten. | `R/features/settings/tabs/DevicesTab.tsx`, `N/commands/devices.rs`; spec `2026-10-19-devices-and-password-design.md` |
| Change password | Confirmed with the current one; signs out every other device; this one keeps a fresh resume token. | `R/features/settings/ChangePasswordModal.tsx` |
| Change username | Settings → Account, password-confirmed, two a day. The old name stays reserved for the account (and still signs it in) for 14 days. Everything keys on the uid, so nothing breaks; a live rename reaches communities as `SESSION_TOKEN_UPDATE` without reconnecting. | `R/features/settings/RenameUsernameModal.tsx`; spec `2026-10-15-uid-identity-design.md` |
| Log out | Asks central to forget this device, stops voice / stream / call, clears the saved sign-in. | `N/commands/devices.rs::forget_this_device` |
| Certificate pinning | Central is trust-on-first-use; communities are pinned to the fingerprint central reports. A mismatch opens a re-trust dialog instead of connecting. | `N/net/pins.rs`, `N/net/tls.rs`, `R/components/CertMismatchModal.tsx` |

## 2. People: profiles, presence, friends

| Feature | Behaviour | Where |
|---|---|---|
| Profile picture | Upload / change / remove through the shared **picture editor** (pan, zoom to fill, theme veil, EXIF-safe crop). Avatars are fetched 6 at a time and cached (≤ 400 off-screen). Colours hash the uid; the letter is the current name's. | `R/components/pictureEditor/`, `R/stores/avatarStore.ts`; spec `2026-10-28-picture-editor-design.md` |
| Profile popup / screen | Anchored popout (identity, live stream, roles, quick DM); its avatar opens the full profile (Profile tab + Privacy tab: safety number, fingerprint, pin state, key-change notice). | `R/features/dm/UserProfilePopup.tsx`, `R/features/profile/` |
| Status | Online / Away / Do Not Disturb / Invisible, owned by central and saved on the account. Invisible is airtight (shown as offline everywhere, including community rosters and call invites). DND only silences the call ring / taskbar flash. | `R/features/presence/`; spec `2026-10-11-user-status-design.md` |
| Auto-away | Per device: after a threshold (default 10 min) without input or talking in voice, plus lock / sleep. The account shows Away only while *every* device is idle. Linux Wayland idle comes from native `ext-idle-notify` / Mutter. | `R/features/presence/useAutoAway.ts`, `N/system_idle.rs` |
| Custom status message | Emoji + up to 128 characters, optional clear-after; visible only while you are visible. | spec `2026-10-12-status-messages-design.md` |
| Presence delivery | Subscription-based: the client tells central which users it is showing; central pushes changes (and avatars / renames / key changes) only to watchers. | `R/features/presence/presenceInterest.ts` |
| Friends | Add by name, accept / reject, remove, block / unblock (unblock = remove); Friends home screen with online / pending / blocked; at most 1000 pending requests. | `R/features/friends/`, `N/commands/friends.rs` |
| Server nicknames | Per community; shown at every name inside that server (`useDisplayName`). Change your own (CHANGE_NICKNAME) or others' (MANAGE_NICKNAMES). | `R/hooks/useDisplayName.ts` |

## 3. Direct messages and calls (central)

| Feature | Behaviour | Where |
|---|---|---|
| Persistent DMs | History paged from central; conversation list with last-message preview and unread count; read state synced across your devices (`DM_READ_SYNC`). | `R/features/dm/`, `R/stores/dmStore.ts`; spec `2026-05-14-persistent-dms-design.md` |
| Send / edit / delete / reply | Own messages only; optimistic faded bubble until central echoes it (`nonce`). | `N/commands/dm.rs` |
| Friends-only DMs | Privacy setting saved on the account, so the gate answers the same whether you are online or not. | `R/features/settings/tabs/PrivacyTab.tsx` |
| End-to-end encryption | When both people have set it up: static X25519 identities, per-message HKDF + AES-256-GCM, passphrase backup on central, trust-on-first-use pins + safety numbers. A pinned peer is never downgraded to plaintext. | `N/e2ee/`, `R/features/e2ee/`; spec `2026-09-03-e2ee-dms-design.md` |
| Unread-DM tiles | Unread conversations appear as tiles at the top of the server bar. | `R/features/servers/ServerBar.tsx` |
| Voice calls | Peer-to-peer over the native UDP engine (STUN + hole punch, AES-256-GCM sealed, keys signed by the E2EE identity; fail closed). Rings every device of the callee; the first answer takes the call. 45 s ring timeout. A call and a voice channel are mutually exclusive. | `R/features/call/`, `N/commands/call.rs`, `N/media/{media_socket,call_crypto,stun,punch}.rs`; specs `2026-08-28-p2p-dm-calls-design.md`, `2026-09-03-authenticated-p2p-calls-design.md` |
| Screen share in calls | Same stream engine as voice channels, announced over central (no thumbnails). The call stage uses the voice view's design: tiles, focus, theater, fullscreen; per-peer local mute + volume. | `R/features/call/CallStage*`; live test pending |

## 4. Communities (community server)

| Feature | Behaviour | Where |
|---|---|---|
| Self-hosting | One C++ process per community: TLS chat on 8082, UDP relay on 8083/8084, attachment HTTPS on 8085, SQLite. Registers with central by heartbeat. | `C/`, `ARCHITECTURE.md` §1 |
| Joining | By invite link / code (resolved by central to host:port) or from the public directory (Browse) when the server is publicly listed. | `R/features/servers/ServerBrowseView.tsx`, `DeepLinkJoinModal.tsx` |
| Memberships | Central keeps your memberships; the client reconnects to every community at sign-in (auto-rejoin) and when another device joins one (`MEMBERSHIP_ADDED`). | spec `2026-05-14-auto-rejoin-communities-design.md` |
| Server bar | 64 px bar of server tiles with pictures (hover-reveal), unread DM tiles, Home. | `R/features/servers/ServerBar.tsx` |
| Server picture | Through the picture editor; distributed by central (`SERVER_PICTURE_CHANGED`). | spec `2026-05-15-custom-server-pictures-design.md` |
| Invites | Create (expiry, max uses), list, revoke; **personal invites** for one user (single use, DM'd by "Invite friends"); links are code-only `decibell://invite/<CODE>` and render as invite cards in chat (name, picture, members, Join). | `R/features/servers/InviteModal.tsx`, `InviteFriendsModal.tsx`, `inviteLink.ts`, `R/features/chat/InviteEmbed.tsx`; spec `2026-10-14-welcome-banner-actions-design.md` |
| Welcome banner | An empty text channel offers Invite friends and Manage channel (the topic is edited in channel settings). | `R/features/chat/WelcomeState.tsx` |
| Leave | Leave the community; the owner can't (transfer ownership first). | `R/components/LeaveServerConfirmModal.tsx` |
| Server settings | Overview (name, description, public listing), Members (server-side search, roles, kick / ban / timeout), Roles (create, colours with RGB picker, hierarchy by drag, permissions, members with counts), Bans (unban), Audit Log (server-side search + action filters), Storage (host disk, this server's footprint, minimum free space). Saves settle on the server's echo; unsaved edits hold the window (shake). | `R/features/servers/ServerSettingsModal.tsx`, `R/features/servers/tabs/` |
| Transfer ownership | Owner only; the picker searches the whole membership on the server. | `R/features/servers/tabs/OverviewTab.tsx` |
| Moderation | Kick, ban (reason, optional expiry, optional purge of recent messages), unban, timeout, server mute / deafen, move between voice channels, disconnect from voice, slowmode, audit log of every action. | `C/main.cpp`, spec `2026-08-22-server-management-moderation-design.md` |
| Member list | Right-hand roster grouped Online / Offline by the community's view (connected and not invisible), paged and windowed (only rows near the viewport are drawn), live deltas. | `R/features/members/`; spec `2026-10-18-roster-paging-design.md` |

## 5. Channels, roles and permissions

| Feature | Behaviour | Where |
|---|---|---|
| Channel types | Text, voice, categories. Create, rename, delete, drag to reorder (channels and categories). | `R/features/channels/` |
| Channel settings | One settings window per channel: overview (name, topic, slowmode, voice bitrate), retention per kind (text / image / video / document / audio, in days), permissions (overwrites; only the bits that apply to the channel type), delete and wipe (plain confirm). | `R/features/servers/ChannelSettingsModal.tsx`, `channelSettings/` |
| Topics | One line ≤ 1024 bytes in the channel header; never encrypted (the settings say so). | `R/features/servers/channelTopic.ts` |
| Encrypted text channels | Chosen at creation, fixed for the channel's life, default off. Epoch keys escrowed to members; attachments sealed client-side; search uses filters only. | `N/e2ee/channel_keys.rs`; spec `2026-09-04-encrypted-text-channels-design.md` |
| Retention | A periodic sweep deletes old messages and attachment blobs per channel setting; purged attachments leave a "cleaned up after N days" tombstone. | `C/db.cpp` |
| Roles | Discord-style: ordered hierarchy, colours, per-role permission bits, "members with this role can manage each other" (nicknames + voice moderation only). Up to 250 roles, 500 channels. | spec `2026-10-09-permissions-v3-design.md` |
| Permission bits | Administrator, manage server / channels / roles / messages / invites / nicknames, kick, ban, moderate (timeout), view audit log, view channel, read history, send messages, attach files, embed links, create invite, change nickname, connect, speak, stream, mute / deafen / move members. Exact bits: `PERM_*` in `proto/messages.proto`. | `C/authz.hpp`, `R/features/servers/permissions.ts` |
| Channel overwrites | Per-role and per-member allow / deny per channel. | spec `2026-08-22-permissions-v2-design.md` |
| Category sync | Opt-in per category ("Channels follow this category's permissions"); dropping a channel into a syncing category asks Sync / Keep / Cancel; only an explicit sync changes what a channel allows. | spec `2026-10-09-permissions-v3-design.md` |

## 6. Text chat

| Feature | Behaviour | Where |
|---|---|---|
| Message list | Real-DOM sliding window (no virtualisation), day dividers, grouped runs, faded pending bubbles, jump to message / to present, scroll position kept per channel. | `R/features/chat/RealMessageList.tsx`; spec `2026-08-25-real-dom-message-list-plan.md` |
| Formatting | `**bold**`, `*italic*`, `__underline__`, `~~strike~~`, inline code, fenced code blocks with highlighting (27 grammars), `$TeX$` math (KaTeX), autolinked URLs; a live preview while composing formatted text; a code / math composer panel. | `R/features/chat/richText.ts`, `CodeBlock.tsx`, `MathTex.tsx`, `RichComposer.tsx` |
| Emoji | Twemoji picker with search and an info popover; native emoji rendered as twemoji. | `R/features/chat/EmojiPicker.tsx`, `R/components/emoji/` |
| GIFs | GIFs tab in the picker searching KLIPY (or GIPHY) from the main process; a sent GIF is just its https URL. Content filter `low` unless "Unfiltered GIF search" is on. | `R/features/chat/GifPicker.tsx`, `M/gifs.ts` |
| Link previews | Unfurled by the client's main process (OpenGraph / Twitter / oEmbed, image size probe, private-network guard), behind the Privacy toggle "Show link previews". A message that is only a media link hides the URL. | `M/linkPreview.ts`, `R/features/chat/LinkEmbeds.tsx` |
| Attachments | Images, video, audio, documents; pick, drag-and-drop or paste; resumable chunked upload over HTTPS; thumbnails (320 / 640 / 1280) and a ThumbHash placeholder made by the uploader; per-server size cap (default 100 MiB); the server refuses uploads that would eat its free-space headroom. Inline media capped at 550 × 350 (GIFs 440 × 280). | `R/features/chat/uploadAttachment.ts`, `AttachmentList.tsx`, `C/attachment_http.cpp` |
| Media playback | Inline video and audio players that survive scrolling and channel switches (persistent layers); a download button beside media. | `R/features/chat/PersistentVideoLayer.tsx`, `PersistentAudioLayer.tsx` |
| Image viewer | Themed stage with fly-in / fly-out, real-pixel zoom with a minimap, gallery of the clicked message's images, Copy image, Save. | `R/features/chat/imageViewer/`, `ImageViewer.tsx` |
| Transfers panel | Title-bar downloads manager (pause / resume over Range, encrypted files included, show in folder) and uploads tab (pause, go to message, taskbar progress, quit guard). Speed caps in Settings → Network. | `R/features/transfers/`, `M/downloads.ts`, `M/downloadPacer.ts`; spec `2026-10-04-transfers-panel-design.md` |
| Edit / delete | Edit your own messages; delete your own, or anyone's with Manage Messages. "(edited)" marker. | spec `2026-05-15-message-deletion-design.md` |
| Replies | Reply to a message (spine line, embedded preview, click the name for the profile, the line to jump). | `R/features/chat/ReplyPreview.tsx` |
| Send pacing | Sends are paced to the server's 10-burst / 3-per-s bucket, so a burst is delayed, never dropped; a refused message is withdrawn and its text goes back into the box. | `R/features/chat/sendPacing.ts` |
| Slowmode | A pill + countdown in the composer for members it applies to; the send is held with the text kept. | `R/features/chat/slowmode.ts`, `SlowmodeIndicator.tsx` |
| Drafts | One draft per channel / DM, kept while you browse. | `R/stores/draftsStore.ts` |
| Message search | Header search box with chips (from, in, has: file / image / video / audio / document / link, before / after, sort newest / oldest / relevance, "inside words"), results panel in the member list's place (floats over chat below 1024 px), Jump. Runs on the community server (FTS5); covers every readable channel; encrypted channels get filters only. Nothing about a search is stored on the device. | `R/features/search/`, `C/search.cpp`; spec `2026-10-14-message-search-design.md` |
| Time format | 12- / 24-hour clock or follow the system (Settings → Appearance). | `R/lib/timeFormat.ts` |

## 7. Voice channels

| Feature | Behaviour | Where |
|---|---|---|
| Join / leave / switch | Click a voice channel; a switch shows you in the new one at once (optimistic roster, then the server's). Join / leave sounds. | `R/features/voice/`, `N/commands/voice.rs` |
| Sidebar rosters | Every voice channel shows who is in it (with LIVE and speaking state), per server, even when you are not in voice. Drag a member onto another channel to move them (MOVE_MEMBERS). | `R/features/channels/` |
| Voice view | A stage of 16:9 tiles (avatar-tinted, status chip, speaking outline); "+N more" past the size floor with hidden speakers swapped in; streams large above a people strip; floating control dock. | `R/features/voice/stage/`; spec `2026-10-04-voice-view-redesign.md` |
| Audio processing | Opus at the channel's bitrate (≤ 320 kbps), echo cancellation (AEC3), noise suppression (RNNoise), automatic gain control (AGC2, on by default), soft-clip mixer, adaptive jitter buffer with packet-loss concealment. | `N/media/pipeline.rs`, `jitter.rs` |
| Input modes | Voice activity (sensitivity gate) or push to talk (release delay); push to mute. | `N/media/voice_gate.rs`, `R/features/settings/tabs/AudioTab.tsx` |
| Devices | Input / output pickers, "Default" follows the OS default device, hot-plug handled, optional separate output device for stream audio, stereo stream audio. | `N/media/audio_device.rs`, `default_device_watch.rs` |
| Per-user controls | Local mute and volume per person (right-click). | `R/features/voice/` |
| Self / server state | Mute / deafen yourself; moderators server-mute / deafen, move or disconnect you (`VOICE_FORCE_NOTIFY`). Server-mute stops talking but not stream audio (by decision). | `C/main.cpp` |
| Encryption | Every voice channel session is an MLS group (OpenMLS); no keys → no voice; plaintext media is ignored. | `N/e2ee/group.rs`, `N/media/frame_crypto.rs`; spec `2026-09-03-mls-voice-channel-encryption-design.md` |
| Connection stats | Ping and packet-loss history for the session (popover from the user panel). | `R/features/voice/ConnectionStatsPopover.tsx`, `R/stores/voiceStatsStore.ts` |
| Global hotkeys | Toggle mute / deafen, push to talk, push to mute, leave voice / hang up, answer / decline a call. Bound keys only (GlobalShortcuts portal on Wayland, exact grabs on X11, `GetAsyncKeyState` on the bound keys on Windows, accelerators on macOS); `decibell --hotkey=<action>` for window managers. | `N/hotkeys/`, `M/hotkeys.ts`, `R/features/hotkeys/`; spec `2026-10-10-global-hotkeys-design.md` |

## 8. Screen sharing (streams)

| Feature | Behaviour | Where |
|---|---|---|
| Go Live | Source picker (OS dialog on Linux / macOS, our grid on Windows), presets **Smooth** (1080p60), **Sharp** (full res 30), **Slides** (full res 5 fps, encoded for detail), **Data saver** (720p30), and Advanced (resolution incl. "Source", frame rate 5 / 15 / 30 / 60 / 120, bitrate, codec). Options panel beside the dialog on wide windows. | `R/features/voice/CaptureSourcePicker.tsx`, `goLive/` |
| Capture + encode | **Windows** (default): WGC (Win11) or DXGI Desktop Duplication with GPU cursor compositing (Win10), D3D11 video processor, FFmpeg NVENC / AMF. **Linux** (when a hardware encoder probes): xdg-desktop-portal + PipeWire (SHM) into a static FFmpeg NVENC. **Fallback, and macOS**: Chromium `getDisplayMedia` + WebCodecs `VideoEncoder` in the renderer. Codecs: H.264, HEVC, AV1 (by probe and settings). | `N/media/capture_*.rs`, `encoder*.rs`, `R/features/voice/streaming/StreamCapture.ts` |
| Stream audio | Share audio by app: Selected apps / All except / All apps, changeable while live. Linux: PipeWire tap; Windows: per-process WASAPI loopback mixer; not on macOS. Never captures Decibell's own output. | `N/media/capture_audio_{pipewire,wasapi}.rs`; spec `2026-09-02-multi-app-audio-picker-design.md` |
| Watching | Watch, Switch, Watch too (several streams), Focus, theater, fullscreen; click the video to return to the grid; viewer count + stats; per-stream volume. | `R/features/voice/StreamVideoPlayer.tsx`, `stage/streamActions.ts` |
| Mini-player | Pop-out player that follows you around the app (resizable, flick to a corner, click to return). One persistent player is reparented between the full view, the grid tile and the mini-player, never re-created. | `R/features/voice/StreamPipManager.tsx`, `streamPipHost.ts`, `MiniStreamPlayer.tsx` |
| Thumbnails | 960 px JPEG previews at start, +2 s, then every 15 s; shown to people not watching (plaintext by decision). | `N/media/thumb_encode.rs` |
| Take stream with me | Optional (Settings → Codecs): switching voice channels on the same server keeps the share going. | `R/features/settings/tabs/CodecsTab.tsx` |
| Encryption | Video sealed per frame and stream audio per packet under the channel's MLS keys; FEC / NACK unchanged. | `N/media/frame_crypto.rs` |

## 9. App, settings and shell

| Feature | Behaviour | Where |
|---|---|---|
| Settings → Account | Picture, username change, password change, log out. | `R/features/settings/tabs/AccountTab.tsx` |
| Settings → Devices | See §1. | `DevicesTab.tsx` |
| Settings → Appearance | Theme (Nocturne default, Matinee, Graphite, Graphite Light, Console, Console Light, Console Split), text size, list density, time format. | `AppearanceTab.tsx`, `R/styles/globals.css` |
| Settings → Privacy | Show me as Away (auto-away threshold), friends-only DMs, link previews, unfiltered GIF search, anonymous crash reports (Sentry, opt-in banner on first run), encryption setup / passphrase. | `PrivacyTab.tsx` |
| Settings → Audio | Devices, input mode, processing toggles, mic test, separate stream output. | `AudioTab.tsx` |
| Settings → Keybinds | Global hotkey recorder with per-platform notes. | `KeybindsTab.tsx` |
| Settings → Network | Attachment upload / download speed limits. | `NetworkTab.tsx` |
| Settings → Codecs | AV1 / HEVC use, refresh codec capabilities, take stream with me. | `CodecsTab.tsx` |
| Settings → About | Version and update status / check. | `AboutTab.tsx` |
| Settings windows | One shared frame; below 1024 px the sidebar folds to an icon rail; app minimum 800 × 600. | `R/components/SettingsWindow.tsx` |
| Auto-update | electron-updater against GitHub releases. | `M/update.ts` |
| Deep links | `decibell://invite/<CODE>` opens the join dialog. | `M/index.ts`, `R/features/servers/DeepLinkJoinModal.tsx` |
| Taskbar flash | On an incoming call when the window isn't focused (not in DND). | `M/window.ts` |
| Crash reports | Sentry, opt-in, DSN baked into release builds. | `M/sentry.ts`, `R/lib/sentry.ts` |

## 10. Not built (yet)

Common chat-app features Decibell does **not** have today, so a new client shouldn't look for them on
the wire: message reactions, typing indicators, @mentions and mention notifications, pinned messages,
threads, per-channel unread markers / read state (DMs have unread counts; channels don't), desktop
notifications for messages, webcam video, server emoji / stickers, voice messages, message forwarding.
Codec renegotiation mid-stream exists in the proto (`UPDATE_CAPABILITIES_REQ`,
`STREAM_CODEC_CHANGED_NOTIFY`, `STREAM_WATCHER_NOTIFY`) but the client never sends or acts on it.
DM attachments are planned as end-to-end encrypted blobs in object storage (central stores no file bytes).
