# Live-stream indicators in UserPopup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a Live indicator + thumbnail in `UserProfilePopup` for users who are currently streaming on a server the local user has access to, with a one-click join-and-watch.

**Architecture:** Server-side already broadcasts stream-presence to all server members and we receive those events for every connected community server. We just stop discarding `(serverId, channelId)` in the renderer listener so we can show streams from channels we're not currently in. Thumbnails are fetched on-demand from a new server-side cache instead of being pushed server-wide.

**Tech Stack:** Protobuf 3 (proto3), C++ (community server) with pqxx + Boost.Asio, Rust napi-rs (native addon), React + Zustand (renderer).

**Spec:** `docs/superpowers/specs/2026-05-14-live-stream-indicators-design.md`

---

## File map

**Modify:**
- `proto/messages.proto` — 2 new messages, 2 new packet types, 2 new oneof payload entries
- `src/community/main.cpp` — `latest_thumbnails_` field, write/erase on stream events, `FETCH_STREAM_THUMBNAIL_REQ` handler
- `electron-client/native/src/state.rs` — `pending_thumbnail_fetches` HashMap
- `electron-client/native/src/net/community.rs` — `FetchStreamThumbnailRes` route_packets arm
- `electron-client/native/src/commands/streaming.rs` — `fetch_stream_thumbnail` napi command
- `electron-client/src/stores/voiceStore.ts` — `streamsByUser` field + actions
- `electron-client/src/features/voice/useVoiceEvents.ts` — refactor `stream_presence_updated` listener
- `electron-client/src/features/channels/ServerChannelsSidebar.tsx` — use shared `joinVoiceChannel` helper
- `electron-client/src/features/dm/UserProfilePopup.tsx` — Live section + click handler

**Create:**
- `electron-client/src/features/voice/streaming/joinVoiceChannel.ts` — extracted shared helper

---

## Task 1: Protobuf additions

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Add the two new packet-type enum values**

In `proto/messages.proto`, inside the `Packet.Type` enum (the avatar block currently ends at `AVATAR_CHANGED = 65;` around line 115), append:

```proto
    // Live-stream popup: on-demand thumbnail fetch (community server).
    FETCH_STREAM_THUMBNAIL_REQ = 66;
    FETCH_STREAM_THUMBNAIL_RES = 67;
```

- [ ] **Step 2: Add the two new oneof payload entries**

Inside `Packet.oneof payload`, after `AvatarChanged avatar_changed = 67;` (around line 201), append:

```proto
    // --- Live-stream popup thumbnails (see docs/superpowers/specs/
    //     2026-05-14-live-stream-indicators-design.md §2) ---
    FetchStreamThumbnailReq fetch_stream_thumbnail_req = 68;
    FetchStreamThumbnailRes fetch_stream_thumbnail_res = 69;
```

- [ ] **Step 3: Add the two new message definitions**

Append at the end of `proto/messages.proto` (after the last message in the file):

```proto
// On-demand fetch of the latest thumbnail for a currently-streaming user.
// Sent by the client when a UserProfilePopup opens for a user who's known
// to be streaming. The community server keeps the most recent thumbnail
// in `latest_thumbnails_` and replies with the JPEG bytes (or empty if
// no frame has arrived yet).
message FetchStreamThumbnailReq {
  string owner_username = 1;
}

message FetchStreamThumbnailRes {
  string owner_username = 1;
  bytes thumbnail_data = 2;  // empty when no thumbnail cached yet
}
```

- [ ] **Step 4: Regenerate the Rust prost bindings**

```bash
cd electron-client/native && cargo build
```

Expected: no errors. `prost-build` runs through `build.rs` and updates the generated module that already powers existing proto types. The build may take 30-60s on a clean target dir.

- [ ] **Step 5: Verify the C++ proto generation succeeds**

The C++ server uses cmake's `protoc` invocation; on Windows it's awkward to build locally. Just verify the file parses by re-reading the diff once. The actual C++ regen happens at server build time on the Linux build host.

- [ ] **Step 6: Commit**

```bash
git add proto/messages.proto
git commit -m "proto: FetchStreamThumbnail request/response for popup live preview"
```

---

## Task 2: Community server — thumbnail cache + fetch handler

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Add the cache field + public accessors on `SessionManager`**

In `src/community/main.cpp`, find the `SessionManager` class. After `active_streams_` in the private state block (around line 165), add:

```cpp
    // Latest thumbnail JPEG per streamer username. Written by
    // update_thumbnail_cache(), served by get_thumbnail(),
    // erased by erase_thumbnail_cache(). Guarded by mutex_.
    std::unordered_map<std::string, std::vector<uint8_t>> latest_thumbnails_;
```

