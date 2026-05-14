# Live-stream indicators in UserPopup — Design

**Date:** 2026-05-14
**Author:** sunkhan (with Claude)
**Status:** Approved (pending implementation plan)

## Problem

When a user is streaming in a voice channel, that fact is only visible to people who are already *connected to that voice channel*. There is no way for the local user to:

1. Discover that a friend/server-member is currently live without first guessing which voice channel they're in.
2. Join the streamer's voice channel and start watching their stream from a single entry point.
3. See a live preview of someone else's stream before committing to join.

We want the user popup card (the existing `UserProfilePopup`, opened from member lists / friends list / DMs / chat usernames) to surface "this user is live" status with a preview thumbnail and a single click that joins their voice channel and starts watching. Discord parity.

## Access scope

A user sees the Live indicator in another user's popup iff they have access to the server the streamer is in. In Decibell, "access" == being a member of the community server. The check is automatic: the community server only broadcasts stream-presence events to its connected sessions, so the local client simply will not have any state about streams on servers it isn't connected to.

## Existing infrastructure (no change needed)

- **Server-side stream-presence broadcast** is already server-wide, not channel-scoped. `SessionManager::broadcast_stream_presence(channel_id)` iterates over `sessions_` (all connected sessions on the community server), not just `voice_channels_[channel_id]`. Every connected member receives `STREAM_PRESENCE_UPDATE { channel_id, active_streams[] }` whenever streams start/stop on any channel.
- **Native relays the full payload** to the renderer with `{ serverId, channelId, streams[] }` (see `events.rs::StreamPresenceUpdatedPayload`).
- **Multi-server connections:** the client connects to all of its joined community servers concurrently, so we receive presence events for every server we're in.
- **CodecBadge component** and `canWatchStream(streamInfo, decodeCaps)` helper are already in use by VoicePanel for the streams-grid view; we reuse them.

## Design

### 1. Renderer data model

Add to `voiceStore`:

```ts
streamsByUser: Map<string, {
  serverId: string;
  channelId: string;
  streamInfo: StreamInfo;
}>
```

One entry per actively-streaming user. Built from the existing `stream_presence_updated` listener in `useVoiceEvents.ts` — the event payload already carries `serverId` and `channelId`; today's listener discards them. The refactored listener:

1. Drops any existing entries whose `(serverId, channelId)` matches the incoming event AND whose `ownerUsername` is no longer in `event.streams`.
2. Adds or updates entries from `event.streams` with the event's `serverId` + `channelId`.

