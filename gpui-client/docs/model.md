# The state model (`crates/decibell-model`)

The layer between the shared core (`crates/decibell-core`: connections, protocol, media, crypto) and the
views. In the Electron client this is the renderer's zustand stores plus the `use*Events` hooks that feed
them; here it is GPUI entities holding plain Rust state, fed by typed core events, with actions that call
the core's API. Read with `CONVENTIONS.md` and `docs/gpui-notes.md` §3–§4.

Everything below was run on 2026-11-26 against the local stack (`tools/dev/`); §13 has the numbers.

## 1. Map

```
crates/decibell-model/
  src/lib.rs            Model: every store, as a GPUI global; reads that span stores
  src/core.rs           the tokio runtime, starting / stopping the core, `prepare` (§2)
  src/local_key.rs      the E2EE at-rest key: OS keychain, or injected
  src/pump.rs           EventQueue, ModelSink (the core's EventSink), the pump task, StreamFrameSink
  src/events/           the router (one exhaustive match) and a module per event area
  src/stores/           the stores (§5)
  src/actions/          what the UI asks for: store updates + the core call
  src/call.rs           `call`, `call_or_toast`, `show_error`, `offer_retrust`
  src/error.rs          UiError: the one place a CoreError becomes what is shown
  src/save_echo.rs      EchoSave: a settings save that settles on the server's echo
  src/invite_link.rs    the `decibell://invite/…` grammar
  src/ids.rs            Uid, ServerId, ChannelId
  src/bin/headless.rs   decibell-headless (§8)
  tests/                invalidation.rs, pump_real_loop.rs, stack.rs (§9)