Then in the PUBLIC section of `SessionManager` (around line 44-147), declare three accessor methods. A good spot is right after the existing stream methods (`stop_stream`, `broadcast_stream_presence`):

```cpp
    // Cache the most recent thumbnail bytes for `username` so popup
    // viewers can fetch on demand without waiting for the next push.
    void update_thumbnail_cache(const std::string& username,
                                const std::string& bytes);
    // Drop the cache entry — called when a stream stops or the
    // streamer disconnects.
    void erase_thumbnail_cache(const std::string& username);
    // Copy the cached thumbnail bytes for `username` into `out`.
    // Returns false if no entry is cached.
    bool get_thumbnail(const std::string& username,
                       std::vector<uint8_t>& out);
```

And the definitions, placed near `broadcast_stream_presence`:

```cpp
void SessionManager::update_thumbnail_cache(const std::string& username,
                                            const std::string& bytes) {
    std::lock_guard<std::mutex> lock(mutex_);
    latest_thumbnails_[username].assign(bytes.begin(), bytes.end());
}

void SessionManager::erase_thumbnail_cache(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    latest_thumbnails_.erase(username);
}

bool SessionManager::get_thumbnail(const std::string& username,
                                    std::vector<uint8_t>& out) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = latest_thumbnails_.find(username);
    if (it == latest_thumbnails_.end() || it->second.empty()) return false;
    out = it->second;
    return true;
}
```

- [ ] **Step 2: Cache the bytes on every thumbnail update**

Find the `STREAM_THUMBNAIL_UPDATE` handler (around line 543). Replace the body so the broadcast path is preserved AND the cache is written via the public method:

```cpp
        else if (packet.type() == chatproj::Packet::STREAM_THUMBNAIL_UPDATE) {
            auto* update = packet.mutable_stream_thumbnail_update();
            update->set_owner_username(username_); // Enforce identity
            std::string channel_id = update->channel_id();
            // Stash a copy for on-demand popup fetches before the
            // broadcast — bytes are owned by the protobuf so the
            // helper copies them.
            manager_.update_thumbnail_cache(username_, update->thumbnail_data());
            // Broadcast to all voice channel participants (not just watchers).
            manager_.broadcast_to_voice_channel_tcp(packet, channel_id);
        }
```

- [ ] **Step 3: Erase the cache when a stream stops**

In `SessionManager::stop_stream` (around line 1197), AFTER the existing lock block closes (around line 1214), add:

```cpp
    // Drop any cached thumbnail for this streamer; popup viewers
    // will now get an empty response (and they'll stop polling
    // once the next stream-presence event removes the entry from
    // their streamsByUser map).
    erase_thumbnail_cache(session->get_username());
```

Place it right before the `if (removed)` check.

- [ ] **Step 4: Erase the cache when a session disconnects**

In the session-disconnect path (the method where `sessions_.erase(session)` happens — `SessionManager::remove_session` or similar, around line 1144-1161), after the existing lock block, add:

```cpp
    // Stream may have been active when this user dropped; clear the
    // cache regardless. Idempotent on non-streamers.
    erase_thumbnail_cache(session->get_username());
```

(Use the same pattern as Step 3 — call the public method outside the existing lock block since the method takes its own lock.)

- [ ] **Step 5: Implement the FETCH_STREAM_THUMBNAIL_REQ handler**

After the `STREAM_THUMBNAIL_UPDATE` handler (the one we modified in Step 2), add a new handler block:

```cpp
        // --- FETCH_STREAM_THUMBNAIL_REQ ---
        // Sent by clients when a UserPopup opens for a streaming user.
        // Replies with the latest cached JPEG (or empty bytes if no
        // frame has arrived yet). Authenticated callers only — the
        // session is already authenticated to this community server,
        // and the only way to know the streamer's username is to have
        // received a stream-presence event from us, so no extra ACL
        // check is needed.
        else if (packet.type() == chatproj::Packet::FETCH_STREAM_THUMBNAIL_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_stream_thumbnail_req();
            const std::string& target = req.owner_username();

            chatproj::Packet response;
            response.set_type(chatproj::Packet::FETCH_STREAM_THUMBNAIL_RES);
            auto* res = response.mutable_fetch_stream_thumbnail_res();
            res->set_owner_username(target);
            std::vector<uint8_t> bytes;
            if (manager_.get_thumbnail(target, bytes)) {
                res->set_thumbnail_data(bytes.data(), bytes.size());
            }
            send_packet(response);
        }
```

