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

## 15. M2: messages, DMs and what the chat views read

Everything about messages (the message half of `chatStore`, `dmStore`, `historyMerge.ts`,
`historyRequests.ts`, `sendPacing.ts`, `slowmode.ts`, `draftsStore`, `attachmentsStore`,
`linkPreviewStore`, the search store, the small viewer stores) lives in two module trees, exposed on
`Model` as one field each: `model.chat: ChatStores` and `model.dm: DmStores`.

```
src/chat/            mod.rs (ChatStores), message.rs, merge.rs, slice.rs (+ slice/tests.rs), cache.rs,
                     history.rs, pacing.rs, slowmode.rs, drafts.rs, composer.rs, link_previews.rs,
                     search.rs, viewers.rs, time.rs (local days, clocks), topic.rs (the topic field's clamp)
src/dm/              mod.rs (DmStores, DmList), conversation.rs (the DM side of MessageSlice),
                     read_sync.rs, tests.rs
src/events/chat.rs   every m2 event and hook (events/chat/tests.rs: router tests); events/dm.rs
src/actions/chat.rs  what the chat views ask for; actions/dm.rs the DM twin
src/bin/headless/chat.rs   the headless message steps; tests/stack_chat.rs
```

### 15.1 Entities and when they notify

| Entity | Owns | Renderer origin | Notifies on |
|---|---|---|---|
| `chat.channels: ChannelCache` | `(server, channel)` → `Entity<MessageSlice>`; the LRU of visited channels (10); the displayed channel; per-community `resync_epoch`; the outside jump request | `channelAccessOrder`, `displayedChannelKey`, `resyncEpoch`, `jumpRequest` | a slice created or dropped; `CacheEvent::Resync(server)`, `CacheEvent::JumpRequested` |
| `MessageSlice` (one per channel, one per DM peer) | rows (`Arc<Message>`) + facts + the change log; `has_more_before/after`, `loading`, `fetched`, `resyncing`, held live rows, scroll position, pending deletes (each with its 5 s watchdog), echo watchdogs (30 s), paging guards | `messagesByChannel` and the seven per-channel maps, `heldLive`, `pendingDeletions`, the panels' `lastRequested*Ref` | any change to that one channel / conversation |
| `chat.history: HistoryRequests` | waits on history replies (10 s each), shared newest-page loads | `historyRequests.ts` | never (nothing draws it) |
| `chat.pacing: SendPacing` | per-community token bucket + FIFO queue; the per-community call chain; the legacy-refusal suppression | `sendPacing.ts` | never |
| `chat.slowmode: Slowmode` | per-channel "next send allowed at" | `slowmode.ts` | `SlowmodeEvent::Changed(key)` / `Nudged(key)` |
| `chat.drafts: Drafts` | channel and DM drafts (memory only) | `draftsStore.ts` | `DraftsEvent::Changed(key)` / `Restored(key)` |
| `chat.composer: ComposerAttachments` | attachments in message boxes: status, progress, ids, sealed metadata, claims | `attachmentsStore.ts`, `uploadAttachment.ts` | `ComposerEvent::Changed(server, channel)` |
| `chat.link_previews: LinkPreviewMemo` | 400 unfurls by URL (loading / done) over `decibell-unfurl` | `linkPreviewStore.ts` | `PreviewChanged(url)` |
| `chat.search: SearchStore` | the one search: query, sort, seq, results, paging, per-community box drafts | `features/search/searchStore.ts` | search state |
| `chat.viewer`, `media_menu`, `audio`, `video`, `video_versions`, `drag` | image viewer, image / video context menu, playing audio (+ mirrored playback state), playing video, per-attachment video cache version (`VideoCacheBumped(id)`), file drag state | `imageViewerStore`, `imageContextMenuStore`, `activeAudioStore`, `activeVideoStore`, `videoCacheVersionStore`, `uiStore.dragActive/dragHoveredKey` | their own change |
| `dm.list: DmList` | per peer a `Conversation` (its `MessageSlice` entity, last message, its time, unread, read cursor); the open peer; the displayed peer; the DM send chain; the composer's inline send error | `dmStore.ts`, `features/dm/readSync.ts` | a conversation's sidebar facts (`DmEvent::Changed(peer)`), `ActiveChanged`, `Resync` |

A message notifies its own slice only. The registry notifies when a slice is created or dropped (a
list of channels never redraws for a message). The DM list notifies when the sidebar's facts of one
conversation change; the rows of a conversation are its slice.

### 15.2 The slice and the list view (the hot path)

`MessageSlice::rows()` is the ordered window (stored rows by id, optimistic sends at the tail; DMs keep
live arrivals where they land until a page sorts them), each row an `Arc<Message>` with a stable
identity `Message::key()` (`RowKey::Id`, else `RowKey::Nonce`). `facts()` is parallel to it: what the
renderer computed per row in `renderBubble`, now once per change: `grouped` (same sender, same local
day, < 7 min, not a reply), `new_day` (day divider), `own`, `edited`, `can_edit` (channels: not a sealed
message in a plaintext channel; DMs: opened), `can_reply`, `can_delete_own`, `others_may_delete` (with
`RowFacts::can_delete(can_delete_others)`, `actions::chat::can_delete_others` = server-wide Manage
Messages, as `useCanDeleteOthers`), `replies_to_me`. Text is `SharedString`; nothing is parsed here.