```

Depends on `gpui`, `decibell-theme` (for `ThemeId`; pure data) and `decibell-core`. Never on `decibell-ui`
or any view crate: the model does not know views exist. `decibell-app` depends on it; feature crates take
`Model` from the global.

Identity (`ids.rs`): users are `Uid = i64` everywhere (0 = nobody, negative = a community's placeholder,
unique only inside that community). A community is `ServerId`, an integer: central's id as it is, or a
provisional negative id for the `"host:port"` key a connection opened from an invite carries until the
community re-keys it (`ServerId::from_wire` / `from_central` / `wire()`). Channels are `ChannelId =
SharedString` (slugs, unique only within their community): always key on `(ServerId, ChannelId)`.

## 2. Starting and stopping the core

```rust
// main(): the platform first, `prepare` between it and the Application (Linux needs it: below).
let platform = gpui_platform::current_platform(false);
decibell_model::core::prepare(&platform.background_executor());
gpui::Application::with_platform(platform).with_assets(Assets).run(|cx| {
    // Stores + pump, synchronously: build the first window right after.
    let model = Model::init(cx, core::default_user_data_dir().ok());
    // The window can open with model.layout.read(cx).prefs().theme / text_size_px / row_scale (§7).
    Model::start_core(cx, CoreOptions {
        user_data_dir: None,                        // $DECIBELL_USER_DATA_DIR, else <config dir>/decibell
        app_version: env!("CARGO_PKG_VERSION").into(),
        local_key: Box::new(KeychainKey),
    }).detach();
});
// After run() returned (the event loop is gone, blocking is fine):
decibell_model::core::shutdown(Duration::from_secs(3));
```

What `Model::start_core` does, in order (ARCHITECTURE.md §7.3 "Starting the core"; the renderer's
`loadSettings.ts`):

1. The user data directory: `$DECIBELL_USER_DATA_DIR`, else `decibell` under the platform's config
   directory (`$XDG_CONFIG_HOME` or `~/.config`; `%APPDATA%`; `~/Library/Application Support`). Never
   Electron's (`decibell-electron-client`). It holds the core's `config.json`, `cert_pins.json`, the E2EE
   key store, and this client's `ui.json`.
2. The runtime: tokio multi-thread, **2 workers**, `enable_all`, threads named `decibell-rt` (built by
   `prepare`, else here).
3. The at-rest key (`LocalKeyProvider::load`, a task: the UI thread does not wait on the keychain).
4. `lifecycle::init(InitOptions, sink, handle)`. Once per process.
5. `settings::load_config` → `Settings::apply_loaded` (emits `SettingsEvent::Appearance`).
6. If a sign-in is saved: `AuthStore::set_logging_in(true)`, `auth::auto_login()`.
7. `AuthStore::booted = true`. Until then the root view shows neither the sign-in page nor the app.

`login_succeeded` then asks for the server list; memberships, community answers, rosters follow as events.

**The at-rest key** (`local_key.rs`). Electron keeps 32 random bytes in `<userData>/e2ee/local.key` wrapped
by `safeStorage`; here they live in the OS keychain through GPUI's credentials API (item URL
`decibell://e2ee-local-key/<user data dir>`, so one key per profile). Same rules: no keychain → no key (the
core seals with its machine-derived key); a stored value that is not 32 bytes of base64 is ignored, not
replaced; nothing stored → generate, store, use; `local_key_protected` is true exactly when the key came
from the keychain. **The saved sign-in depends on it**: sealed under a subkey of this key when protected,
else under the derived key; a profile whose keychain item is gone shows the sign-in page ("saved sign-in
unavailable: sealed under the keychain key"). Tests and the headless tool inject `FixedKey(LocalKey::none())`
and never touch the keychain. On Linux the item is labelled `zed-github-account` (GPUI hard-codes the
label; gpui-notes §2.18).

**`core::prepare` and the D-Bus clash (Linux).** The core depends on `zbus` with its `tokio` feature; GPUI's
Linux platform uses the same `zbus` through `ashpd` and `oo7` with `async-io`. Cargo builds one `zbus` with
both features and `zbus` then uses tokio everywhere, so every D-Bus call GPUI makes (portal settings, the
file chooser, `open_url`, the keychain, power and sleep listeners) panics with "there is no reactor running"
on a thread outside a tokio runtime — and the release profile aborts on a panic, at startup. `prepare`
builds the runtime before the `Application` exists and enters its context for good on GPUI's `Worker-N`
threads and on smol's executor thread, which is where GPUI runs that code. The main thread is deliberately
left outside (the core runs an `api` call inline on a thread that is inside a runtime; the UI thread must
never do the core's work). Without `prepare`, `KeychainKey` answers "no key" instead of taking the app
down. The real fix is in the core (§12).

**Shutdown.** `Model::init` registers an `on_app_quit` that flushes what is inside a debounce (`ui.json`, a
pending settings save, which goes straight to the runtime). `core::shutdown(limit)` then runs
`lifecycle::shutdown()` for at most half the limit and gives the runtime's threads the rest. Idempotent.

## 3. Events in: one wake per burst

`pump.rs`. The core calls `EventSink::event` from its own threads. `ModelSink` pushes onto
`EventQueue` (a `Mutex<Vec<Incoming>>`) and wakes the UI thread **only when the queue went from empty to
non-empty**. The pump is one foreground task: per wake it swaps the whole queue out (the drained `Vec`
becomes the queue's next buffer, so steady state allocates nothing) and applies the batch inside a single
`App::update`. GPUI flushes effects and schedules frames once per `update`, so a burst is one wake of the
UI thread and at most one frame. The only lock shared between the core's threads and the UI thread is that
mutex, held for a `push` or a `mem::swap`.

- `stream_frame` / `stream_frame_sink_removed` never touch the UI thread: they go to the installed
  `StreamFrameSink` (`core::set_stream_frame_sink`; default: dropped), on the thread that produced the
  frame. The video crate provides it.
- `stream_thumbnail` is an ordinary queued item (`Incoming::Thumbnail`), routed to `todo::m3`.
- The queue is unbounded. Every event is small and rare except `voice_input_level` (~16/s, only while the
  Audio tab's meter asked for it); a stalled UI thread grows it by what arrives meanwhile.
- `Model::pump_stats()`: `events`, `wakes`, `drains`, `largest_batch` (the headless `dump pump`).

Accounting, measured:

| Scenario | events | wakes | drains |
|---|---|---|---|
| 10,000 events queued before the UI thread runs (`pump::tests`, GPUI's test scheduler) | 10,000 | 1 | 1 |
| 5 bursts of 2,000 from 8 threads while the UI thread is busy, real event loop (`tests/pump_real_loop.rs`) | 10,000 | 5 | 5 |
| 4 producers racing a live consumer (`pump::tests`): every wake found work, wakes ≤ events | 10,000 | = turns | = turns |
| Real sign-in as perf2 to "Daily Gaming connected, roster in" (headless, `dump pump`) | 9 | 7 | 7 |

## 4. Which views re-render: the rule, and what it means for stores

Verified at the pin by `tests/invalidation.rs` (and read in `gpui/src/app.rs` `App::notify`,
`gpui/src/window.rs` `record_entities_accessed` / `mark_view_dirty` / `invalidate_view`,
`gpui/src/view.rs` `prepaint_view`, `gpui/src/key_dispatch.rs` `view_path_reversed`):

1. **A window is redrawn when an entity it read during its last frame notifies.** Reading (`entity.read(cx)`
   or `update`) inside `render` / layout / paint records the entity for that window.
2. **A redraw calls `render` on the root view and on every view that is not cached.**
3. **A cached view** (`entity.clone().cached(style)`, one per pane: CONVENTIONS "Frames and wakeups")
   **re-renders only when that view itself was notified** (or a view inside it). A store's id in the
   window's dirty set matches no view, so **reading a store in `render` is not enough for a cached view: it
   goes stale.** It must observe what it reads:
   `cx.observe(&store, |_, _, cx| cx.notify())`, or `cx.subscribe` and notify only for its keys.
4. **A notify on an entity no window read costs no frame**; changing state without `notify` draws nothing.
5. Several notifies inside one `update` are one frame.

Consequences, which are the rules for stores here:

- **A store method calls `cx.notify()` exactly when state changed, never otherwise** (every setter compares
  first). A notify is a frame for every window that read the store, even if no view is dirty.
- **Entities are split by how often and for whom they change.** A roster delta notifies that community's
  `Roster` and nothing else; a connection flap notifies `Connections`, not a channel list (asserted by
  `events::tests::connection_state_is_apart_from_content`); a pointer move during a resize notifies
  `Layout` only.
- **Where the renderer selected a slice of a big store, the store here emits a keyed event** next to the
  notify, and a small view subscribes and filters: `UsersStore` → `NamesChanged(Vec<Uid>)`, `Layout` →
  `LayoutEvent`, `InviteResolve` → `InviteChanged(code)`, `Navigation` → `NavEvent::ChannelActivated`,
  `Settings` → `SettingsEvent::Appearance`. A view that shows many keys (a member list) just observes.
- **Derived data is computed in the store, once per change, not in `render`**: `Roster::visible()` (the
  sidebar's sorted rows, rebuilt lazily after a change: a batch of deltas is one sort), `Member::key` (the
  roster key, once per version of a member), `SharedString` for every text a view draws.
- **Views never mutate a store from `render`**: `Roster::want(uid)` and the like are called where rows are
  built or from an observer.

## 5. The stores

All are fields of `Model` (`Model::global(cx)`), GPUI entities, created in `Model::build`.

| Field / entity | Owns | Renderer origin | Notifies on |
|---|---|---|---|
| `auth: AuthStore` | username, uid, renames, devices, sign-in page state, login notice, `saved_username`, `booted` | `authStore.ts` | sign-in / out, rename |
| `users: UsersStore` | uid → current name; which uids central named | `usersStore.ts` | a name changed (`NamesChanged`) |
| `nav: Navigation` | active view / community / channel, Settings tab | `uiStore.activeView`, `chatStore.activeServerId` / `activeChannelId` | navigation |
| `overlays: Overlays` | the modal (renderer ids: `overlays::modal`), profile popup, user context menu, auth error, cert mismatch, membership revocation | `uiStore` | open / close |
| `settings: Settings` | the core's `AppSettings` (config.json), debounced save | `uiStore` settings fields, `loadSettings.ts`, `saveSettings.ts` | a setting changed |
| `layout: Layout` | `ui.json`: widths, corners, panel visibility, picker memory, `extra` (§7) | the `localStorage` keys | that part changed (`LayoutEvent`) |
| `servers: ServersStore` | memberships (tiles), central's directory, picture versions + data, pending invite | `chatStore.servers`, `discoverableServers`, `serverPicture*`, `pendingInvite` | list / picture changes |
| `connections: Connections` | central's link (the banner), connected and connecting communities | `uiStore.connectionStatus`, `chatStore.connectedServers`, `pendingMembershipServerIds` | a link changed |
| `communities: Communities` | the registry: `ServerId` → `Community` (five entities, below) | — | a community appeared / sign-out |
| `Community::meta: CommunityMeta` | name, description, owner uid, listing, attachment port / limit | `serverMeta`, `serverOwner`, `serverPublicListing`, `serverAttachmentConfig` | settings edits |
| `Community::channels: Channels` | the ordered channel list (categories inline: Discord's flat model), per-channel overwrites | `channelsByServer`, `overwritesByChannel` | channel edits |
| `Community::roles: Roles` | the role list, most senior first | `rolesByServer` | role edits |
| `Community::roster: Roster` | held members, paging state, the sidebar's rows, lookups | `membersByServer`, `memberRosterMeta`, `features/members/` | every roster packet |
| `Community::moderation: Moderation` | bans, invites, their "truncated" flags | `bansByServer`, `invitesByServer` | moderation |
| `toasts: Toasts` | toasts and their lifetime timers | `toastStore.ts` | push / dismiss |
| `invites: InviteResolve` | memo of invite-code lookups (30 s retry of connectivity failures, 300 entries) | `inviteResolveStore.ts` | an entry changed (`InviteChanged`) |
| `update: UpdateStore` | the updater's status (shape only) | `updateStore.ts` | — |
| `answers: Answers` | nothing: emits the answers no store keeps (§6) | views' own `listen()` | — |

Reads that span stores are on `Model`: `name_of(uid, fallback)`, `display_name(server, uid, name)`
(nickname in that community, else the current name: `useDisplayName`), `is_owner`, `has_permission`
(`usePermission`), `channel_permission` (`useChannelPermission`), `community(id)`.

**Names** (CLAUDE.md "Backend identity is the uid"): every name drawn comes from `UsersStore` through
those two functions. Central's names win; a community's only fill gaps; names kept as history (ban rows,
audit entries, invite labels) never feed it. The fallback is the payload's name, else the view draws
"Unknown user". Roster order uses the community's own name (`Member::key`), never the users store's.

**Permissions** (`stores/permissions.rs`, a port of `features/servers/permissions.ts`): `perm::*` bits,
`EDITABLE_PERMISSIONS` (labels, descriptions, groups), `CHANNEL_OVERWRITE_BITS`,
`compute_effective_permissions`, `member_level`, `highest_role`, `Hierarchy` (`can_manage`: lower-ranked
and, with "members can manage each other", peers; `can_moderate`: strictly lower only; never the owner).
A channel's permissions are **the server's answer** (`Channel::my_permissions`), not computed here. The
owner's arrive as all 64 bits set; `perm::ALL` is the renderer's 53.

**The roster** (`Roster`; spec `2026-10-18-roster-paging-design.md`): `apply_page` / `upsert` / `remove`
return false on a revision gap and the handler calls `refetch(false)`; a first page replaces the roster
under a new `generation`; `visible()` gives the sidebar's rows (members past the loaded edge are held, not
drawn); `load_more()` pages (the sidebar calls it when `member_window::near_loaded_end`); `want(uid)` looks
a missing member up (50 ms batches of ≤ 200, once per generation, 3 tries 1.5 s apart). The window
arithmetic of the sidebar is `stores/member_window.rs`, roster order `stores/roster_order.rs`.

## 6. The 85 events

`events::route` matches every `CoreEvent` variant with no wildcard, and so does `events::users::directory`
(the users directory sees each event first). "answers" = also emitted on `Model::answers` for a view that
listens while it is open (`cx.subscribe(&model.answers, |this, _, event: &CoreEvent, cx| …)`).
"dir: central / community" = feeds the users directory with that precedence.

| # | Event | Handled by |
|---|---|---|
| 1 | `connection_lost` | `events/connection.rs` (central banner) + `events/servers.rs` (community: disconnected; hooks `m2::community_connection_lost`, `m3::clear_server_presence`) |
| 2 | `connection_restored` | the same two |
| 3 | `login_succeeded` | `events/auth.rs`; dir: central; hooks `m2::signed_in`, `m3::signed_in`, `m3::seed_own_avatar` |
| 4 | `user_renamed` | `events/auth.rs` (ours); dir: central |
| 5 | `avatar_changed` | follow-up: M3 (avatars) |
| 6 | `login_failed` | `events/auth.rs` |
| 7 | `register_responded` | `events/auth.rs` |
| 8 | `logged_out` | `events/auth.rs`; hooks `m2::logged_out`, `m3::logged_out` |
| 9 | `devices_changed` | follow-up: M3 (devices; the "New sign-in" toast) |
| 10 | `session_token_updated` | follow-up: M2 (attachment tokens) |
| 11 | `server_list_received` | `events/servers.rs` |
| 12 | `memberships_received` | `events/servers.rs` |
| 13 | `membership_added` | `events/servers.rs` |
| 14 | `community_auth_responded` | `events/servers.rs`; dir: community (owner); hooks `m2::reset_send_pacing`, `m2::community_authenticated`, `m2::attachment_target_changed`, `m3::clear_server_presence` |
| 15 | `message_received` | follow-up: M2; dir: central (DM) / community (channel) |
| 16 | `channel_history_received` | follow-up: M2; dir: community |
| 17 | `channel_pruned` | follow-up: M2 |
| 18 | `channel_updated` | `events/servers.rs`; answers; hook `m3::channel_updated` (live bitrate) |
| 19 | `channel_wipe_responded` | answers (channel settings) |
| 20 | `channel_wiped` | follow-up: M2 |
| 21 | `user_list_updated` | follow-up: M3 (presence); dir: central |
| 22 | `presence_delta` | follow-up: M3; dir: central |
| 23 | `own_presence` | follow-up: M3 |
| 24 | `friend_list_received` | follow-up: M3; dir: central |
| 25 | `friend_action_responded` | follow-up: M3 |
| 26 | `member_list_received` | `events/servers.rs`; dir: community |
| 27 | `mod_action_responded` | `events/servers.rs`; answers; hook `m2::legacy_rejection_suppressed` |
| 28 | `channel_message_rejected` | follow-up: M2 (send pacing, slowmode) |
| 29 | `membership_revoked` | `events/servers.rs`; hook `m3::clear_server_presence` |
| 30 | `role_list_received` | `events/servers.rs` |
| 31 | `member_upsert` | `events/servers.rs`; dir: community |
| 32 | `member_remove` | `events/servers.rs` |
| 33 | `ban_list_received` | `events/servers.rs` |
| 34 | `server_meta_updated` | `events/servers.rs`; dir: community (owner) |
| 35 | `server_update_responded` | answers (Server Settings → Overview) |
| 36 | `audit_log_received` | answers (Audit Log tab) |
| 37 | `member_search_received` | answers (`useMemberSearch`); dir: community |
| 38 | `message_search_received` | follow-up: M2 (search); dir: community |
| 39 | `storage_info_received` | answers (Storage tab) |
| 40 | `voice_force_notify` | follow-up: M3 |
| 41 | `channel_overwrites_received` | `events/servers.rs` |
| 42 | `role_action_responded` | `events/servers.rs`; answers |
| 43 | `channel_list_updated` | `events/servers.rs`; hooks `m2::purge_channel_state`, `m3::clear_server_presence` |
| 44 | `channel_action_responded` | `events/servers.rs`; answers |
| 45 | `invite_list_received` | `events/servers.rs` |
| 46 | `invite_create_responded` | `events/servers.rs`; answers (a personal invite's caller) |
| 47 | `invite_revoke_responded` | `events/servers.rs` |
| 48 | `voice_presence_updated` | follow-up: M3; dir: community |
| 49 | `voice_state_changed` | follow-up: M3 |
| 50 | `hotkey_action` | follow-up: M3 |
| 51 | `hotkeys_status` | follow-up: M3 |
| 52 | `voice_user_speaking` | follow-up: M3 |
| 53 | `voice_user_state_changed` | follow-up: M3 |
| 54 | `voice_input_level` | follow-up: M3 |
| 55 | `voice_ping_updated` | follow-up: M3 |
| 56 | `voice_connection_stats` | follow-up: M3 |
| 57 | `voice_error` | follow-up: M3 |
| 58 | `stream_presence_updated` | follow-up: M3; dir: community |
| 59 | `stream_codec_changed` | follow-up: M3 |
| 60 | `stream_gpu_fallback` | follow-up: M3 (no emitter in the core today) |
| 61 | `stream_capture_ended` | follow-up: M3 |
| 62 | `keyframe_requested` | follow-up: M3 |
| 63 | `native_stream_failed` | follow-up: M3 |
| 64 | `caps_refreshed` | follow-up: M3 (no emitter in the core today) |
| 65 | `dm_conversations_received` | follow-up: M2; dir: central |
| 66 | `dm_history_received` | follow-up: M2; dir: central |
| 67 | `dm_read_synced` | follow-up: M2 |
| 68 | `dm_message_delete_responded` | follow-up: M2 |
| 69 | `dm_message_deleted` | follow-up: M2 |
| 70 | `channel_message_delete_responded` | follow-up: M2 |
| 71 | `channel_message_deleted` | follow-up: M2 |
| 72 | `dm_message_edit_responded` | follow-up: M2 |
| 73 | `dm_message_edited` | follow-up: M2 |
| 74 | `e2ee_status_changed` | follow-up: M3 |
| 75 | `e2ee_peer_changed` | follow-up: M3 |
| 76 | `voice_e2ee_state` | follow-up: M3 |
| 77 | `channel_message_edit_responded` | follow-up: M2 |
| 78 | `channel_message_edited` | follow-up: M2 |
| 79 | `server_picture_update_responded` | `events/servers.rs`; answers (the picture editor) |
| 80 | `server_picture_received` | `events/servers.rs` |
| 81 | `server_picture_changed` | `events/servers.rs` |
| 82 | `call_signal` | follow-up: M3; dir: central |
| 83 | `call_connected` | follow-up: M3 |
| 84 | `call_failed` | follow-up: M3 |
| 85 | `call_dropped` | follow-up: M3 |
| — | `stream_thumbnail` (the sink's own method) | follow-up: M3 (`todo::m3::stream_thumbnail`) |
| — | `stream_frame`, `stream_frame_sink_removed` | `StreamFrameSink`, off the UI thread (the video crate) |

Not core events in Electron either: `deep_link_received` (main process) → `actions::servers::open_invite_link(url)`
once the single-instance socket exists; `update_status` → `UpdateStore::set`.

## 7. Client-only preferences: every `localStorage` key and its home

The renderer has 18 keys. Settings chosen in Settings live in the core's `config.json` (`Settings`, saved
whole through `api::settings::save_settings`, 250 ms after the last change). Per-install view state lives
in **`<user data dir>/ui.json`** (`Layout` / `UiPrefs`): read synchronously when the model is built,
written atomically (temporary file + rename) off the UI thread 200 ms after the last change; no timer while
nothing is pending; flushed on quit.

| `localStorage` key | Home |
|---|---|
| `decibell.theme` | `config.json` `theme` (`Settings::theme()`), mirrored in `ui.json` `theme` so the window opens with it before the core has read the config |
| `decibell.textSizePx` | `config.json` `text_size_px`; mirror `ui.json` `text_size_px` |
| `decibell.rowScale` | `config.json` `list_density`; mirror `ui.json` `row_scale` |
| `decibell.layout.sidebarWidth` | `ui.json` `sidebar_width` (180–480, default 240) |
| `decibell.layout.pipWidth` | `ui.json` `pip_width` (240–640, default 320) |
| `decibell.layout.pipCorner` | `ui.json` `pip_corner` (default `bottom-right`) |
| `decibell.layout.callStageVoiceHeight` | `ui.json` `call_stage_voice_height` (200–1200, default 300) |
| `decibell.layout.callStageStreamHeight` | `ui.json` `call_stage_stream_height` (200–1200, default 470) |
| `decibell.layout.membersPanel` | `ui.json` `members_panel` (default on) |
| `decibell.layout.dmFriendsPanel` | `ui.json` `dm_friends_panel` (default on) |
| `decibell.emoji.recent` | `ui.json` `emoji_recent` (`Layout::set_emoji_recent`; M2's picker) |
| `decibell.picker.tab` | `ui.json` `picker_tab` (`"emoji"` / `"gifs"`) |
| `decibell.decoder_caps.v6` | `ui.json` `extra["decoder_caps"]` if the video crate wants a cache (`Layout::extra` / `set_extra`; M3) |
| `decibell.native_encoder_caps.v2` | `ui.json` `extra["native_encoder_caps"]` (M3) |
| `decibell.encoder_caps.v6` | none: the WebCodecs encoder probe's cache; there is no WebCodecs here |
| `decibell.linux_native_encode` | none: an opt-out to WebCodecs encode; native encode is the only path here |
| `decibell.win_native_encode` | none, same |
| `decibell.real_message_list_debug` | none persisted: a developer trace flag; use an environment variable (M2) |

**Theme and scale** go through the `Theme` global in `decibell-ui`, which the model does not depend on: the
app subscribes once and applies.

```rust
cx.subscribe(&model.settings, |settings, _: &SettingsEvent, cx| {
    let s = settings.read(cx);
    let (id, text, row) = (s.theme(), s.text_size_px() as f32, s.row_scale() as f32);
    if cx.theme().id != id { Theme::set(cx, id); }
    Theme::set_scale(cx, text, row);
}).detach();
```

Views change them with `Settings::set_theme` / `set_text_size_px` / `set_row_scale` / `set_time_format`,
every other setting with `settings.update(cx, |s, cx| s.change(|v| v.aec_enabled = on, cx))`.

## 8. `decibell-headless`

The core and the stores with no window, driven by a script on the command line; one process per client.
`cargo run -p decibell-model --bin decibell-headless -- --help` lists the steps (`login <account>`,
`logout`, `wait booted | logged-in | logged-out | connected <community> | roster <community> | online /
offline <community> <user>`, `open-server`, `open-channel`, `dump <store>`, `idle <seconds>`, `print`,
`wait-file`). Each `dump` prints one line `DUMP <what> <json>`; the last line is `DONE` or `FAILED …`.

It only talks to the local stack: it exits 2 unless `DECIBELL_CENTRAL` is a loopback address **and** the
build honours that variable (a debug build, or `--features local-central`), and `login` takes only accounts
of the stack's `accounts.json`. Default: no keychain (`--keychain` opts in), profile from
`--user-data-dir`.

```sh
eval "$(../tools/dev/local-stack.sh client-env)"
target/debug/decibell-headless --user-data-dir /tmp/p login perf2 wait connected "Daily Gaming" \
    wait roster "Daily Gaming" dump roster "Daily Gaming" dump pump logout wait logged-out
# Measuring core + model alone (release, never shipped: it has `local-central`):
cargo build --release -p decibell-model --features local-central --bin decibell-headless
target/release/decibell-headless --user-data-dir /tmp/p wait connected "Daily Gaming" idle 100 &
../tools/perf/measure.sh --pid $! --settle 20 --duration 60 --label "core + model idle"
```

## 9. Tests

```sh
cargo test -p decibell-model                         # unit + router + invalidation + pump; stack tests skip
cargo clippy -p decibell-model --all-targets
eval "$(../tools/dev/local-stack.sh client-env)"     # the stack must be up
DECIBELL_STACK=1 cargo test -p decibell-model --test stack -- --nocapture
```

- **Unit tests** live beside the code. Store logic that needs timers or notifications uses
  `#[gpui::test]` with `TestAppContext` (deterministic clock: `cx.executor().advance_clock(d)`); how
  requests leave a store is a closure, so tests record instead of calling the core (`Roster::set_requester`,
  `Settings::new(saver)`, `InviteResolve::new(resolver)`).
- **Router tests** (`src/events/tests.rs`): `Model::init(cx, None)` without the core, events built by hand
  and delivered through the real queue and pump (`deliver`). Every core call a handler makes answers "not
  running" (deferred, so a handler may update the entity that called).
- **`tests/invalidation.rs`**: the GPUI rule of §4. **`tests/pump_real_loop.rs`**: §3 on the real loop.
- **Stack tests** (`tests/stack.rs`) drive `decibell-headless`; each client has its own temporary user data
  directory. They cover: password sign-in as `perf2`; memberships → Daily Gaming connects; its channel tree,
  roles and first roster window against what `tools/dev` seeded (298 members, alice owns it, role counts,
  `slow-lane` 30 s, `vault` encrypted, Staff hidden); a member's effective permissions; the saved sign-in
  on the next start; Log out clearing it; a second client (`perf4` or `perf1`, whichever is offline)
  coming online and leaving as live roster deltas seen by `perf3`; and in Rustaceans what `dave` (member)
  and `bob` (owner) may do in `core-team` / `announcements` / `showcase`. Not covered: resume after a
  forced disconnect (nothing cheap drops one connection without restarting central, which other work
  shares).
- A new stack test: write the steps as a `&[&str]`, `run_ok(profile.path(), steps)`, assert on
  `run.dump("<what>")`; two clients at once use `Live::start` + `until("MARK …")`. Add a `dump` to
  `headless.rs` for a new store. Never `reset` / `seed` the stack; only accounts of `accounts.json`.

## 10. How to

**Add a store.** A plain struct in `stores/`, a field on `Model`, `cx.new(..)` in `Model::build`. Methods
take `&mut Context<Self>` and call `cx.notify()` only when something changed; add an `EventEmitter` event
when small views need to tell their key apart. Keep per-community state in `Community` (a new entity there
if it changes on its own rhythm). Clear it in `events::auth::logged_out` (or the `todo::*::logged_out`
hook). How its requests leave is a closure given at construction if it has timers or batching to test.

**Route an event.** `events/todo.rs` already has a function for every unhandled variant and hook, with the
renderer code it ports: replace its body with the store call (or move it to the feature's `events/<area>.rs`
and point the router's arm at it). Events that also name users are already fed to the directory. An answer
a view waits for: add `answer(model, E::X(p), cx)` to its arm (after the stores).

**Call the API.** `call(cx, api::area::function(args), |result, cx| …)` or `call_or_toast(cx, "Couldn't …",
api::…)`; inside a view that should cancel with itself, `cx.spawn(async move |this, cx| { let r =
api::…(args).await; this.update(cx, …) })`. Most calls only queue a request: `Ok` means sent, the answer is
an event. Errors: `UiError` (`text` verbatim, `kind`, `cert`); `show_error` is the one way to show one
(re-trust prompt for a pin failure, else a toast); `offer_retrust(&e, Some(retry), cx)` when there is
something to run again. Never call `api` before `core::is_running()` (the helpers check).

**Persist a preference.** Chosen in Settings → a field of the core's `AppSettings`, changed with
`Settings::change`. Per-install view state → a typed field of `UiPrefs` with a clamping setter on `Layout`
(and a `LayoutEvent`), or `Layout::set_extra(key, Some(&value), cx)` for a cache.

**A settings save that settles on the echo** (`save_echo.rs`): `EchoSave<T>` in the view; `start(sent, cx,
|this| &mut this.save)` after sending, `settle(|sent| store_shows(sent))` in the store observer (drop the
draft when it returns `Some`), `stop()` on the refusal (from `model.answers`). The 5 s limit is a timer
held only while a save waits.

**A timer.** Only for a named live reason, as a `Task` stored in the state it serves and dropped with it:
see `Toast::_lifetime`, `Roster::lookups.flush`, `Settings::save`, `EchoSave`'s limit. A task that clears
its own slot must do everything else inside that same `update` (dropping the slot cancels the task at its
next await).

## 11. Where this differs from the renderer, on purpose or not

- **Mirrored although it looks wrong**: the sign-in page's error and the sign-up failure keep JS's
  `"Error: "` prefix (`setLoginError(String(err))`): `UiError::js_string()`. Central's own refusals
  (`login_failed`) have none. `AuthStore::logout` leaves `renames` / `devices` / `is_registering` as they
  were. `Connections::central` is never set to `Disconnected` (nothing sets it in the renderer either), and
  sign-out leaves it as it was. `mergeServers` keeps the first entry: a community renamed while we were
  offline shows its old tile name until `server_meta_updated` (central's membership row is what was merged
  first).
- **Changed**: a member's role ids are de-duplicated on arrival (`community::distinct`). The community
  server sends the requester's own roles twice on a first roster page that also lists them (§12).
- **Line drawn in `chatStore`**: everything about messages is M2's — `messagesByChannel`,
  `pendingDeletions`, `hasMoreHistory`, `hasMoreAfter`, `historyLoading`, `historyFetched`,
  `scrollPositionsByChannel`, `chatViewSize`, `channelAccessOrder` (the LRU: follow
  `NavEvent::ChannelActivated`), `jumpRequest`, `resyncEpoch`, `resyncing`, `heldLive`,
  `displayedChannelKey`, and the mutators from `addMessage` to `setChatViewSize` plus `markServerStale` and
  `purgeChannelState` (hooks `m2::community_authenticated`, `m2::purge_channel_state`). The rest is here.
- `uiStore`'s `dragActive` / `dragHoveredKey` (attachment drag-and-drop) are not ported: M2, as their own
  small entity (they change on every pointer move over a target).

## 12. Needed from the core and the skeleton (not changed here)

1. **Core: `zbus` feature.** `crates/decibell-core/Cargo.toml` has `zbus = { version = "5",
   default-features = false, features = ["blocking-api", "tokio"] }`. In this workspace that turns GPUI's
   own D-Bus use into panics (§2). Replacing `"tokio"` with `"async-io"` (what GPUI's `ashpd` uses) removes
   the clash; the core's three users (`system_idle.rs`, `hotkeys/portal.rs`, `media/capture_pipewire.rs`)
   would need Electron's bar re-run. Until then **`decibell-app` must call `core::prepare`** as in §2, or a
   release build aborts at startup on Linux as soon as it links the core. **This already reaches the
   app through the workspace**: cargo unifies features over everything one command builds, so
   `cargo build --workspace` (with this crate a member) builds a `decibell` whose `zbus` has `tokio` on,
   although the app does not depend on the core yet. `cargo build -p decibell-app` is unaffected. Checked
   in an isolated session bus with a scratch keyring: with `prepare`, GPUI's keychain calls work (the key
   is created, the saved sign-in is sealed under it and opens on the next start); without it they panic.
2. **Skeleton (`decibell-app`)**: the boot sequence of §2 (`current_platform` → `prepare` →
   `Application::with_platform`; `Model::init`; `Model::start_core`; `core::shutdown` after `run`), the
   `SettingsEvent::Appearance` subscription of §7, the initial theme from `layout.prefs()`, and
   `decibell-model = { path = "crates/decibell-model" }` in `[workspace.dependencies]` (left out: an unused
   workspace dependency warns on every build). The shipped binary must not enable this crate's
   `local-central` feature.
3. **Core: no way to drop one connection** for a test (resume after a forced disconnect is untested here).
4. **Community server** (`src/community/main.cpp`, the first-page branch of the member list): the
   requester's uid is added to the role query's uid list a second time when their row is already in the
   page, so their `role_ids` arrive doubled. Electron shows the same data.
5. **Windows check job** (`.github/workflows/win-native-check.yml`, job `gpui-client`): with this crate a
   workspace member, `cargo check` of the workspace now builds `decibell-core` on Windows too, so that job
   needs what the addon's job has for the core (FFmpeg through vcpkg, protoc). This crate's Windows side is
   written to compile (`smol` and the thread adoption are Linux-only) but was not built there.
6. `cargo clippy -p decibell-model` prints the core's own warnings (14; unused imports and the like in
   `media/`): a path dependency outside the workspace is not lint-capped. None are this crate's.

## 13. Measured (2026-11-26, this machine; see HANDOFF.md §4 for the machine)

`decibell-headless` (release, fat LTO, `--features local-central`; 25.0 MB stripped, FFmpeg static: `ldd`
lists no `libav*` / `libsw*`), signed in as `perf2` with Daily Gaming connected and its 298-member roster
loaded, no window, then idle. `tools/perf/measure.sh --pid … --settle 20 --duration 60 --gpu none
--no-frames`:

| Metric | Value |
|---|---|
| Private memory (`Private_Clean + Private_Dirty`) | **14.2 MB** (private dirty 2.6 MB) |
| PSS / RSS | 14.2 MB / 17.0 MB |
| CPU | 0.0004 % of one core (0.25 ms in 60 s) |
| Threads | 26, every one named |
| Wakeups, whole process | **0.17 / s** (10 in 60 s) |
| File descriptors | 25 |
| Events through the pump during the idle 105 s | 0 (`dump pump` before and after: 9 events, 7 wakes, 7 drains) |

| Thread (count) | Whose | Wakeups in 60 s | Why |
|---|---|---|---|
| `decibell-rt` (2) | this crate: the core's tokio runtime | 10 (one of the two; 0.17 / s) | the 30 s keep-alive pings of the two connections (central, Daily Gaming) and their answers |
| main (1) | GPUI's event loop | 0 | parked in `epoll_wait`: no core event arrived, so the pump never woke it |
| `Worker-0` … `Worker-19` (20) | GPUI's background executor, one per CPU | 0 | parked |
| `Timer` (1) | GPUI's timer thread | 0 | holds the script's one `idle` timer |
| `smol-1`, `async-io` (2) | smol's executor and reactor (GPUI's power listener; started by `core::prepare`) | 0 | parked |

No hotkey or audio-device watcher runs in this state (no binding, no voice): they appear only with their
cause. So the core + model's share of the client's memory budget (Electron's whole idle footprint here is
~340 MB private; the target is ≤ 114 MB with the GPU driver set aside) is about **14 MB, 12 % of the
target**, and its share of the wakeup budget (≤ 30 / s) is 0.17 / s. The 20 parked GPUI workers cost stack
address space, not memory (they are in the 14 MB).

## 14. For the two follow-ups

Both: read §4 before designing a store; fill in `events/todo.rs`; add `dump`s to `headless.rs` and tests to
`tests/stack.rs`; keep every thread named and every timer owned.

**M2 — messages, DMs, history merge, send pacing, slowmode, drafts, attachments, link previews, search.**

| Piece | Renderer |
|---|---|
| Message store: per-channel slices, `mergeMessage`, optimistic bubbles, pending deletions, edits, prune / wipe, LRU of 10 channels, background cap of 150, windowed slices, re-sync after reconnect | `stores/chatStore.ts` (message half: §11), `lib/historyMerge.ts`, `lib/channelKey.ts`, `features/chat/useChatEvents.ts`, `features/chat/historyRequests.ts`, `features/servers/useServerEvents.ts` (the four `channel_message_*` listeners) |
| DMs | `stores/dmStore.ts`, `features/dm/useDmEvents.ts` |
| Send pacing, slowmode | `features/chat/sendPacing.ts` (10 burst / 2.7 per s; `legacyRejectionSuppressed`), `features/chat/slowmode.ts` |
| Drafts | `stores/draftsStore.ts` |
| Attachments | `stores/attachmentsStore.ts`, `features/chat/uploadAttachment.ts`; the service is `crates/decibell-attachments`; hooks `m2::attachment_target_changed`, `m2::session_token_updated`, `m2::community_connection_lost` |
| Link previews | `stores/linkPreviewStore.ts`; `crates/decibell-unfurl`; the Privacy toggles are `Settings` fields (`link_previews_enabled`, `gif_unfiltered`) |
| Search | `features/search/` (`useSearchEvents.ts`, its store); nothing about a search is stored on the device |
| Also | `stores/imageViewerStore.ts`, `imageContextMenuStore.ts`, `transfersStore.ts` (with M3 if it lands there), the drag state (§11), `friends_only_dms` (`dmStore`; `Settings` holds it, `own_presence` adopts the account's value) |

Shape hints: one entity per loaded channel slice (a message in one channel must never notify another
channel's list), the registry notifying only on create / evict; payloads arrive by value in `todo::m2`, so
move their strings into the store; message ids and timestamps are `i64` (no BigInt problem here);
`MessageReceivedPayload::timestamp` is a decimal string.

**M3 — voice, streams, call, presence + status, friends, E2EE, hotkeys, devices, audio settings, transfers.**

| Piece | Renderer |
|---|---|
| Voice + streams | `stores/voiceStore.ts`, `voiceStatsStore.ts`, `features/voice/useVoiceEvents.ts`, `features/voice/userGain.ts`; `voice_force_notify` in `useServerEvents.ts`; frames go to `StreamFrameSink` (the video crate), thumbnails to `m3::stream_thumbnail` |
| Calls | `stores/callStore.ts`, `features/call/` |
| Presence + status | `stores/presenceStore.ts`, `features/presence/` (`presenceInterest.ts`: the watch set; the client never watches itself), `stores/avatarStore.ts` |
| Friends | `stores/friendsStore.ts`, `features/friends/` |
| E2EE | `stores/e2eeStore.ts`, `features/e2ee/`; modal ids in `overlays::modal` |
| Hotkeys | `stores/hotkeysStore.ts`, `features/hotkeys/`; bindings live in `Settings` (`hotkeys`, `input_mode`, `ptt_release_delay_ms`) |
| Devices | `stores/devicesStore.ts`, `features/settings/deviceLabels.ts`; the `devices_changed` toast (20 s, action → Settings → Devices: `nav.set_settings_tab("devices")` + `overlays.open_modal(modal::SETTINGS)`) |
| Audio settings | `stores/audioDevicesStore.ts`, `hooks/useAudioDeviceSync.ts`; the values are `Settings` fields |
| Transfers | `stores/transfersStore.ts`; `download_dir`, `ask_download_location` are `Settings` fields |

Shape hints: speaking and input level are the high-rate events — each gets its own tiny entity (a speaking
ring must not re-render the voice stage; the mic meter must not touch the voice store); presence separate
from names, keyed by uid, with a keyed event; `m3::clear_server_presence(server, keep)` is called from
four handlers here.
