# Custom Profile Pictures — Design Spec

**Date:** 2026-05-12
**Status:** Approved for implementation planning
**Scope:** Centrally-stored per-user profile pictures, end-to-end. Settings → Account tab upload + crop, central server DB storage, on-the-wire distribution to all clients, in-renderer cache + invalidation. Replaces the letter-in-coloured-box fallback at every existing avatar site.

---

## 1. Goal

Users upload a profile picture from the Settings → Account tab, crop it to a square, and have it displayed everywhere their username appears across the app. Storage and distribution live in the existing central server (the single C++ + Postgres binary). Avatars propagate to peers in real time via a push event, with cache invalidation keyed on a content-hash version so a re-upload reaches everyone within one RTT without staleness windows or polling.

Aligns with the project's "low-resource, decentralized identity" north star — avatars sit on central (where username/email/JWT already live), community servers stay unaware.

---

## 2. Scope

**In scope:**
- New `avatar BYTEA` + `avatar_version VARCHAR(64)` columns on the central `users` table
- Five new protobuf packet types: `UPDATE_AVATAR_REQ` / `UPDATE_AVATAR_RES` / `FETCH_AVATAR_REQ` / `FETCH_AVATAR_RES` / `AVATAR_CHANGED`
- Two existing-message shape changes: `FriendInfo` gains `avatar_version`; `PresenceUpdate` rewrites `online_users` from `repeated string` to `repeated UserPresence { username, avatar_version }`
- Account-tab UI for upload + remove
- Square-crop modal with pan + zoom (no external dep — inline ~200 LOC)
- Client cache (Zustand) with sha256-hex-keyed invalidation, lazy fetch, blob-URL backed `<img>` rendering
- Shared `<UserAvatar>` component replacing inline letter-avatar markup at every call site
- Server-side validation: JPEG magic bytes + 200 KB hard size limit
- Idempotent schema migration in `initializeDatabase`