Every change to rows or facts is logged as `RowOp`s under a revision:

- `Splice { at, removed, inserted }`: at `at`, `removed` rows went and `inserted` came;
- `Update { at }`: same identity, new content / flags / facts (an edit, a send shown as sent, the
  next row re-grouping after a neighbour came or went).

The list keeps the revision it drew and a per-row array. On the slice's notify it calls
`changes_since(seen)`: `Changes::Ops(ops)` replays in order (splice its array with placeholders, mark
updates dirty), after which its array is aligned 1:1 with `rows()` and every untouched entry is
guaranteed unchanged (same `Arc`, same facts: checked by the property tests); it builds the
placeholders and dirty entries from `rows()[i]` / `facts()[i]` and sets `seen = revision()`.
`Changes::Reset` (the list is more than 256 ops behind, or a change reordered rows: rare, a DM page
sorting live arrivals) means rebuild, reusing whatever it caches by `RowKey`. Costs: a live message is
one `Splice` (plus one `Update` if the bubble after it re-groups), an older page one `Splice` at 0, the
echo of our send one `Splice` of 1 for 1 at the bubble's place, an edit one `Update`, a trim one
`Splice`. Wholesale changes (a merge that repairs the slice) go through a one-pass diff keyed by row
identity.

Rules for the list view, besides the replay:

- **Observe the slice it shows** (`cx.observe(&slice, …)`), never the registry for rows.
- Mount: `chat.channels.update(|c, _| c.set_displayed(Some(key)))` (it keeps its slice bounded itself);
  unmount: `set_displayed(None)`. Leaving: `slice.set_scroll(ScrollPosition { at_bottom, anchor_id,
  offset })`; mounting restores from `scroll()`.
- Trims: `trim_tail(keep)` / `trim_head(keep)` when it grows past its pixel budget.
- Paging: near the top `actions::chat::load_older`, near the bottom while `list_windowed()`
  `load_newer`; a `false` answer → re-check edges after `history::RETRY_DELAY`. "Jump to present":
  `jump_to_present`. Jump to a message: `jump_to_message` (`JumpStart::Loaded` → land now;
  `Fetching(task)` → land on the commit that holds it, then `jump_landed`).
- The panel follows `CacheEvent::Resync(server)` for the channel on screen:
  `resync_displayed(server, channel, at_bottom)`; and takes `take_jump_request(server, channel, now)`
  once the first page is in.
- First visit: `open_channel(server, channel)` (loads the newest page unless `fetched`); the slice's
  `loading` / `fetched` drive "Loading history…" and Retry (`load_latest` resolves `false`).

### 15.3 Sending, and the timers

`actions::chat::send_message(server, channel, text, reply_to)` → `SendOutcome`: `JumpedToPresent`
(windowed: the draft stays), `HeldBySlowmode` (text and attachments stay, the pill shakes via
`SlowmodeEvent::Nudged`), `Nothing`, or `Sent { nonce }` (clear the box and the reply target). The
bubble is on screen at once (faded: `Message::pending`); queued attachments start uploading; once they
all ended (event-driven, no polling) the message goes to the community's bucket (`chat::pacing`: 10
burst, 2.7 / s, FIFO per community, reset when the community authenticates) and then to the core,
after the previous send of that community (`call_in_order`). Refused (`channel_message_rejected`): the
bubble goes, the text comes back (`Drafts::restore` + `DraftsEvent::Restored`), the reason toasts and
a slowmode refusal corrects the window. Not echoed within 30 s: the echo watchdog asks the server
(after-pages of 100 from `sent_after_id`, up to 5) before withdrawing; offline it waits for the
reconnect. `send_gif`, `edit_message` (paced, not optimistic), `delete_message` (optimistic, 5 s
watchdog), `latest_own_editable` (ArrowUp), `attach_file` / `set_draft` complete the composer. DMs:
`actions::dm::{open_dm, open_history, load_older, load_newer, jump_to_present, jump_to_message,
resync_displayed, send_dm, send_gif, edit_dm, delete_dm, mark_read, set_draft}`; DM sends are not paced
but leave one after another; `mark_read` reads up to the newest loaded row and tells central 250 ms
later.

Timers, each with its reason and dropped with it: the pacer's wait for the next token (only while
sends are queued); a history request's 10 s reply wait (only while it waits); a newest-page load's
1 s retry delay (only between attempts); the 30 s echo watchdog (only while its bubble is pending:
the slice drops it when the bubble settles or goes); the 5 s delete watchdog (only while the delete
waits); mark-read's 250 ms; search's 8 s "no answer" (only while a page is awaited) and its retry
after a "too fast" answer. Slowmode keeps no timer (the composer's pill counts down while shown:
`Slowmode::remaining`). At rest the model holds none (measured below: 0 wakeups from it).

### 15.4 What views observe