(`send_packet` is the existing helper that frames and dispatches a single packet on the session's socket — see `MEMBER_LIST_REQ` handler around line 942 for the call shape.)

- [ ] **Step 6: Verify the C++ syntax compiles locally if possible**

The community server only builds cleanly on Linux. If the user has a Linux build host:

```bash
cmake --build build-servers --target community-server
```

On Windows, skip — the Linux build host catches errors on push. (Same pattern we used for the avatar feature in 0.6.3.)

- [ ] **Step 7: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community,streams): on-demand thumbnail cache + fetch handler"
```

---

## Task 3: Native — pending fetch slot on AppState

**Files:**
- Modify: `electron-client/native/src/state.rs`

- [ ] **Step 1: Add the HashMap field**

Find the `AppState` struct definition. Near the existing `pending_avatar_fetches` (the avatar feature added it; mirror its position), add:

```rust
    /// Outstanding `fetch_stream_thumbnail` napi calls awaiting their
    /// FetchStreamThumbnailRes from the community server. Keyed by
    /// owner_username. Last-request-wins per username — a previous
    /// in-flight fetch for the same user gets its sender replaced;
    /// the earlier .await times out.
    pub pending_thumbnail_fetches:
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<crate::net::proto::FetchStreamThumbnailRes>>,
```

Then in `AppState::new()` (or wherever the struct is constructed), initialize:

```rust
            pending_thumbnail_fetches: std::collections::HashMap::new(),
```

- [ ] **Step 2: Verify the addon still builds**

```bash
cd electron-client/native && cargo check
```

Expected: success (the field is unused but compiles).

- [ ] **Step 3: Commit**

```bash
git add electron-client/native/src/state.rs
git commit -m "feat(native): pending_thumbnail_fetches state slot"
```

---

## Task 4: Native — route_packets arm for FetchStreamThumbnailRes

**Files:**
- Modify: `electron-client/native/src/net/community.rs`

- [ ] **Step 1: Add the response arm**

Find the existing `route_packets` match (look for `Some(packet::Payload::StreamPresenceUpdate(...))` arm we examined earlier — around lines 650-670). After the closing `}` of an existing arm in the same match, add a new arm:

```rust
                Some(packet::Payload::FetchStreamThumbnailRes(res)) => {
                    // Resolve the matching oneshot waiter. If no waiter is
                    // registered (response arrived after timeout), drop
                    // silently — the caller already returned an error.
                    let mut s = state_arc.lock().await;
                    if let Some(tx) = s.pending_thumbnail_fetches.remove(&res.owner_username) {
                        let _ = tx.send(res);
                    }
                }
```

(Note: `state_arc` is in scope inside `route_packets` already, per the avatar arm pattern. If the variable name differs, match what the existing arms use.)

- [ ] **Step 2: Verify the addon still builds**

```bash
cd electron-client/native && cargo check
```

Expected: success.

- [ ] **Step 3: Commit**

```bash
git add electron-client/native/src/net/community.rs
git commit -m "feat(native,streams): route FetchStreamThumbnailRes to pending waiter"
```

---

## Task 5: Native — fetch_stream_thumbnail napi command

**Files:**
- Modify: `electron-client/native/src/commands/streaming.rs`

- [ ] **Step 1: Add the args + result structs**

At the top of `commands/streaming.rs` (or near existing `WatchStreamArgs`), add:

```rust
#[napi(object)]
pub struct FetchStreamThumbnailArgs {
    pub server_id: String,
    pub username: String,
}