**Out of scope (deferred to follow-ups if/when needed):**
- Animated avatars (GIF / WebP / AVIF storage)
- Avatar history / "undo"
- Server-side re-encoding / EXIF stripping (canvas re-encode at the cropper handles both)
- CDN / pre-signed URLs / object store
- NSFW or content moderation
- Cache persistence across app restarts (in-memory only; fresh fetch on launch)
- Friend-only visibility gating (avatar visibility matches username visibility today — authenticated callers can fetch anyone's)
- Backward-compat for older clients (per user decision, all clients track latest — wire-breaking `PresenceUpdate` change is acceptable)

---

## 3. Architecture

```
                              ┌──────────────────────────────┐
                              │      Central Server          │
                              │      (C++ + Postgres)        │
                              │                              │
   Client A                   │   users.avatar (BYTEA)       │              Client B
   ────────                   │   users.avatar_version       │              ────────
                              │     (sha256-hex)             │
   upload via cropper         │                              │              every render of
   ──────────────────────┐    │   ┌──────────────────────┐   │              another user reads
                         ▼    │   │  UPDATE_AVATAR_REQ   │   │              avatarStore →
                              ├──►│  validate magic+size │   │              UserAvatar comp
                              │   │  UPDATE users SET …  │   │
                              │   │  broadcast            │   │              cache miss/dirty:
                              │   │    AvatarChanged ──┐ │   │              FetchAvatarReq ───►
                              │   └──────────────────┐  │ │   │                                  │
                              │                      │  │ │   │              ◄──── bytes + ver ─┘
                              │                      │  │ │   │              avatarStore.update
                              │                      │  └─┼─►  │              UserAvatar re-renders
                              │                      └────┴───►│
                              └──────────────────────────────┘

   Distribution of avatar_version (so clients know what's current without polling):
     ┌─ via FriendInfo.avatar_version       (on every FRIEND_LIST_RES)
     ├─ via UserPresence.avatar_version    (on every PRESENCE_UPDATE)
     └─ via AvatarChanged                  (server-pushed on each upload, fans out
                                            to every active session)
```

Two thread/state concerns:

- **Server fan-out** of `AvatarChanged` is synchronous over each session's existing write channel (`Connection::send`). The handler walks the active-sessions map under the standard mutex, builds the packet once, iterates and sends; no separate task queue or batching.
- **Client fetch deduplication** lives in `avatarStore`: a username with `status='loading'` won't re-trigger a fetch when re-rendered. Promises are not exposed — `<UserAvatar>` is purely reactive, reading from store, falling back to letter avatar in any non-loaded state.

---

## 4. Protobuf changes

New messages (`proto/messages.proto`):

```protobuf
message UpdateAvatarReq {
  bytes data = 1;       // empty = remove; otherwise JPEG bytes, max 200 KB
}

message UpdateAvatarRes {
  bool success = 1;
  string message = 2;
  string version = 3;   // sha256-hex; '' on removal
}

message FetchAvatarReq {
  string username = 1;
}

message FetchAvatarRes {
  string username = 1;
  string version = 2;   // '' when user has no avatar
  bytes data = 3;       // empty when version == ''
}

message AvatarChanged {
  string username = 1;
  string version = 2;   // '' on removal
}
```

Modified messages:

```protobuf
message FriendInfo {
  string username = 1;
  enum Status { ONLINE=0; OFFLINE=1; PENDING_INCOMING=2; PENDING_OUTGOING=3; BLOCKED=4; }
  Status status = 2;
  string avatar_version = 3;       // NEW
}

message UserPresence {            // NEW message
  string username = 1;
  string avatar_version = 2;
}

message PresenceUpdate {
  repeated UserPresence users = 1;  // CHANGED from `repeated string online_users`
}
```

New `Packet.Type` enum entries: `UPDATE_AVATAR_REQ`, `UPDATE_AVATAR_RES`, `FETCH_AVATAR_REQ`, `FETCH_AVATAR_RES`, `AVATAR_CHANGED` (5 new values appended after the last existing variant).

---

## 5. Central server (C++)

### Schema migration

In the existing `initializeDatabase` path next to the `friends` / `community_invites` setup:

```cpp
txn.exec(
    "ALTER TABLE users ADD COLUMN IF NOT EXISTS avatar BYTEA"
);
txn.exec(
    "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
    "avatar_version VARCHAR(64) NOT NULL DEFAULT ''"
);
```

`IF NOT EXISTS` makes the migration idempotent. Existing rows get `NULL` avatar + `''` avatar_version; the client treats both as "no avatar" and falls back to the letter.

### Handlers

**`UPDATE_AVATAR_REQ`** (in `main.cpp`):
1. Resolve sender username from JWT (existing pattern).
2. If `data.size() > 0`: validate `data[0] == 0xFF && data[1] == 0xD8` (JPEG SOI). Reject with `success=false, message="Not a JPEG"` otherwise.
3. If `data.size() > 200 * 1024`: reject with `success=false, message="Avatar too large"`.
4. Compute `version = sha256_hex(data)` (`''` if data is empty). A small `sha256_hex` helper goes alongside `auth_utils.hpp` — uses the OpenSSL the server already links against for TLS.
5. Single transaction: `UPDATE users SET avatar = $1, avatar_version = $2 WHERE username = $3`. Use a parameterized bytea bind for `data`.
6. Respond `UpdateAvatarRes { success=true, version }`.
7. Build `AvatarChanged { username, version }` once, iterate active sessions, send to each.

**`FETCH_AVATAR_REQ`**:
1. Resolve sender from JWT (auth check).
2. `SELECT avatar, avatar_version FROM users WHERE username = $1`. No friend-gating — authenticated callers can fetch anyone's.
3. Respond `FetchAvatarRes { username, version, data }`. Empty data + empty version when the user has no avatar set (or doesn't exist).

**`FRIEND_LIST_RES` / `PRESENCE_UPDATE` builders**: queries now `LEFT JOIN users` to pull `avatar_version` per row. Trivial extension of the existing presence/friend snapshot logic.

### Validation summary

| Check | Limit | Failure mode |
|-------|-------|--------------|
| JPEG magic bytes (`0xFF 0xD8`) | first 2 bytes | reject with `"Not a JPEG"` |
| Max bytes | 200 KB | reject with `"Avatar too large"` |
| Sender auth | valid JWT | session already gated by auth middleware |
| No re-encoding | — | we trust the client's canvas-encoded output |
| No EXIF stripping | — | canvas re-encode at client drops it |

---

## 6. Native (Rust) client

`electron-client/native/src/`:

- **`net/proto.rs`** — auto-regenerated from the updated `messages.proto` via the existing `prost-build` step in `build.rs`. No manual edits.

- **`net/central.rs::route_packets`** — three new arms:
  - `Type::AvatarChanged` → `events::send("avatar_changed", { username, version })` via the existing JSON bus
  - `Type::UpdateAvatarRes` → resolve a pending oneshot (mirrors `pending_invite_resolves` in `AppState`)
  - `Type::FetchAvatarRes` → resolve a pending oneshot keyed by `username`

- **`state.rs::AppState`** gains:
  - `pending_avatar_updates: Option<oneshot::Sender<UpdateAvatarResponse>>` (one at a time — one upload in flight per session is the realistic UX)
  - `pending_avatar_fetches: HashMap<String, oneshot::Sender<FetchAvatarResponse>>` (keyed by username, supports overlapping fetches for different users)

- **`commands/auth.rs`** — two new napi commands (live alongside login/logout because avatar is a central-account concern, not a streaming/community concern):
  - `upload_avatar(jpeg: Buffer) -> { success: bool, message: string, version: string }`
    - 5-second timeout via `tokio::time::timeout`
    - Builds + sends `UPDATE_AVATAR_REQ` over the central connection
    - Stashes the oneshot Receiver, returns when the router routes the response
  - `fetch_avatar(username: String) -> { version: string, data: Buffer }` — same pattern; empty `data` Buffer (length 0) when the user has no avatar

- **Existing `FRIEND_LIST_RES` / `PRESENCE_UPDATE` deserialization** updates `state.friends_avatar_versions` / fires the `friend_list_received` event payload with `avatar_version` per entry. The renderer-side store consumes it (see §7).

---

## 7. Renderer

### New: `src/stores/avatarStore.ts`

Zustand store with this shape:

```ts
type AvatarEntry = {
  version: string;       // sha256-hex; '' for "no avatar"
  data: Uint8Array | null;
  blobUrl: string | null;
  status: "idle" | "loading" | "loaded" | "missing" | "error";
};

interface AvatarStoreState {
  entries: Map<string, AvatarEntry>;
  setVersion(username: string, version: string): void;
  fetchIfNeeded(username: string): void;
  invalidate(username: string): void;
  clearAll(): void;
}
```

Semantics:

- **`setVersion(username, version)`**: if no entry exists or `entry.version !== version`, replace the entry with `{ version, data: null, blobUrl: null, status: 'idle' }`. Revoke the previous `blobUrl` first. This is the cache-invalidation primitive.
- **`fetchIfNeeded(username)`**: when entry is `idle` (just-invalidated or never-seen), set `status='loading'`, kick `invoke('fetch_avatar', { username })`, store result. If `version === ''`: `status='missing'`. Otherwise: build `blobUrl` via `URL.createObjectURL(new Blob([data], { type: 'image/jpeg' }))`, set `status='loaded'`.
- **`invalidate(username)`**: revoke `blobUrl`, set `status='idle'`. Next render triggers re-fetch.
- **`clearAll()`**: revoke every `blobUrl`, clear the map. Called on logout.

### New: `src/components/UserAvatar.tsx`

```tsx
<UserAvatar username="alice" size={32} className="..." />
```

- Subscribes to `avatarStore` (selector by `username` for minimal re-renders)
- On mount: calls `fetchIfNeeded(username)`
- Render branches:
  - `status === 'loaded'` → `<img src={blobUrl} className={rounded-md + size}>`
  - `status === 'loading'` → letter fallback (don't show a spinner — letter is good-enough placeholder; transition is instant once loaded)
  - `status === 'missing' | 'idle' | 'error'` → letter fallback

Letter fallback is the existing markup, extracted into a small `<LetterAvatar>` sub-component used both standalone and as the loading/missing state of `<UserAvatar>`.

### New: `src/features/settings/AvatarCropperModal.tsx`

- Loads the picked file into an `HTMLImageElement` (via `URL.createObjectURL` on the File).
- Renders a square viewport (e.g. 320×320 px display) with a draggable image underneath. Mouse wheel / pinch zoom scales the image; click-drag pans it.
- "Save" → derives the crop rectangle, draws onto a 256×256 `OffscreenCanvas`, calls `canvas.convertToBlob({ type: 'image/jpeg', quality: 0.85 })`, ships the bytes to `invoke('upload_avatar', { jpeg: new Uint8Array(buf) })`.
- "Cancel" → revoke the blob URL on the source image, close the modal.
- Inline implementation (no external dep). Roughly:
  - `useState({ x, y, scale })` for image positioning
  - `useRef` for the loaded `HTMLImageElement` + the preview canvas
  - `useEffect` repaints the preview canvas whenever position state changes
  - Mouse event handlers wired to `onWheel` / `onMouseDown` / `onMouseMove` / `onMouseUp`
  - On Save: compute source rect from current state, draw onto a separate 256×256 canvas

### Modify: `src/features/settings/tabs/AccountTab.tsx`

Add at the top of the existing tab:

```tsx
<div className="flex items-center gap-4">
  <UserAvatar username={currentUser} size={96} />
  <div>
    <button onClick={openFilePicker}>Change picture</button>
    {hasAvatar && <button onClick={removeAvatar}>Remove picture</button>}
  </div>
</div>
<input
  ref={fileInputRef}
  type="file"
  accept="image/jpeg,image/png,image/webp"
  hidden
  onChange={onFilePicked}
/>
{cropperOpen && <AvatarCropperModal file={pickedFile} onSave={...} onCancel={...} />}
```

`removeAvatar` calls `invoke('upload_avatar', { jpeg: new Uint8Array(0) })`.

### Modify: existing avatar call sites

Replace inline `<div className="...">{username.charAt(0).toUpperCase()}</div>` with `<UserAvatar username={...} size={...} />` at:

| File | Size (px) |
|------|----|
| `src/features/channels/UserPanel.tsx` | 32 |
| `src/features/channels/ConversationSidebar.tsx` | 32 |
| `src/features/friends/MembersList.tsx` | 32 |
| `src/features/dm/UserProfilePopup.tsx` | 64 |
| `src/features/servers/MembersAdminPanel.tsx` | 32 |
| `src/features/voice/VoicePanel.tsx` | 40 |
| Any chat-message sender avatar (e.g. `MessageDelegate`) | 40 |

Each call site keeps its own ring/border/online-dot decoration logic — `<UserAvatar>` just renders the picture + fallback letter inside whatever container its parent provides.

### Modify: existing event listeners

- **`useAuthEvents.ts`** — add a listener for the new `avatar_changed` event; call `avatarStore.setVersion(username, version)`. Add a `clearAll()` on the `logged_out` event.
- **`useFriendsEvents.ts`** — on `FRIEND_LIST_RES`, for each `FriendInfo` call `avatarStore.setVersion(username, version)`.
- **The presence-update listener** (wherever `PRESENCE_UPDATE` is consumed) — for each `UserPresence`, call `setVersion(username, version)`. This is the path that catches non-friend avatar updates.

---

## 8. Failure handling

| Failure | Detection | Recovery |
|---------|-----------|----------|
| Cropper produces no blob | `convertToBlob` resolves with `null` | toast "Couldn't save picture, try again"; modal stays open |
| Upload bytes too large (server rejects) | `UpdateAvatarRes.success=false` with message | toast with the server-provided message |
| Upload bytes not JPEG (server rejects) | same | same |
| Network drop during upload | `tokio::time::timeout` (5s) in native | napi command returns Err; renderer toasts "Upload failed, retry" |
| Fetch fails | `tokio::time::timeout` in native; or `FetchAvatarRes` with empty version + data when user not found | `avatarStore` marks `status='error'`; UI silently falls back to letter |
| `AvatarChanged` for unknown user | no current cache entry | `setVersion` no-op (only acts on cache misses); next `<UserAvatar username=X>` mount will fetch |
| Rapid back-to-back uploads | new `setVersion` revokes the previous `blobUrl` | safe — no leak |
| Server out of disk space | DB UPDATE fails | server responds `success=false, message="Storage error"`; client surfaces it |

---

## 9. Testing strategy

**Native unit tests:**
- `sha256_hex` round-trip (Rust client computes the same hash the server stores for identical input)
- JPEG magic-byte check (happy path with a real JPEG SOI; rejection for PNG, GIF, empty-but-nonzero bytes, all-zero)
- Bytes-too-large rejection (201 KB rejected, 200 KB accepted, 0 bytes accepted as remove)

**Server-side tests:**
- Idempotency: invoking `initializeDatabase` twice doesn't error on the new `ADD COLUMN`s
- `UPDATE` then `FETCH` round-trip: bytes stored and returned match exactly

**Manual integration matrix:**
- Upload + immediate self-display in AccountTab
- Friend sees your update mid-session (across two clients)
- Cold-launch with previously-uploaded friend → letter while loading, then image
- Remove avatar (empty upload) → letter fallback returns for self + peers
- Malformed: rename a `.txt` to `.jpg`, attempt upload → magic-byte rejection surfaced to user
- Oversize: bypass the cropper, upload a raw 10 MB JPEG → size-cap rejection surfaced
- Two friends update their avatar within ~1 second of each other → both broadcasts received, both invalidated, both refetched

---

## 10. Rollback

DB migration is purely additive (`ADD COLUMN IF NOT EXISTS`). Reverting code on the server while leaving the columns in place is safe — old server code doesn't read them. Reverting the client falls back to letter avatars at every site (because `<UserAvatar>` is replaced with its previous inline markup, AND the `avatar_version` fields in `FriendInfo` / `PresenceUpdate` are ignored).

If a release goes wrong, `git revert` the merge commit and redeploy. Future re-attempts can keep the existing columns; no schema un-do is ever required.

---

## 11. Out of design — open items intentionally deferred

- **Cache persistence across launches.** Currently in-memory only; first display after restart re-fetches. Acceptable for small-friend-list scale; promote to IndexedDB-backed cache if and when the fetch volume becomes painful.
- **Animated avatars.** Out of scope. The cropper output is always static JPEG; the server stores JPEG only. Adding animated support would mean a second storage path (different format + size limits) and a different cropper.
- **Re-encoding on the server.** We trust the canvas-encoded output. If a future security review wants belt-and-braces validation (e.g. defending against a malformed JPEG that crashes Chromium decoders downstream), the cleanest fix is to add `libvips` to the server build and re-encode on receive.
- **Cross-server avatar federation.** Community servers do not store or serve avatars; clients always look them up from central. A future federated-identity model could push avatars to community servers, but that's out of the project's current architecture.

---

## 12. Scope summary

Roughly **~600 LOC new** (server handlers + Rust commands + Zustand store + `<UserAvatar>` + cropper modal + Account-tab UI), **~150 LOC modified** (existing avatar call-site refactors), **1 SQL migration** (idempotent `ADD COLUMN`s), **5 new protobuf packet types** + **2 modified message shapes**.

All changes are visible at every avatar render site. No platform-specific gating. Linux / macOS / Windows behave identically.