| View | Observes / subscribes |
|---|---|
| Message list (channel) | its `MessageSlice` (observe); `Settings` for the time format; the users store's `NamesChanged` for its senders |
| Chat panel chrome (header, pills, Retry) | its slice (flags), `CacheEvent` (resync, jump) |
| Composer | `Drafts` (`Changed` / `Restored` for its key), `ComposerEvent` for its channel, `SlowmodeEvent` for its channel, the channel's `my_permissions` (`Channels`) |
| Link preview card | `PreviewChanged` for its URL (request on row build when the Privacy toggle allows) |
| Search panel | `SearchStore` (observe) |
| DM sidebar | `DmList` (observe; `sorted()`, `total_unread()`) |
| DM conversation | its `MessageSlice` (observe), `DmEvent::Resync` / `ActiveChanged`, `DmList::send_error` |
| Image viewer, media menu, players, drag overlay | their own small entity |

### 15.5 Where this differs from the renderer

Mirrored although it looks wrong (listed, not fixed): `channel_pruned` ignores the attachment
tombstones it carries (the renderer drops only the deleted messages; purged attachments keep showing
until a reload); a prune or a wipe creates the channel's slice even for a channel never visited (so it
then caches live messages up to 150); `restoreChannelDraft` counts a whitespace-only draft as "typed"
while `restoreDmDraft` doesn't; DM mark-read sends one marker per call (the "coalesce" comment never
coalesced); `markServerStale` keeps a channel's slice that holds only a scroll position or a flag even
when the channel is gone; the DM composer's send error is a string from `String(err)` ("Error: …").