#[napi(object)]
pub struct FetchStreamThumbnailResult {
    pub username: String,
    /// Empty Buffer when the server has no cached thumbnail yet
    /// (stream just started, or fetch arrived between frames).
    pub jpeg: napi::bindgen_prelude::Buffer,
}
```

- [ ] **Step 2: Add the napi function**

After the `watch_stream` napi function (or wherever existing commands like it live), add:

```rust
/// On-demand fetch of the latest thumbnail for a streaming user.
/// Called by the renderer when UserProfilePopup opens for a user
/// known to be streaming. Returns empty bytes when no frame is
/// cached yet — caller renders the gradient placeholder.
///
/// Same shape as `fetch_avatar` in commands/auth.rs: single-slot
/// oneshot per username, 5-second timeout, slot cleanup on timeout.
#[napi]
pub async fn fetch_stream_thumbnail(
    args: FetchStreamThumbnailArgs,
) -> napi::Result<FetchStreamThumbnailResult> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, FetchStreamThumbnailReq};
    use tokio::sync::oneshot;

    let FetchStreamThumbnailArgs { server_id, username } = args;
    let state_arc = crate::state::shared();

    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let client = s.communities.get(&server_id).ok_or_else(|| {
            napi::Error::from_reason(format!("Not connected to community server {}", server_id))
        })?;
        let tx = client.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let jwt = client.jwt().to_string();
        let pkt = build_packet(
            packet::Type::FetchStreamThumbnailReq,
            packet::Payload::FetchStreamThumbnailReq(FetchStreamThumbnailReq {
                owner_username: username.clone(),
            }),
            Some(&jwt),
        );
        let (otx, orx) = oneshot::channel();
        // Last-request-wins per username — supersedes any earlier
        // in-flight fetch (its .await will time out, harmless).
        s.pending_thumbnail_fetches.insert(username.clone(), otx);
        (tx, pkt, orx)
    };

    if tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
        .is_err()
    {
        state_arc.lock().await.pending_thumbnail_fetches.remove(&username);
        return Err(napi::Error::from_reason("Failed to send thumbnail fetch"));
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => Ok(FetchStreamThumbnailResult {
            username: resp.owner_username,
            jpeg: resp.thumbnail_data.into(),
        }),
        Ok(Err(_)) => Err(napi::Error::from_reason(
            "Community connection closed before thumbnail response",
        )),
        Err(_) => {
            state_arc
                .lock()
                .await
                .pending_thumbnail_fetches
                .remove(&username);
            Err(napi::Error::from_reason("Thumbnail fetch timed out"))
        }
    }
}
```

Note: `client.connection_write_tx()` and `client.jwt()` are the existing helpers on `CommunityClient` used by other community-side commands (e.g., `watch_stream`). If their names differ, use whatever the surrounding code uses — the avatar implementation in `commands/auth.rs` is a recent precedent for the central-server version of this same pattern.

- [ ] **Step 3: Rebuild the native addon**

```bash
cd electron-client/native && npm run build
```

Expected: success. The build copies the new `index.win32-x64-msvc.node` and regenerates `index.d.ts` / `index.js` with the new exports.

- [ ] **Step 4: Verify the renderer can see the new export**

```bash
cd electron-client && npx tsc --noEmit
```

Expected: success. The new `fetchStreamThumbnail` function should be picked up via the regenerated `native/index.d.ts`.

- [ ] **Step 5: Commit**

```bash
git add electron-client/native/src/commands/streaming.rs electron-client/native/index.d.ts electron-client/native/index.js
git commit -m "feat(native,streams): fetch_stream_thumbnail napi command"
```

---

## Task 6: Renderer voiceStore — streamsByUser field

**Files:**
- Modify: `electron-client/src/stores/voiceStore.ts`

- [ ] **Step 1: Add the type and field**

At the top of the store file (with other type imports), find the existing `StreamInfo` type. Add a new type and add the field to the store interface:

```ts
/// Per-username view of all active streams across every connected
/// community server. Used by UserPopup to look up "is this user live?"
/// in O(1) without needing to scan all of activeStreams.
export interface StreamLocation {
  serverId: string;
  channelId: string;
  streamInfo: StreamInfo;
}
```

Then in the store state interface, add:

```ts
  streamsByUser: Map<string, StreamLocation>;
  setStreamsByUser: (m: Map<string, StreamLocation>) => void;
```

- [ ] **Step 2: Wire the state default + action**

In the `create<...>()` factory, add:

```ts
  streamsByUser: new Map(),
  setStreamsByUser: (m) => set({ streamsByUser: m }),
```

(Mirror the placement of `activeStreams` and `setActiveStreams` — they should be adjacent.)

- [ ] **Step 3: Clear the new field on disconnect**

Find the existing `disconnect:` action that already clears voice state (it resets `participants`, `activeStreams`, `streamThumbnails`, etc.). Add to the same `set({...})` call:

```ts
      streamsByUser: new Map(),
