# Decibell docs index

Start with the living documents; the dated ones below record how each feature was designed and are
not updated after it ships (later changes are in the running feature log).

| Living document | What it is |
|---|---|
| `../CLAUDE.md` | Working rules, verification bar, the owner's standing decisions |
| `../ARCHITECTURE.md` | System reference: servers, tokens, wire protocol per feature, encryption, UDP media plane, client layers, reusing the native crate |
| `FEATURES.md` | Every user-visible feature, where it lives, which design record covers it, and what isn't built |
| `../electron-client/HANDOFF.md` | The Electron client: release history, gotchas, building on Linux / Windows, open items |
| `../electron-client/PACKAGING.md` | Packaging, CI, AUR |
| `reviews/2026-08-23-community-server-review.md` | The **running feature log**: one entry per shipped feature or fix since 2026-08-23, with rationale |

## Design records (`superpowers/specs/`)

Status: **live** = describes shipped behaviour (details may have moved on; the feature log has the
deltas); **partly** = some of it shipped; **history** = superseded or never built.

| Record | Status |
|---|---|
| `2026-03-13 … 2026-03-23-tauri-rebuild-phase{1..5}-design.md` | history: the Tauri rebuild; its Rust networking / voice / video work carried into `electron-client/native/` |
| `2026-03-28-windows-screen-capture-design.md` | partly: Tauri-era Windows capture; WGC + DXGI live on in `native/media/capture_{wgc,dxgi}.rs` |
| `2026-03-29-stream-audio-sharing-design.md` | live (updated 2026-09-02 for the multi-app picker) |
| `2026-04-10-voice-pipeline-quality-fix-design.md`, `2026-04-11-voice-scaling-fix-design.md` | live in spirit: the voice pipeline has been reworked since (HANDOFF §5.14, the feature log) |
| `2026-04-27-video-codec-negotiation-design.md` | partly: capabilities ride `JOIN_VOICE_REQ` and the streamer picks a codec; mid-stream renegotiation (Plan C) was never wired |
| `2026-04-28-windows-d3d11-zero-copy-design.md` | live (Windows D3D11 pipeline) |
| `2026-05-12-custom-profile-pictures-design.md` | live (editor replaced 2026-10-28: picture editor) |
| `2026-05-12-windows-native-ffmpeg-encoder-design.md` | live (QSV later dropped from the probe) |
| `2026-05-14-auto-rejoin-communities-design.md` | live |
| `2026-05-14-live-stream-indicators-design.md` | live |
| `2026-05-14-persistent-dms-design.md` | live (conversation list later moved to a trigger-kept table, review B11) |
| `2026-05-15-custom-server-pictures-design.md` | live |
| `2026-05-15-linux-native-ffmpeg-encoder-design.md` | live: the PipeWire SHM → NVENC path; the DMA-BUF zero-copy part was closed (dead code remains) |
| `2026-05-15-message-deletion-design.md` | live |
| `2026-05-16-electron-auto-update-design.md` | live |
| `2026-05-17-sentry-error-monitoring-design.md` | live |
| `2026-08-17-roles-permissions-design.md` | live, extended by v2 and v3 |
| `2026-08-22-permissions-v2-design.md` | live, extended by v3 |
| `2026-08-22-server-management-moderation-design.md` | live |
| `2026-08-22-theme-a-asymmetric-jwt-cert-pinning-design.md` | live (Ed25519 tokens, community secret, cert pinning) |
| `2026-08-25-real-dom-message-list-plan.md` | live: the message list's invariants + the Virtuoso postmortem |
| `2026-08-28-p2p-dm-calls-design.md` | live (two-machine live test pending) |
| `2026-09-02-multi-app-audio-picker-design.md` | live |
| `2026-09-03-authenticated-p2p-calls-design.md` | live |
| `2026-09-03-e2ee-dms-design.md` | live |
| `2026-09-03-mls-voice-channel-encryption-design.md` | live |
| `2026-09-04-encrypted-text-channels-design.md` | live (since 2026-10-14 chosen at creation, no toggle) |
| `2026-10-04-transfers-panel-design.md` | live |
| `2026-10-04-voice-view-redesign.md` | live |
| `2026-10-09-permissions-v3-design.md` | live |
| `2026-10-10-global-hotkeys-design.md` | live |
| `2026-10-11-user-status-design.md` | live |
| `2026-10-12-status-messages-design.md` | live |
| `2026-10-14-message-search-design.md` | live |
| `2026-10-14-uid-identity-audit.md` | history: the audit that led to the uid design |
| `2026-10-14-welcome-banner-actions-design.md` | live (the banner's Edit topic button was later dropped; topics are edited in channel settings) |
| `2026-10-15-central-hardening-design.md` | live |
| `2026-10-15-central-scaling-design.md` | live |
| `2026-10-15-uid-identity-design.md` | live (all five stages) |
| `2026-10-18-multi-device-design.md` | live |
| `2026-10-18-roster-paging-design.md` | live |
| `2026-10-19-devices-and-password-design.md` | live |
| `2026-10-28-picture-editor-design.md` | live |
| `2026-11-07-qt-native-client-design.md` | approved, not built: the native Qt 6 Quick + Rust client (core split, zero-copy video, theming, spike plan, Electron baseline) |

`superpowers/plans/` holds the step-by-step implementation plans for the 2026-03 … 2026-05 specs (same
names). They're history: useful for why code is shaped the way it is, never as a to-do list. The
`2026-04-02-dmabuf-zero-copy-capture.md` plan describes the Linux path that was closed.

`design/` holds two HTML design proposals committed 2026-08-17 (a design overhaul and a design-system v2); history.

## Reviews (`reviews/`)

| Review | Status |
|---|---|
| `2026-06-11-client-code-review.md` | history |
| `2026-07-27-frontend-review.md` | open thread: the attachment scroll glitch (its header is a hand-off for it) |
| `2026-08-15-streaming-review.md` | history |
| `2026-08-21-community-server-review.md` | history: the findings behind fix batches 13–19 and permissions v2 / v3; superseded by the running log |
| `2026-08-23-community-server-review.md` | **the running feature log** (see above) |
| `2026-10-17-pre-1.0-review.md` | §1 all fixed (2026-10-18); §2–§6 not started |

Elsewhere: `../CODE_REVIEW.md` (2026-07 whole-codebase review, history), `../design_handoff_theme_switcher/`
(the five-palette theme hand-off, implemented in 0.6.10, since restyled), `../tauri-rebuild-prompt.md`
and `../tauri-client/docs/` (Tauri era, history).