Changed on purpose: (1) **sends and edits of a community leave one after another** (`call_in_order`):
the core runs calls concurrently and a burst admitted at once reached the server out of order (the
stack's 15-send burst arrived as 1, 7, 2, 3, 4, 10, … before this); the renderer intends FIFO. (2) A
reply with no cursor echoed is the newest page **unless it answers a waiting older-page request**
(`HistoryRequests::waiting_before_cursor`): an older server that doesn't echo `before_id` reads as 0,
the 2026-10-28 incident. (3) Resetting the pacer on community auth refills the bucket and serves the
sends still queued first, on that bucket (the renderer forgot the queue's bucket and let it drain on
the old one beside the new). (4) The echo watchdog lives with its bubble and is dropped when the bubble
settles (the renderer's timer always fired). (5) Pending deletions live in their slice: an evicted
channel forgets them (the renderer kept the map entry and re-inserted into an evicted slice). (6) The
DM echo check waits for central's next sign-in event-driven (`DmList::wait_for_resync`), as the
channel one does. (7) Search's `fold` drops combining marks and maps precomposed Latin letters to their
base without NFD (no Unicode-normalization crate in the tree); other scripts pass lower-cased.
(8) `TimeFormat::Auto` is decided from `LC_ALL` / `LC_TIME` / `LANG` (12-hour regions), not Intl.

### 15.6 Lines added to shared files

`lib.rs`: `pub mod chat; pub mod dm;`, the fields `chat: chat::ChatStores` and `dm: dm::DmStores`,
their two `…::new(cx)` lines in `Model::build`, and `model.chat.follow_navigation(&model.nav, cx);` in
`Model::init` (the channel LRU follows `NavEvent::ChannelActivated`). `events/mod.rs` and
`actions/mod.rs`: `pub mod chat; pub mod dm;`. `events/todo.rs`: each `m2` body is one call into
`events::chat` / `events::dm` (and the doc of `legacy_rejection_suppressed`). `Cargo.toml`:
`decibell-attachments`, `decibell-unfurl`, `chrono` (clock; already in GPUI's tree) and `tokio-rustls`
(the connector type of `CoreTls`; already the core's). `bin/headless.rs`: the `mod chat` include, a
`Step::Chat` variant and its parse / run arms, `chat` / `dm` in the dump argument count, the
`chat | dm | search` dump arm, the usage print, `say` made `pub(crate)`.

### 15.7 Needed from elsewhere

- **Core**: `tls::exact_connector(fingerprint)` (side-effect free). `chat::composer::CoreTls` today
  calls `pins::set_expected(host, port, fp)` then `tls::create_pinned_connector(host, port).0`; the
  switch is that one body. A hook for **central's** `connection_lost` to abandon DM history waits
  (`useDmEvents`' listener): today they end by their 10 s timeout or at the next sign-in.
- **M3**: nothing is read from M3's stores. The E2EE status (locked / not set up) that the DM
  composer's banner shows is M3's `e2eeStore`; after an unlock the renderer calls
  `invalidateAllHistory`: `DmList::invalidate_all_history` is here for M3's unlock handler to call
  (then the active conversation's `open_history`). `friends_only_dms` is a `Settings` field.
- **The app**: a `MediaProbe` for video / audio facts at attach time (the composer probes pictures
  only: `upload::prepare(source, None)`); the Settings → Network speed caps (`Attachments::limits()`,
  reachable through `CoreAttachmentService::attachments()`).

### 15.8 Tests and measurements

`cargo test -p decibell-model`: the M2 unit tests (history merge cases, the slice under 40 random
sequences of 400 events each checking after every step that stored ids are unique and ascending,
bubbles sit at the tail, facts equal a fresh computation and the change log replays to exactly the
rows; pacing, slowmode, echo / delete / search timers on GPUI's fake clock; DM unread and read-sync
arithmetic; drafts; search supersede and retries; history waits) and the router tests
(`events/chat/tests.rs`: events through the real queue and pump). `tests/retained.rs` counts what a
slice retains with a counting allocator. `tests/stack_chat.rs` against the local stack:

```sh
eval "$(../tools/dev/local-stack.sh client-env)"
DECIBELL_STACK=1 cargo test -p decibell-model --test stack_chat -- --nocapture --test-threads 1
```

It pages `#general` back four pages and opens a window around a deep id in a fresh client; sends,
edits and deletes in `#off-topic` while a second client sees each as a live event; sends a burst of 15
(10 at once, 5 paced, all delivered, in order); holds a second send in `#slow-lane` and delivers it once
the window passed; sends a DM perf2 → perf3 seen unread on perf3's first device and cleared there by
`DM_READ_SYNC` when perf3's second device reads it; searches "wayland". Every test deletes what it
posted (and checks it against a fresh page from the server).

Measured (2026-11-26, this machine, the release `decibell-headless` with `local-central`: 28.6 MB
stripped, +3.6 MB over the foundation's 25.0 for the attachment service's image codecs and the
unfurler, which the app links anyway). `tools/perf/measure.sh --pid … --settle 20 --duration 60 --gpu
none --no-frames`, signed in as `perf2` with Daily Gaming connected and its roster in, then idle:

| State | Private (dirty) | PSS / RSS | Threads | Wakeups / s (whose) |
|---|---|---|---|---|
| A: nothing opened | 15.6 MB (2.77) | 15.7 / 18.4 MB | 26 | 0.20 (`decibell-rt`: keep-alives) |
| B: `#general` shown, its newest page (50 rows) | 15.9 MB (2.89) | 16.1 / 18.9 MB | 26 | 0.17 (same) |
| C: all 7 text channels shown, `#general` paged to 150 rows (≈ 350 rows in 7 slices) | 16.3 MB (3.40) | 16.5 / 19.3 MB | 26 | 0.17 (same) |

No thread, timer or wakeup of the model's at rest in any state (the main thread and GPUI's workers
parked; every wakeup is the core's two keep-alives). What a slice keeps, counted exactly
(`tests/retained.rs`, rows shaped like `#general`'s: 74 bytes of text on average, 1 in 20 with an
attachment, 1 in 20 a reply): **340 bytes a message** requested from the allocator (about 400 with
malloc's own overhead): the row (`Message`, 200 bytes, in its `Arc`), its text and sender as shared
strings, its slot and facts; channel ids and empty lists are shared, never per row. A 50-row page is
~17 KB; the cap of 10 visited channels at the background cap of 150 rows is ≤ 1,500 rows ≈ **0.6 MB**
worst case. The process figures move more than that (B +0.13 MB, C +0.63 MB private dirty over A):
decode buffers of the pages are freed but kept by malloc's arenas, and the core keeps per-channel
state of its own.

## M3. Voice, streams, calls, presence, friends, E2EE, hotkeys, devices, transfers

The live / people half (`src/live/`, routed from `events/todo.rs`' `m3` functions to
`events/{voice,calls,presence,friends,e2ee,misc}.rs`, actions in `actions/{voice,calls,presence,friends,e2ee,devices,
hotkeys,transfers}.rs`). One grouping struct on `Model`: **`model.live: LiveStores`** (handles, cheap to clone),
installed by `live::install` from `Model::init` (the observers that follow other stores). Calls live in
`live::calls` / `events::calls` / `actions::calls` because `crate::call` (the "call the core" helper) already
exists.

### M3.1 Entities and their rhythm

The rule of §4 decided every split: a change notifies only the entity whose state changed, and where many
small views each show one key, the entity emits a keyed event they filter on.

| Entity (`model.live.…`) | Holds | Notifies on | Keyed event | Who observes / subscribes |
|---|---|---|---|---|
| `voice.session` (`VoiceSession`) | connected (community, channel) or the call's peer, our mute / deafen, the channel's MLS state, the engine's error | join / leave / switch / move, `voice_state_changed`, MLS state, error | — | user panel, dock, call bar, voice header (observe) |
| `voice.participants` | the stage's rows (connected channel, or [us, peer] in a call) | every roster change of the connected channel, a participant's mute flip | — | the voice stage (observe) |
| `voice.rosters` (`VoiceRosters`) | every voice channel's roster on every connected community, members' flags, capabilities, our pending moves (`SelfMove`) | per batch | `VoiceRosterEvent::Changed(keys)` | the channel list subscribes and notifies only for its community's channels; never observe it from a pane |
| `voice.speaking` | who is speaking | **high rate**: every start / stop | `SpeakingChanged { uid, speaking }` | a tile / row shows one person: **subscribe and filter by uid**; nothing that draws the whole stage may observe it |
| `voice.input_level` | our mic level (dB) | ~16 Hz, only while a meter is retained (`retain_meter` switches the core's reporting on) | — | the Audio tab's meter only |
| `voice.stats` | last ping, ≤ 200 samples (a 30 s gap restarts the history) | ~0.5 Hz in a session | — | the signal icon, the connection popover |
| `voice.streams` (`StreamPresence`) | every live stream by owner across communities; a DM call's streams | stream start / stop / codec / viewers | `StreamsChanged { key }` | LIVE badges (subscribe), the stage, the profile popup |
| `voice.thumbnails` | posters as `Arc<gpui::Image>` (encoded JPEG; GPUI decodes when drawn) + version | a poster (~15 s per stream) while the voice view retains them | `ThumbnailChanged(uid)` | stream tiles (subscribe) |
| `voice.watching` | watched streams, focused, mini player, window fullscreen | user actions | — | stage, mini player, **the app** (applies `fullscreen` to the window) |
| `voice.stream_views` | how many views draw each stream | never notifies | — | — (`actions::voice::show_stream` / `hide_stream` switch the core's frame sink: the video hub receives only what is displayed) |
| `voice.own_stream` | idle / starting / live (codec, size, fps, audio, where), the Go Live error, cursor option, Windows source | Go Live, stop, failures | — | user panel, Go Live dialog |
| `voice.devices`, `voice.stream_audio_apps`, `voice.codecs` | audio device lists; stream-audio apps; codec toggles and caps | a refresh that changed something, a toggle | — | Settings → Audio / Codecs, the Go Live picker |
| `calls` (`Calls`) | the call state machine; the 45 s ring timer (only while ringing) | a handful of transitions per call | `ShowConversation(uid)` (accepting lands on the caller's DM: **M2's DM store subscribes**) | DM header, call stage, incoming card |
| `presence.statuses` (`Presence`) | watched users' statuses + who central answered for | a watched user's change; a whole subscription answer is one batch | `PresenceChanged(uids)` | a dot / avatar (one person) subscribes and filters; a member list observes |
| `presence.messages` | watched users' custom statuses (apart, so an edit doesn't regroup lists) | idem | `MessagesChanged(uids)` | idem |
| `presence.own` (`OwnPresence`) | our status, message + clear-after timer (only while a deadline is set), idle, account idle, supported | our choices, any device's `own_presence` | — | user panel, status menu |
| `presence.interest` | the watch set (ref-counted), 600 ms batching (timer only while a change waits) | never notifies | — | views retain / release (`actions::presence::retain`) |
| `presence.auto_away` | the idle driver: one timer (M3.4) | our idle flag | — | — |
| `presence.avatars` | pictures by uid: version, `Arc<gpui::Image>`, state; 6 fetches at a time, newest first, skips users off screen; ≤ 400 loaded pictures of users off screen | announced versions, finished fetches | `AvatarsChanged(uids)` | avatars subscribe and filter; call `fetch_if_needed(uid)` where the row is built |
| `friends` | the list, the inline action error (5 s timer while shown), the list request's retries (10 s × 3, only while waiting) | a list, a presence flip of a friend | — | Friends page, DM sidebar |
| `e2ee` | status, fingerprint, changed peers, the nudge, **the passphrase dialog** (`PassphraseFlow`: mode, busy, error) | status pushes, flow steps | `E2eeEvent::BecameReady` (**M2 reloads DM history**), `PeerChanged(uid)` | DM panel, Settings → Privacy, the dialog |
| `hotkeys` | the listener's status, recording, the focused fallback (the core's own `Matcher`) | status, recording | — | Settings → Keybinds |
| `devices` | the device list, sign-out busy / confirm, the change-password flow | load, steps | — | Settings → Devices, the password dialog |
| `transfers` | downloads (mirror of the manager), this session's uploads, panel state, attention dot | ~4 Hz per active transfer | — | the title-bar button, the open panel; **the app** (progress, quit guard) |
| `cues` (`Cues`) | sound / attention triggers | never notifies | `Cue` | **the app's audio layer and window** |
| `notices` | the revocation notice's 8 s | — | — | — |

Per-uid entities were considered for presence and rejected: the watch set is hundreds of users that come and go
as lists scroll, each change is one user, and a keyed event costs one cheap filter per subscribed view against
creating / dropping hundreds of entities with their observers.

### M3.2 What the app wires (traits and glue)

| What | Where | Notes |
|---|---|---|
| Sounds and attention | subscribe to `model.live.cues` | `Cue::Play(Sound)` (`Sound::asset()` → `assets/sounds/*.wav`), `StartRing` / `StopRing` (loop `call_ring.wav`), `Flash` (taskbar flash / dock bounce without focus). The model plays nothing. |
| Decode capabilities | `codecs.update(|c, _| c.set_decoder_probe(Rc<dyn DecoderProbe>))` | The video crate's probe; the model adds the H.264 fallback, caches in `ui.json` `extra["decoder_caps.v6"]`. Without one (headless) only H.264 is advertised. |
| Codec caps at boot | `actions::voice::init_codec_caps(false, cx)` once after `start_core` | As `main.tsx` did: decode probe, the core's `probe_native_encoders` (cached `extra["native_encoder_caps.v2"]`, one re-probe 15 s after a cached boot), shipped to the core (its caps are in memory: every start). Before it, a voice join advertises no caps. |
| Idle source | `auto_away.update(|a, cx| a.set_source(Rc::new(CoreIdleSource), cx))` or the platform's own `IdleSource` | The core's answers Wayland + GNOME, "unsupported" on X11 / Windows / macOS (Electron used `powerMonitor`): the app supplies `XScreenSaver` / `GetLastInputInfo` / `CGEventSource` sources there, ideally with `idle_for`. Also `set_locked(bool)` on lock / sleep, `poke()` on resume pushes and on input in our window. |
| Device hot-plug | `devices.update(|d, cx| d.hotplug(cx))` | Electron had `devicechange`; GPUI has none (PipeWire / WASAPI notifications are the app's). 400 ms debounce, timer only while pending. Pickers call `ensure_loaded`. |
| Downloads | `transfers.update(|t, cx| t.attach_downloads(Rc<dyn DownloadControl>, cx))` → `DownloadEvents`; give `move |e| events.push(e)` to `DownloadManager::new` | `ManagerControl { manager, runtime }` implements the trait. Then `actions::transfers::configure_downloads`. |
| Uploads | **M2**: `actions::transfers::sync_uploads(&[PendingUpload], cx)` when pendings change; `t.set_upload_control(Rc<dyn UploadControl>)` | Assumed: M2's pending has a status (queued / pending / uploading / ready / failed), paused, cancelled, error, preview image, attachment id, sizes. |
| Taskbar / quit guard | observe `transfers`: `progress()` (fraction or none, paused-only) and `uploads_in_flight()` | Electron throttled the platform call to 4 / s. |
| Window | observe `voice.watching.fullscreen`; `live::window_title(model, M2's active DM, cx)` | |
| Hotkeys | window key events → `hotkeys.update(|h, _| h.key(code, down))`, blur → `window_blurred()`; `actions::hotkeys::inject_cli(arg)` from the single-instance glue (`--hotkey=<action>[:down\|:up]`) | Codes are `KeyboardEvent.code` names: `live::hotkeys::code_from_gpui_key` maps GPUI's `Keystroke::key` **assuming a US layout** (no physical code at the pin). |
| Frame sinks | the video views: `actions::voice::show_stream(uid)` on mount, `hide_stream(uid)` on unmount | |

### M3.3 Overlay requests

- **The E2EE passphrase dialog** is `model.live.e2ee.passphrase` (not `Overlays::active_modal`): Settings → Privacy
  opens it over the Settings modal. Opened by the first "locked" report of a sign-in, by Settings, and by a voice
  join without keys ("no keys → no voice": `actions::voice::voice_keys_ready`). The foundation's
  `overlays::modal::E2EE_*` ids are not used.
- The change-password dialog: `model.live.devices.password`.
- The "Review devices" toast action: `nav.set_settings_tab("devices")` + `overlays.open_modal(modal::SETTINGS)`.
- The incoming-call card: `calls.status == Incoming` / `calls.incoming_ended` (the view clears it with
  `clear_incoming_ended` after showing it).

### M3.4 Timers (each with its live reason)

The self-move expiry (only while a move is pending), the 45 s ring (only while ringing), our message's
clear-after (only while a deadline is set), the watch set's 600 ms flush (only while a change waits), the
friend list's 10 s reply timeout (only while waiting), the friends error's 5 s (only while shown), the device
hot-plug 400 ms (only while pending), the revocation notice's 8 s, the codec re-probe 15 s after a cached boot
(once), and **auto-away's one timer**: active with a known idle time → `threshold − idle_for`; unknown (the
core's Wayland flag) → one threshold (Away may then show up to one threshold late: core gap 1); speaking holds
it off; Away on a source that can't push resumes → 1 s re-reads (the renderer's rate) for that sole reason.
Nothing of M3 polls at idle: signed in with nothing on screen, no M3 timer exists.

### M3.5 Renderer → here

| Renderer | Here |
|---|---|
| `stores/voiceStore.ts` | `live/voice/{session,rosters,speaking,streams}.rs`; `enterChannel` / `disconnect` / `clearServerPresence` = `actions::voice::{enter_channel,disconnect}`, `events::voice::clear_server_presence` |
| `features/voice/useVoiceEvents.ts` (14 listeners) | `events/voice.rs` |
| `voiceStatsStore.ts` | `live/voice/stats.rs` |
| `voiceActions.ts`, `streaming/joinVoiceChannel.ts`, `applyVoicePrefs.ts`, `streamEnded.ts`, `stage/streamActions.ts`, `userGain.ts`, `streamAudioFilter.ts`, `useStreamThumbnails.ts`, `CaptureSourcePicker.tsx`'s Go Live, `StreamCapture.ts`'s native branch / `retarget` | `actions/voice.rs` |
| `goLive/presets.ts`, `loadSettings.ts`' stream block | `live/voice/go_live.rs` (`StreamSettings::of` / `store` over `Settings`) |
| `codecMap.ts`, `codecToasts.ts`, `canWatchStream.ts`, `decoderProbe.ts`, `encoderProbe.ts` (native path), `codecSettingsStore.ts` | `live/voice/codecs.rs` + `actions::voice::{init_codec_caps,set_codec_toggles}` |
| `audioDevicesStore.ts`, `useAudioDeviceSync.ts`, `streamAudioAppsStore.ts` | `live/voice/devices.rs` |
| `useServerEvents.ts`' `voice_force_notify`, live bitrate | `events::voice::{voice_force_notify,channel_updated}` |
| `memberDrag.ts`, the context menu's voice moderation | `actions::voice::voice_mod` (the drag state is the view's) |
| `streamStats.ts` | the video crate's (decode stats published by the player) |
| `stores/callStore.ts` | `live/calls/mod.rs` |
| `features/call/useCallEvents.ts`, `callActions.ts` | `events/calls.rs`, `actions/calls.rs` |
| `stores/presenceStore.ts`, `usePresenceEvents.ts` | `live/presence/statuses.rs`, `events/presence.rs` |
| `presenceInterest.ts` | `live/presence/interest.rs` |
| `useAutoAway.ts` | `live/presence/auto_away.rs` (+ `actions::presence::CoreIdleSource`) |
| `StatusMenu.tsx`' `chooseStatus`, `CustomStatusModal.tsx`' `saveStatusMessage` | `actions::presence::{choose_status,save_status_message}` |
| `stores/avatarStore.ts` | `live/presence/avatars.rs` |
| `stores/friendsStore.ts`, `useFriendsEvents.ts`, `friendList.ts`, the pages' actions | `live/friends.rs`, `events/friends.rs`, `actions/friends.rs` |
| `stores/e2eeStore.ts`, `useE2eeEvents.ts`, `PassphraseModal.tsx`, `usePeerE2ee.ts` | `live/e2ee.rs`, `events/e2ee.rs`, `actions/e2ee.rs` |
| `stores/hotkeysStore.ts`, `hotkeyRuntime.ts`, `keys.ts` | `live/hotkeys.rs` (the matcher is the core's), `actions/hotkeys.rs`, `events::misc::hotkey_action` |
| `electron/main/hotkeys.ts`' `--hotkey=` | `actions::hotkeys::inject_cli` |
| `stores/devicesStore.ts`, `DevicesTab.tsx`, `ChangePasswordModal.tsx`, `deviceLabels.ts`, the `devices_changed` toast | `live/devices.rs`, `actions/devices.rs`, `events::misc::devices_changed` |
| `stores/transfersStore.ts`, `transfers/{downloads,uploads,format}.ts`, `TransfersButton.tsx`' derived values, `window.ts`' quit guard | `live/transfers.rs`, `actions/transfers.rs` |
| `utils/sounds.ts` | `live::{Sound,Cue,Cues}` (triggers only) |
| `hooks/useWindowTitle.ts` | `live::window_title` |
| `MembershipRevokedToast.tsx` (8 s, headline) | `live::{Notices,revocation_headline}` |

### M3.6 Where this differs from the renderer

- **Mirrored although it looks wrong**: a failed voice join is only logged (the optimistic switch is undone, no
  toast); the sidebar switches to the voice view even when the E2EE gate refused the join; friend-action send
  errors keep JS's "Error: " prefix; one join / leave sound per user (simultaneous joins overlap); posters are
  kept only while the voice view retains them (one arriving with the view closed is dropped and fetched again on
  the next visit); `channel_updated` for a channel we had no row of retunes the bitrate; the leave hotkey does
  nothing while a call rings; `voice_user_state_changed` patches the stage's row, not the channel roster;
  `useAutoAway` counts only our own speaking flag; a failed avatar fetch is retried only when central names the
  version again; the friends error clears after 5 s (the renderer's views did it; here the store).
- **Changed**: voice and stream rosters are cleared on sign-out (the renderer kept them until the next sign-in's
  snapshots replaced them); one `stop_stream` for the user panel and the dock (the dock's never sent STREAM_STOP
  to a call peer; the announce is a no-op outside a call); the device list lives in the store, not the tab; the
  native encode path is the only one (no WebCodecs fallback, no `isNativeEncodeActive` switch); the hotkey
  status is the core's `hotkeys::status()` rather than a JSON round trip.
- **Central**: expired status messages are cleared for others by central's 30 s sweep (they see it up to 30 s
  late); our own copy clears exactly at the deadline, as in the renderer.

### M3.7 Core and platform gaps

1. `system_idle` is a query: no push of the compositor's `idled` / `resumed`, so auto-away either lags (≤ one
   threshold) or re-reads; a push from `system_idle.rs`'s watcher would make it exact and timer-free.
2. No out-of-voice mic test: the Audio tab's meter outside voice used Web Audio's `getUserMedia`; the core
   meters only inside a session (`voice_input_level`).
3. No device-change notification from the core (only `default_device_watch` for the default device).
4. GPUI's `Keystroke` has no physical key code: the recorder's `KeyboardEvent.code` names assume a US layout.
5. `probe_native_encoders` does not exist on macOS: encode caps are empty there.
6. `caps_refreshed` and `stream_gpu_fallback` have no emitter (as the foundation noted).

### M3.8 Tests

- **Unit** (beside the code, `#[gpui::test]` with the test clock, no network): the self-move queue (switch,
  queue order, expiry, no-op leave), community clears, capabilities, keyed events; speaking / input-level
  notifies; stats ring; streams, watching, frame-sink counting; codec gating, fallback, intersection, toast
  wording; Go Live validation and presets; gains; device hot-plug debounce; presence covered / delta / invisible /
  disconnect, messages, own clear-after; interest ref-counts and 600 ms batching; auto-away's arming (threshold −
  idle, pushing vs polling sources, lock, speech); avatars (6 in flight, newest first, off-screen skipped, 400
  bound, sign-out drops late results); friends retries and transitions; E2EE flows; hotkeys (configure
  collapsing, input mode, focused fallback, combos); devices; transfers (attention, progress, quit guard,
  vanished uploads, speed, event coalescing).
- **Router** (`src/live/tests.rs`, `Model::init` without the core, events through the real pump): rosters under
  join / others joining / confirmation / force-move / force-disconnect / re-authentication / channel removal;
  an unconfirmed join expiring; speaking notifying nothing else; streams and watches; no keys → no voice; the call
  machine against a fake transport and the test clock (45 s "No answer", incoming → answered elsewhere → busy →
  connected → hang-up, DND silencing only the ring, glare); presence routing (friends flip, friends-only DMs
  adopted, invisible, central lost); hotkey routing; the new-device toast; sign-out; the revocation notice; the
  window title.
- **Stack** (`tests/stack_live.rs`, `DECIBELL_STACK=1 … --test stack_live -- --test-threads 1`): statuses reach a
  watcher and not a bystander, invisible looks offline, a message with clear-after goes (central's 30 s sweep);
  dave → erin request, accept, remove (seed state restored); two perf4 devices listed, one signs the other out
  (`SESSION_CLOSED {SIGNED_OUT}` → the notice); perf3 / perf4 E2EE ready and a safety number; perf3 + perf4 in
  Lounge, both rosters, MLS ready with two members, deafen / undeafen propagated, join / leave cues; a call
  ringing on two perf4 devices, answered on one (the other: "Answered on another device"), **connected P2P over
  loopback** (host path, verified), hung up. Every client is muted or deafened: no audio leaves or plays.
  **perf3 and perf4 now have E2EE keys** on the stack (passphrase `decibell-m3-test-passphrase`): fresh profiles
  of them start locked and unlock with it; a call to them must be signed (both sides unlocked).
- Not exercised on one machine: speaking events (nobody sends audio), a stream (no capture in a headless run),
  two machines across NAT (STUN srflx), lock / sleep auto-away, real hot-plug, the focused fallback with real
  key events.

### M3.9 Measured (2026-11-26, this machine; release `decibell-headless`, `--features local-central`)

`tools/perf/measure.sh --pid … --settle 20 --duration 60 --gpu none --no-frames`. The binary is 36.5 MB
stripped (25.0 MB before M3: `decibell-attachments` — hyper, rustls, image decoders — comes in with the
Transfers store; the app links it anyway for M2).

| Scenario | Private (dirty) | CPU (one core) | Wakeups / s | Threads |
|---|---|---|---|---|
| Signed in as perf3, Daily Gaming connected and its roster loaded, presence interest for 100 members, not in voice | **16.2 MB** (3.0) | 0.001 % | **0.23** (the `decibell-rt` keep-alives; main 0.0) | 26 |
| Signed in as perf4, alone in Lounge (muted, MLS ready), idle | **23.5 MB** (6.1) | 2.75 % | **1388** | 36 |

The voice baseline, per thread (voluntary switches / s; all the core's except the main thread):

| Thread | Wakeups / s | Why |
|---|---|---|
| `decibell-voice-recv` | 442 | the voice UDP receive loop: a 1 ms read timeout (`pipeline.rs`), so it wakes whether or not packets come |
| `decibell-audio` | 197 | the engine's paced 5 ms loop (`hrtimer_nanosleep`) |
| `data-loop.0` × 2 | 187.5 each | PipeWire's client data loops (capture and playback through the ALSA `pipewire` plugin): 256 frames at 48 kHz |
| `decibell-video-recv` | 160 | the video receive loop, running in voice although nobody streams |
| `cpal_alsa_in`, `cpal_alsa_out` | 94 each | cpal's ALSA period, 512 frames at 48 kHz |
| `decibell-rt` × 2 | 20.4 + 4.4 | the core's runtime: voice pings and stats, MLS, keep-alives |
| main (the model) | 0.9 | the pump: a ping and a stats event every 2 s |

So the model adds nothing at idle; in voice its share is the 0.9 wakeups / s of two telemetry events. The voice
floor of ~1,390 wakeups / s and 2.75 % of a core is the core's media loops: `voice-recv`'s 1 ms timeout and the
idle `video-recv` are the candidates (blocking receives, or a video receiver only while a stream is watched)
before the client's voice targets are set. Measured under load from other builds (load average ~10); one presence
run was disturbed by memory reclaim and was repeated.