```

- [ ] **Step 4: Verify typecheck**

```bash
cd electron-client && npx tsc --noEmit
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add electron-client/src/stores/voiceStore.ts
git commit -m "feat(voice): streamsByUser map for cross-channel stream lookup"
```

---

## Task 7: Renderer — refactor stream_presence_updated listener

**Files:**
- Modify: `electron-client/src/features/voice/useVoiceEvents.ts`

- [ ] **Step 1: Update the listener type to declare serverId + channelId**

Find the existing `listen<{...}>("stream_presence_updated", ...)` block (around line 195). Update the generic type to include the fields native is already sending:

```ts
      listen<{
        serverId: string;
        channelId: string;
        streams: {
          streamId: string;
          ownerUsername: string;
          hasAudio: boolean;
          resolutionWidth: number;
          resolutionHeight: number;
          fps: number;
          currentCodec?: number;
          enforcedCodec?: number;
        }[];
      }>("stream_presence_updated", (event) => {
```

- [ ] **Step 2: Drive streamsByUser from the event, then derive activeStreams**

Replace the body of the listener (the `(event) => { ... }` callback) with:

```ts
      }>("stream_presence_updated", (event) => {
        const username = useAuthStore.getState().username;
        const { serverId, channelId } = event.payload;

        const mapped: StreamInfo[] = event.payload.streams.map((s) => ({
          streamId: s.streamId,
          ownerUsername: s.ownerUsername,
          hasAudio: s.hasAudio,
          resolutionWidth: s.resolutionWidth || 0,
          resolutionHeight: s.resolutionHeight || 0,
          fps: s.fps || 0,
          currentCodec: (s.currentCodec ?? VideoCodec.UNKNOWN) as VideoCodec,
          enforcedCodec: (s.enforcedCodec ?? VideoCodec.UNKNOWN) as VideoCodec,
        }));

        // ---- streamsByUser update (source of truth) ----
        // Drop entries for (serverId, channelId) whose owner is no
        // longer in the event; add/update from the event.
        const cur = useVoiceStore.getState().streamsByUser;
        const next = new Map(cur);
        for (const [user, loc] of cur) {
          if (
            loc.serverId === serverId &&
            loc.channelId === channelId &&
            !mapped.some((s) => s.ownerUsername === user)
          ) {
            next.delete(user);
          }
        }
        for (const s of mapped) {
          next.set(s.ownerUsername, {
            serverId,
            channelId,
            streamInfo: s,
          });
        }
        useVoiceStore.getState().setStreamsByUser(next);

        // ---- sound effects (only on the LOCAL user's current channel) ----
        const connSrv = useVoiceStore.getState().connectedServerId;
        const connCh = useVoiceStore.getState().connectedChannelId;
        if (serverId === connSrv && channelId === connCh) {
          if (prevStreamOwners) {
            const current = new Set(mapped.map((s) => s.ownerUsername));
            for (const owner of current) {
              if (!prevStreamOwners.has(owner) && owner !== username)
                playSound("stream_start");
            }
            for (const owner of prevStreamOwners) {
              if (!current.has(owner) && owner !== username)
                playSound("stream_stop");
            }
          }
          prevStreamOwners = new Set(mapped.map((s) => s.ownerUsername));

          // activeStreams is derived from streamsByUser, filtered to
          // the local user's current channel. We assign directly here
          // (rather than computing a Zustand-derived selector) so all
          // existing consumers continue to work unchanged.
          useVoiceStore.getState().setActiveStreams(mapped);

          // Stop-watching cleanup remains scoped to the current channel
          // — we only care about streams the user was watching here.
          const { watchingStreams, fullscreenStream } = useVoiceStore.getState();
          for (const w of watchingStreams) {
            if (!mapped.some((s) => s.ownerUsername === w)) {
              useVoiceStore.getState().removeWatching(w);
            }
          }
          if (
            fullscreenStream &&
            !mapped.some((s) => s.ownerUsername === fullscreenStream)
          ) {
            useVoiceStore.getState().setFullscreenStream(null);
            if (useVoiceStore.getState().isStreamFullscreen) {
              getCurrentWindow().setFullscreen(false).catch(() => {});
            }
          }
        }
      }),
```

Key changes vs. the old listener:
- Reads `serverId` + `channelId` off the event (was being ignored)
- Updates `streamsByUser` unconditionally (every server, every channel)
- Updates `activeStreams` + plays sounds + does watching cleanup ONLY when the event is for the user's CURRENT channel (existing behavior preserved)

- [ ] **Step 3: Verify typecheck**

```bash
cd electron-client && npx tsc --noEmit
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add electron-client/src/features/voice/useVoiceEvents.ts
git commit -m "feat(voice): drive streamsByUser from cross-channel presence events"
```

---

## Task 8: Extract joinVoiceChannel helper

**Files:**
- Create: `electron-client/src/features/voice/streaming/joinVoiceChannel.ts`
- Modify: `electron-client/src/features/channels/ServerChannelsSidebar.tsx`

- [ ] **Step 1: Create the helper**

Create `electron-client/src/features/voice/streaming/joinVoiceChannel.ts`:

```ts
import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { useUiStore } from "../../../stores/uiStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { playSound } from "../../../utils/sounds";

/// Shared voice-channel join flow used by ServerChannelsSidebar (when
/// the user clicks a voice channel row) and UserProfilePopup (when the
/// user clicks a live stream thumbnail).
///
/// Responsibilities:
///   1. Optimistically update voiceStore.connectedChannel so the
///      sidebar shows the pending-join immediately.
///   2. Call `join_voice_channel` via napi.
///   3. Re-apply persisted audio device + DSP preferences against the
///      newly-spawned pipeline.
///
/// On engine failure: resets voiceStore state and re-throws so the
/// caller can surface the error.
export async function joinVoiceChannel(
  serverId: string,
  channelId: string,
): Promise<void> {
  // Optimistic update — sidebar shows the pending-join immediately.
  playSound("connect");
  useVoiceStore.getState().setConnectedChannel(serverId, channelId);

  const channel = useChatStore
    .getState()
    .channelsByServer[serverId]?.find((ch) => ch.id === channelId);

  try {
    await invoke("join_voice_channel", {
      serverId,
      channelId,
      voiceBitrateKbps: channel?.voiceBitrateKbps ?? null,
    });
  } catch (err) {
    useVoiceStore.getState().disconnect();
    throw err;
  }

  // Re-apply persisted audio preferences against the fresh pipeline.
  // Saved threshold + AEC/NS/AGC + device picks all live in uiStore.
  const {
    inputDevice,
    outputDevice,
    separateStreamOutput,
    streamOutputDevice,
    voiceThresholdDb,
    aecEnabled,
    noiseSuppressionLevel,
    agcEnabled,
  } = useUiStore.getState();
  invoke("set_voice_threshold", {
    thresholdDb: voiceThresholdDb <= -60 ? -96 : voiceThresholdDb,
  }).catch(console.error);
  if (inputDevice) {
    invoke("set_input_device", { name: inputDevice }).catch(console.error);
  }
  if (outputDevice) {
    invoke("set_output_device", { name: outputDevice }).catch(console.error);
  }
  if (separateStreamOutput) {
    invoke("set_separate_stream_output", {
      enabled: true,
      device: streamOutputDevice,
    }).catch(console.error);
  }
  if (aecEnabled) {
    invoke("set_aec_enabled", { enabled: true }).catch(console.error);
  }
  if (noiseSuppressionLevel > 0) {
    invoke("set_noise_suppression_level", { level: noiseSuppressionLevel })
      .catch(console.error);
  }
  if (agcEnabled) {
    invoke("set_agc_enabled", { enabled: true }).catch(console.error);
  }
}
```

- [ ] **Step 2: Refactor ServerChannelsSidebar to use the helper**

In `electron-client/src/features/channels/ServerChannelsSidebar.tsx`, find `handleVoiceChannelClick` (around line 86). Replace its body so it delegates to the helper:

```tsx
  const handleVoiceChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    if (channelId === connectedChannelId) {
      setActiveView("voice");
      return;
    }
    joinVoiceChannel(activeServerId, channelId).catch(console.error);
    setActiveView("voice");
  };