The existing `activeStreams` array (used by `VoicePanel` for the current channel's stream grid) becomes a derived Zustand selector: `useVoiceStore(s => Array.from(s.streamsByUser.values()).filter(e => e.serverId === connectedServerId && e.channelId === connectedChannelId).map(e => e.streamInfo))`. No consumer changes.

**Side benefit:** fixes a latent bug where, today, a stream-presence event from any channel of a connected server overwrites the global `activeStreams`. Not hit in practice yet because it's rare for multiple channels of one server to have simultaneous streams, but the new shape eliminates the foot-gun.

### 2. Server-side: latest-thumbnail cache + on-demand fetch

**Protobuf** (`proto/messages.proto`):

```proto
message FetchStreamThumbnailReq {
  string owner_username = 1;
}
message FetchStreamThumbnailRes {
  string owner_username = 1;
  bytes thumbnail_data = 2;  // empty when no thumbnail cached yet
}
```

Plus two new `Packet::Type` enum values (`FETCH_STREAM_THUMBNAIL_REQ`, `FETCH_STREAM_THUMBNAIL_RES`) and the corresponding `oneof Payload` entries.

**Community server** (`src/community/main.cpp`):

- New `SessionManager` field: `std::unordered_map<std::string, std::vector<uint8_t>> latest_thumbnails_` keyed by streamer username, guarded by the existing `mutex_`.
- On `STREAM_THUMBNAIL_UPDATE` handler (around `main.cpp:543`): in addition to the existing `broadcast_to_voice_channel_tcp(packet, channel_id)`, also write the bytes into `latest_thumbnails_[username]`. The proactive broadcast to channel participants stays; the cache is just an additional copy for popup viewers.
- On stream stop / session disconnect (wherever the streamer is removed from `active_streams_`): erase `latest_thumbnails_[username]`.
- New handler for `FETCH_STREAM_THUMBNAIL_REQ`: lookup `latest_thumbnails_[req.owner_username()]`, reply with bytes (or empty bytes if not cached). No extra access check needed — the requesting session is already authenticated to *this* community server, and the only way for them to know the streamer's username is to have received a stream-presence event from us in the first place.

**Why on-demand instead of server-wide push:** Bandwidth. Thumbnails are ~100KB JPEGs broadcast every few seconds while streaming. Pushing them to every member of every server multiplies bandwidth by N_members. On-demand fetch only pays the cost when a popup actually opens. Trade-off: thumbnails in the popup can be up to one refresh-interval stale, which is fine for a preview.

### 3. Native (Rust addon) bridge

New napi command in `electron-client/native/src/commands/streaming.rs` (or sibling, depending on existing file layout):

```rust
#[napi(object)]
pub struct FetchStreamThumbnailArgs {
    pub server_id: String,
    pub username: String,
}

#[napi(object)]
pub struct FetchStreamThumbnailResult {
    pub username: String,
    /// Empty Buffer when the server has no cached thumbnail yet.
    pub jpeg: napi::bindgen_prelude::Buffer,
}

#[napi]
pub async fn fetch_stream_thumbnail(
    args: FetchStreamThumbnailArgs,
) -> napi::Result<FetchStreamThumbnailResult>
```

Mirrors `fetch_avatar`'s pattern exactly:

1. Lookup the matching community-server client by `server_id`.
2. Build a `FetchStreamThumbnailReq` packet.
3. Stash a oneshot sender on `AppState.pending_thumbnail_fetches: HashMap<String, oneshot::Sender<FetchStreamThumbnailRes>>` (keyed by `owner_username`).
4. Send the packet over the community client's write tx.
5. Await the oneshot with a 5-second timeout.
6. On timeout: remove the slot and return an `Err` (renderer falls back to the placeholder gradient).
7. On receive (in the community client's `route_packets` loop): match `FetchStreamThumbnailRes`, pop the matching oneshot from `pending_thumbnail_fetches`, resolve it.

### 4. UserProfilePopup UI integration

**Lookup:**

```ts
const streamEntry = useVoiceStore((s) => s.streamsByUser.get(username));
```

Renders nothing if `streamEntry === undefined` (user isn't streaming, or local user isn't a member of that user's server — both end up as the same undefined state).

**Placement** — the live section always renders directly after the username + online/offline badge block, *before* any roles row or quick-DM input. Top placement maximises visibility and matches Discord. Spacing: `mx-4 mb-3` so it stays in the popup's content gutter and gets a divider below it where appropriate.

**Live section layout:**

- Header line: `Live in #{channelName}` — channel name resolved via `useChatStore.getState().channelsByServer[streamEntry.serverId]?.find(c => c.id === streamEntry.channelId)?.name ?? "voice"`.
- Thumbnail box: `aspect-video`, full content-width (the gutter inside `mx-4`), `rounded-md`, `overflow-hidden`.
  - **Placeholder state** (fetch in flight, fetch failed, or server has no cached frame yet): pulsing gradient — `stringToGradient(username)` background with a subtle `animate-pulse` opacity oscillation so the box feels alive while the real thumb is loading. No user letter — this is a stream preview, not an avatar.
  - **Loaded state**: `<img src={blobUrl}>` with `object-cover h-full w-full`. Placeholder underneath remains so a momentary blob-revoke during refresh doesn't flash empty.
  - Top-left corner: existing `CodecBadge` showing codec + resolution + fps + enforced indicator.
  - Hover state: dark gradient overlay with `WATCH STREAM` label, mirroring VoicePanel's tile hover.
- Thumbnail refresh: on popup mount for a streaming user, fire `invoke("fetch_stream_thumbnail", { serverId, username })`, on success create a blob URL and render. Re-fire every 3 seconds while the popup remains open. On unmount: clear the interval, revoke any blob URLs.

**Codec gate:**

- Compute `const { canWatch, reason } = canWatchStream(streamEntry.streamInfo, decodeCaps)`.
- If `canWatch === false`: set `opacity-50 cursor-not-allowed` on the thumbnail box, attach `title={reason}`, click is a no-op.

**Click handler (auto-join + auto-watch):**

```ts
const handleJoinAndWatch = async () => {
  if (!canWatch) return;
  const { serverId, channelId } = streamEntry;

  // Optimistic state update first so the channel sidebar reflects the
  // pending-join immediately, same as ServerChannelsSidebar's voice
  // channel click. The join_voice_channel.catch path resets state if
  // the engine fails to start.
  useVoiceStore.getState().setConnectedChannel(serverId, channelId);
  setActiveView("voice");
  closePopup();

  try {
    await invoke("join_voice_channel", {
      serverId,
      channelId,
      voiceBitrateKbps: null,
    });
    // Re-apply persisted device prefs here, mirroring
    // ServerChannelsSidebar.handleVoiceChannelClick. Extract this to a
    // shared `joinVoiceChannel` helper rather than copy-pasting.
    await invoke("watch_stream", { serverId, channelId, targetUsername: username });
    useVoiceStore.getState().addWatching(username);
    useVoiceStore.getState().setFullscreenStream(username);
  } catch (err) {
    console.error("Join + watch failed:", err);
    useVoiceStore.getState().disconnect();
  }
};
```

**Extracted helper.** The current `handleVoiceChannelClick` in `ServerChannelsSidebar.tsx` contains ~40 lines of post-join device-pref re-apply logic. We pull that into a shared `joinVoiceChannel(serverId, channelId)` helper in `features/voice/streaming/joinVoiceChannel.ts` (or similar) so both call-sites use the same path. Sidebar's click handler shrinks to a one-liner that also calls this helper.

### 5. Edge cases

| Case | Behavior |
|------|----------|
| Stream stops while my popup is open | `streamsByUser.get(username)` becomes `undefined` on the next presence event; live section unmounts cleanly. |
| Thumbnail fetch times out / no cached frame yet | Stay on the gradient placeholder. No error UI. Retry on next 3s tick. |
| Already in the streamer's voice channel | `join_voice_channel` server handler is a no-op for re-joins (existing semantics). `watch_stream` + `addWatching` + `setFullscreenStream` still fire. |
| In a different voice channel | Existing `join_voice_channel` handler unjoins the old channel first (existing semantics). Self-stream is torn down if it was active. |
| I'm streaming myself, click someone else's live in their popup | Same path as the "different voice channel" case. My stream stops; I join theirs and watch. |
| Multiple streamers in same channel | The popup only shows the *target user's* stream. Joining the channel makes the others visible via the existing VoicePanel grid. |
| Popup opened from a non-server context (friends list, DM) | Still works — `streamsByUser` is keyed by username only, doesn't depend on the popup's `serverId` prop. The stream's actual serverId is in the entry. |

### 6. What's out of scope

- Live indicator on the user's avatar in member lists / friends list / sidebar (Discord shows a red `LIVE` dot on the avatar itself). Could be added later by reading the same `streamsByUser` map in those components. This spec covers only the popup.
- Stream-end notification toast. The presence event already drives state cleanup; we don't surface a UI notification when a stream you were watching ends.
- Per-server thumbnail bandwidth tuning. Server-wide push remains channel-scoped; on-demand fetch is the only added bandwidth and only when popups open.

### 7. File-level change list (preview for the implementation plan)

- `proto/messages.proto` — 2 new messages + 2 enum values + 2 oneof payload entries
- `src/community/main.cpp` + `session_manager.hpp` — `latest_thumbnails_` field, write on thumbnail update, erase on stream stop, new fetch handler
- `electron-client/native/src/state.rs` — `pending_thumbnail_fetches` HashMap
- `electron-client/native/src/net/community.rs` — `FetchStreamThumbnailRes` arm in `route_packets`
- `electron-client/native/src/commands/streaming.rs` — `fetch_stream_thumbnail` napi command
- `electron-client/src/stores/voiceStore.ts` — `streamsByUser` field, `activeStreams` becomes derived
- `electron-client/src/features/voice/useVoiceEvents.ts` — stream-presence listener refactor
- `electron-client/src/features/voice/streaming/joinVoiceChannel.ts` — new shared helper extracted from `ServerChannelsSidebar`
- `electron-client/src/features/channels/ServerChannelsSidebar.tsx` — use shared helper
- `electron-client/src/features/dm/UserProfilePopup.tsx` — live section + click handler

Rough effort: ~400 LOC new, ~150 LOC modified.