```

Add the import at the top of the file:

```tsx
import { joinVoiceChannel } from "../voice/streaming/joinVoiceChannel";
```

Remove now-unused imports if the only places using `playSound`/`useUiStore` were in the old inline body — verify by searching the rest of the file. Most likely `useUiStore` is still imported for other reasons; check before deleting.

- [ ] **Step 3: Verify typecheck**

```bash
cd electron-client && npx tsc --noEmit
```

Expected: success.

- [ ] **Step 4: Smoke test the channel-row join path**

`npm run dev`, join any voice channel via the sidebar. Should work identically to before — same device-prefs re-apply, same sound, same optimistic update.

- [ ] **Step 5: Commit**

```bash
git add electron-client/src/features/voice/streaming/joinVoiceChannel.ts electron-client/src/features/channels/ServerChannelsSidebar.tsx
git commit -m "refactor(voice): extract shared joinVoiceChannel helper"
```

---

## Task 9: UserPopup — Live section + click-to-join-and-watch

**Files:**
- Modify: `electron-client/src/features/dm/UserProfilePopup.tsx`

- [ ] **Step 1: Read streamsByUser + decode caps + chat store for channel name**

Near the existing store reads at the top of `UserProfilePopup`, add:

```tsx
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { canWatchStream } from "../../utils/canWatchStream";
import { CodecBadge } from "../voice/CodecBadge";
import { joinVoiceChannel } from "../voice/streaming/joinVoiceChannel";
import { invoke } from "../../lib/ipc";
import { stringToGradient } from "../../utils/colors";
```

(Some may already be present; keep the file's import list deduplicated.)

Inside the component body, after the existing store reads:

```tsx
  const streamEntry = useVoiceStore((s) =>
    username ? s.streamsByUser.get(username) : undefined,
  );
  const decodeCaps = useCodecSettingsStore((s) => s.decodeCaps);
  const channelName = useChatStore((s) => {
    if (!streamEntry) return null;
    return (
      s.channelsByServer[streamEntry.serverId]?.find(
        (c) => c.id === streamEntry.channelId,
      )?.name ?? "voice"
    );
  });
```

- [ ] **Step 2: Thumbnail fetch state + 3-second refresh interval**

Add inside the component body, after the store reads:

```tsx
  const [thumbnailUrl, setThumbnailUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!streamEntry || !username) {
      setThumbnailUrl(null);
      return;
    }
    let cancelled = false;
    let currentUrl: string | null = null;

    const fetchOnce = async () => {
      try {
        const result = (await invoke("fetch_stream_thumbnail", {
          serverId: streamEntry.serverId,
          username,
        })) as { username: string; jpeg: Uint8Array };
        if (cancelled) return;
        if (result.jpeg && result.jpeg.byteLength > 0) {
          const blob = new Blob([result.jpeg as BlobPart], { type: "image/jpeg" });
          const url = URL.createObjectURL(blob);
          // Revoke the previous URL after swapping in the new one so a
          // momentary <img src=""> doesn't flash empty.
          const prev = currentUrl;
          currentUrl = url;
          setThumbnailUrl(url);
          if (prev) URL.revokeObjectURL(prev);
        }
        // If jpeg is empty, leave the placeholder visible — server has
        // no cached frame yet. Next tick will try again.
      } catch (err) {
        console.warn("[UserPopup] fetch_stream_thumbnail failed:", err);
      }
    };

    fetchOnce();
    const interval = window.setInterval(fetchOnce, 3000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
      if (currentUrl) URL.revokeObjectURL(currentUrl);
    };
  }, [streamEntry, username]);
```

Make sure `useState` and `useEffect` are imported at the top.

- [ ] **Step 3: Click handler — auto-join + auto-watch**

Add inside the component body, after the thumbnail effect:

```tsx
  const handleJoinAndWatch = async () => {
    if (!streamEntry || !username) return;
    const { serverId, channelId } = streamEntry;
    const decodeCheck = canWatchStream(streamEntry.streamInfo, decodeCaps);
    if (!decodeCheck.canWatch) return;

    closePopup();
    setActiveView("voice");

    try {
      // Cheap no-op when we're already in the target channel; otherwise
      // disconnects from the old channel and re-applies device prefs.
      if (
        useVoiceStore.getState().connectedServerId !== serverId ||
        useVoiceStore.getState().connectedChannelId !== channelId
      ) {
        await joinVoiceChannel(serverId, channelId);
      }
      await invoke("watch_stream", {
        serverId,
        channelId,
        targetUsername: username,
      });
      useVoiceStore.getState().addWatching(username);
      useVoiceStore.getState().setFullscreenStream(username);
    } catch (err) {
      console.error("Join + watch failed:", err);
    }
  };
```

- [ ] **Step 4: Render the Live section in JSX**

Find the existing JSX, where the username block ends and the roles row / quick-DM begins. The username block looks like:

```tsx
        <div className="px-4 pb-1 pt-3">
          <div className="font-display text-[16px] font-semibold text-text-primary">
            {username}
          </div>
          <div className="mt-1.5">
            {isOnline ? ( … ) : ( … )}
          </div>
        </div>
```

Right after the closing `</div>` of that block, insert the Live section:

```tsx
        {streamEntry && username && (() => {
          const { canWatch, reason } = canWatchStream(
            streamEntry.streamInfo,
            decodeCaps,
          );
          return (
            <>
              <div className="mx-4 my-3 h-px bg-border-divider" />
              <div className="mx-4 mb-3">
                <div className="mb-2 text-[11px] font-medium text-text-secondary">
                  Live in <span className="text-accent-bright">#{channelName}</span>
                </div>
                <button
                  type="button"
                  onClick={handleJoinAndWatch}
                  disabled={!canWatch}
                  title={canWatch ? "Watch stream" : reason}
                  className={`group relative block aspect-video w-full overflow-hidden rounded-md ${
                    canWatch
                      ? "cursor-pointer"
                      : "cursor-not-allowed opacity-50"
                  }`}
                  style={{
                    background: stringToGradient(username),
                  }}
                >
                  {/* Pulsing-gradient placeholder underneath; the <img>
                      sits on top once the fetch lands. The placeholder
                      stays mounted so a brief blob-revoke between
                      refreshes doesn't flash an empty box. */}
                  <div className="absolute inset-0 animate-pulse" style={{
                    background: stringToGradient(username),
                  }} />
                  {thumbnailUrl && (
                    <img
                      src={thumbnailUrl}
                      alt={`${username}'s stream`}
                      className="absolute inset-0 h-full w-full object-cover"
                      draggable={false}
                    />
                  )}
                  <div className="absolute left-2 top-2">
                    <CodecBadge
                      codec={streamEntry.streamInfo.currentCodec}
                      width={streamEntry.streamInfo.resolutionWidth}
                      height={streamEntry.streamInfo.resolutionHeight}
                      fps={streamEntry.streamInfo.fps}
                      enforced={streamEntry.streamInfo.enforcedCodec !== 0}
                      size="small"
                    />
                  </div>
                  {canWatch && (
                    <div className="absolute inset-0 flex items-center justify-center bg-black/0 transition-colors group-hover:bg-black/40">
                      <span className="text-[12px] font-semibold text-white opacity-0 transition-opacity group-hover:opacity-100">
                        WATCH STREAM
                      </span>
                    </div>
                  )}
                </button>
              </div>
            </>
          );
        })()}
```

Note: `CodecBadge` is in `electron-client/src/features/voice/CodecBadge.tsx`. If the actual path differs, adjust the import.

- [ ] **Step 5: Verify typecheck**

```bash
cd electron-client && npx tsc --noEmit
```

Expected: success.

- [ ] **Step 6: Commit**

```bash
git add electron-client/src/features/dm/UserProfilePopup.tsx
git commit -m "feat(popup): live-stream section with thumbnail + click-to-join-and-watch"
```

---

## Task 10: End-to-end test pass

**Files:** none — manual verification.

- [ ] **Step 1: Server rebuild**

Rebuild the community server on the Linux build host and redeploy. Without this, the FETCH_STREAM_THUMBNAIL_REQ handler doesn't exist and popup thumbnails will time out. Existing stream-presence + thumbnail-broadcast paths keep working regardless.

- [ ] **Step 2: Start dev client + log in with at least two accounts in the same server**

```bash
cd electron-client && npm run dev
```

Log in as user A on one machine/window, user B on another. Both should be members of the same community server.

- [ ] **Step 3: Verify "user not streaming" base case**

Open user B's popup from any context (member list, friends list, DM). Confirm: no live section visible, popup looks exactly like before this feature.

- [ ] **Step 4: Verify "user streaming, same channel" case**

User B joins any voice channel and starts streaming. User A joins the same voice channel. User A opens user B's popup. Confirm:
- Live section appears below the online badge
- "Live in #{channel-name}" header
- Thumbnail loads within ~3 seconds (pulsing gradient briefly, then real frame)
- CodecBadge in top-left shows correct codec/resolution/fps
- Click thumbnail → already in channel, just opens VoicePanel fullscreen with B's stream

- [ ] **Step 5: Verify "user streaming, different channel" case**

User A leaves voice or joins a different channel. User B keeps streaming in channel X. User A opens B's popup. Confirm:
- Live section still appears (server-wide presence)
- Thumbnail may not load (A isn't a voice channel participant yet — but the server cache should still respond from FETCH_STREAM_THUMBNAIL_REQ)
- Click thumbnail → A joins B's voice channel + auto-watches B's stream + VoicePanel opens fullscreen

- [ ] **Step 6: Verify "stream ends while popup open"**

User A has B's popup open with live section visible. User B stops streaming. Confirm:
- Live section disappears within one stream-presence event (~immediately)
- No errors in console; popup remains functional

- [ ] **Step 7: Verify "codec not supported"**

Configure user A to disable HEVC decode in codec settings. User B streams in HEVC. User A opens B's popup. Confirm:
- Live section shows, thumbnail box is opacity-50 and not clickable
- Hover tooltip shows the reason from `canWatchStream`

- [ ] **Step 8: Verify cross-server**

User A and B are members of two community servers (X and Y). User A is currently active on server X. User B streams on server Y. User A opens B's popup from friends list (no server context). Confirm:
- Live section appears with correct serverId/channelId from B's stream
- Click → joins server Y's voice channel, switches active view, watches B

- [ ] **Step 9: Confirm pre-existing behavior still works**

- Click a voice channel row in the sidebar (the existing flow) — should still join via the extracted `joinVoiceChannel` helper, indistinguishable from before
- Click someone's stream tile in VoicePanel grid — should still work
- Self-preview your own stream — should still work
- Hot-reload doesn't unmount/remount VoicePanel decoder

- [ ] **Step 10: No commit — testing only**

If anything is broken, return to the relevant task and fix; otherwise this plan is complete. Version-bump to 0.6.4 and tag is a separate ship-time step (per [[feedback_version_bump]]), not part of this plan.

---

## Self-review checklist (already run)

**Spec coverage:**
- ✅ §1 (streamsByUser data model) → Task 6 + 7
- ✅ §2 (server cache + on-demand fetch) → Task 1 + 2
- ✅ §3 (native bridge) → Task 3 + 4 + 5
- ✅ §4 (UserPopup UI + click flow) → Task 9
- ✅ §4 (shared joinVoiceChannel helper) → Task 8
- ✅ §5 (edge cases) → covered in Task 10 test matrix

**Placeholder scan:** no TBD/TODO; all code blocks complete; helper signatures consistent across tasks (joinVoiceChannel signature in Task 8 matches usage in Task 9).

**Type consistency:** `StreamLocation` declared in Task 6, used in Task 7 listener and Task 9 popup. `FetchStreamThumbnailArgs.serverId` matches the Task 9 call. `canWatchStream` return shape `{ canWatch, reason }` matches Task 9 usage.
