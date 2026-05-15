# Custom Server Pictures — Design

**Date:** 2026-05-15
**Author:** sunkhan (with Claude)
**Status:** Approved (pending implementation plan)

## Problem

Community servers in Decibell's ServerBar render as small rounded rectangles with a generated gradient background, a single-letter avatar, and the server name beside it. Owners have no way to customise what their server looks like — every server with a name starting with "P" looks the same shade of purple, every "G" the same green. Discord-style custom server icons are missing.

## Goal

The owner of a community server can upload a custom image. Once set, the existing tile in the ServerBar is replaced with the image filling the rectangle:

- **Active** (currently viewing one of this server's channels / voice / streams): image only, no overlay, no text. Active-state underline still renders below the tile.
- **Inactive** (in another server, home, or a DM): image + dim overlay (`bg-black/45`) + server name centered on top in white semibold.
- **Pending auto-rejoin** (post-login, before community-auth lands): image with the existing `opacity-60` treatment plus the inactive overlay+name.

Servers without a picture set keep the existing gradient-letter-plus-name rendering unchanged.

Owners reach the upload UI through a new `ServerSettingsModal` mirroring the existing client-side `SettingsModal` chrome. Today's only entry is the picture; the structure is forward-compat for future server-wide settings (name/description edits, eventual role/permission management).

## Non-goals

- **Cropper UI / image transforms.** v1 accepts the user's image as-is and renders via `object-cover` (center-crop). If the framing is wrong they re-upload.
- **Animated pictures.** JPEG and PNG only; no GIF. Same constraint as the user-avatar feature.
- **Per-role permissions today.** Owner-only check today via the existing `db->owner() == username_` pattern (inline, like `CHANNEL_WIPE_REQ` and `INVITE_CREATE_REQ`). When roles ship, this becomes a `can_edit_server_picture(username)` helper alongside `can_delete_others` — same forward-compat shape.
- **Picture preview in ServerBrowseView / member lists / non-ServerBar surfaces.** v1 is ServerBar only. The wire protocol carries `picture_version` everywhere `CommunityServerInfo` flows, so other surfaces can opt in later without re-shipping protocol.
- **Background prefetch of every server's bytes on login.** Lazy-fetch on first tile render is enough; the visual gap (a tile briefly showing without an image) is small and the fetch is small.

## Architecture

```
                          UPDATE_SERVER_PICTURE_REQ
   ┌──────────┐          (JWT, picks up community            ┌──────────┐
   │ client   │           session for serverId)              │community │
   │ (owner)  │ ───────────────────────────────────────────▶ │ server   │
   │          │                                              │          │
   │          │ ◀──── UPDATE_SERVER_PICTURE_RES ───────────  │ owner +  │
   │          │       {success, version}                     │ size     │
   └──────────┘                                              │ check    │
                                                             └────┬─────┘
                                                                  │
                                       SYNC_SERVER_PICTURE_REQ    │
                                       (shared secret, host+port  │
                                       + bytes + version)         │
                                                                  ▼
                                                             ┌──────────┐
                                                             │ central  │
                                                             │          │
                                                             │ stores   │
                                                             │ in       │
                                                             │ community_
                                                             │ servers  │
                                                             └────┬─────┘
                                                                  │
                                       SERVER_PICTURE_CHANGED     │
                                       {server_id, version} to    │
                                       every online session that  │
                                       is a member (joins         │
                                       user_communities)          │
                                                                  ▼
                                                             ┌──────────┐
                                                             │ every    │
                                                             │ member's │
                                                             │ client   │
                                                             └──────────┘


   ┌──────────┐  FETCH_SERVER_PICTURE_REQ                    ┌──────────┐
   │ client   │ ───────────────────────────────────────────▶ │ central  │
   │ (any)    │ ◀──── FETCH_SERVER_PICTURE_RES ───────────── │          │
   │          │       {server_id, version, data}             │          │
   └──────────┘                                              └──────────┘
```

Storage lives on central in `community_servers` (the directory table that already tracks each community's host/port/name). Picture uploads are authorised by the community (which is the source of truth for ownership) and then forwarded to central over the shared-secret channel — same shape as `MEMBERSHIP_REGISTER_REQ` in the auto-rejoin feature. Central broadcasts version changes to every online session whose username is in `user_communities` for that `server_id`. Fetches are direct client↔central with no community involvement.

## Section 1: UX

### Tile rendering

Two render branches keyed off `server.pictureVersion`:

**Empty `pictureVersion`** — existing rendering, unchanged:
- Outer button: `h-[38px] shrink-0 items-center gap-2 rounded-lg px-3.5 text-[13px] font-semibold`, active/inactive class variants
- Inside: gradient bg avatar (h-5 w-5, `stringToGradient(name)`) + first letter + truncated name (`max-w-[100px]`)
- Active underline: `absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent`

**Non-empty `pictureVersion`** — image fills the rectangle:
```tsx
<button … className="… relative overflow-hidden …">
  <img
    src={pictureDataUrl ?? PLACEHOLDER_DATA_URL}
    alt={server.name}
    className="absolute inset-0 h-full w-full object-cover"
  />
  {!isActive && (
    <>
      <div className="absolute inset-0 bg-black/45" />
      <span className="relative text-[13px] font-semibold text-white truncate max-w-[100px]">
        {server.name}
      </span>
    </>
  )}
  {isActive && (
    <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
  )}
</button>
```

The tile retains its existing variable width (gradient case had the name beside the avatar; the picture case has the name overlaid on top, same width). The `max-w-[100px]` truncation still applies to the overlaid name.

Pending auto-rejoin tiles (`pendingMembershipServerIds.has(server.id)`) get the existing `opacity-60` + `cursor-wait` treatment regardless of which render branch they fall into.

### Settings modal entry

A new dropdown entry "Server Settings" appears in `ServerActionsDropdown` for owners only (visibility gated by the new `useCanEditServerSettings(serverId)` hook). Clicking it opens `ServerSettingsModal` via `openModal("server-settings")`.

`ServerSettingsModal` mirrors the existing `SettingsModal` chrome 1:1:
- 820×560 fixed-size portal modal with rounded-2xl border, dark bg, drop shadow
- Fade-in (0.65 backdrop) + scale-0.95→1 transition
- Esc closes; backdrop click closes
- Left sidebar (210px wide, `bg-bg-darkest`, `border-r border-border-divider`): tab pills with icon + label, active state `bg-accent-soft text-text-primary`
- Right pane: title + close-button row, scroll-y content area

v1 has a single tab — "Overview" — containing:
- Current picture preview (or placeholder when no picture is set), rendered ~120×120 inside the tab content
- **Upload picture** button → file picker (JPEG/PNG, validates client-side ≤ 200 KB) → fires `invoke("update_server_picture", { serverId, data })`
- **Remove picture** button (only shown when a picture is currently set) → fires the same command with an empty Buffer

A single-tab sidebar is visually sparse but acceptable; the structure is in place so adding "Roles", "Permissions", "Overview/description editing" later doesn't require a layout change.

### Mid-session refresh

When the owner uploads a picture, every other online member sees their ServerBar tile update within ~50-500ms: central's broadcast lands, the renderer's `chatStore.setServerPictureVersion(serverId, version)` clears any cached bytes for that server, the next tile render's lazy-fetch effect pulls the new bytes.

## Section 2: Permissions

**Server-side gate (community):** owner-only today, enforced inline in the `UPDATE_SERVER_PICTURE_REQ` handler — same pattern as `CHANNEL_WIPE_REQ` (`db->owner() != username_` → reject with a user-readable message):

```cpp
if (db->owner() != username_) {
    res->set_success(false);
    res->set_message("Only the server owner can change the server picture.");
    send_packet(rsp);
    return;
}
```

When roles ship later, the check becomes `db->can_edit_server_picture(username_)` — same forward-compat path as `can_delete_others` from the message-deletion feature. No schema changes today.

**Renderer-side check:** the new `useCanEditServerSettings(serverId)` hook compares `chatStore.serverOwner[serverId]` against `localUsername`. Same internal shape as `useCanDeleteOthers` so when roles ship both hooks pick up the role-permission OR clause together.

**No new auth surface on central:** `SYNC_SERVER_PICTURE_REQ` rides the existing shared-secret channel (joins the JWT-gate whitelist at `src/server/main.cpp:152` alongside `MEMBERSHIP_REGISTER_REQ`). `FETCH_SERVER_PICTURE_REQ` rides the existing JWT-authed central session (no whitelist entry — it's a regular authenticated request).

## Section 3: Storage and wire protocol

### Schema (central)

`community_servers` gains two columns. Both added idempotently inside the existing `upsertCommunityServer` DDL block so the migration is automatic on already-deployed servers:

```sql
ALTER TABLE community_servers ADD COLUMN IF NOT EXISTS picture BYTEA;
ALTER TABLE community_servers ADD COLUMN IF NOT EXISTS
    picture_version VARCHAR(64) NOT NULL DEFAULT '';
```

`picture` is NULL when no picture is set; `picture_version` is `''` then.

### Schema (community)

No schema changes. Community proxies the bytes through to central and stores nothing locally.

### Proto additions

Continues from the message-delete packet types (ended at 81):

```proto
// Packet types
UPDATE_SERVER_PICTURE_REQ = 82;  // client→community (JWT)
UPDATE_SERVER_PICTURE_RES = 83;  // community→requester
SYNC_SERVER_PICTURE_REQ   = 84;  // community→central (shared secret)
FETCH_SERVER_PICTURE_REQ  = 85;  // client→central (JWT)
FETCH_SERVER_PICTURE_RES  = 86;  // central→requester
SERVER_PICTURE_CHANGED    = 87;  // central→every online member of this server

// Messages
message UpdateServerPictureReq { bytes data = 1; }   // empty = remove
message UpdateServerPictureRes {
  bool   success = 1;
  string message = 2;
  string version = 3;  // sha256-hex; '' on removal or failure
}
message SyncServerPictureReq {
  string host = 1;
  int32  port = 2;
  bytes  data = 3;
  string version = 4;  // pre-computed by community
}
message FetchServerPictureReq  { int32 server_id = 1; }
message FetchServerPictureRes  {
  int32  server_id = 1;
  string version = 2;
  bytes  data = 3;
}
message ServerPictureChanged   {
  int32  server_id = 1;
  string version = 2;
}
```

### `CommunityServerInfo` extension

The directory message gains:

```proto
message CommunityServerInfo {
  int32  id = 1;
  string name = 2;
  string description = 3;
  string host_ip = 4;
  int32  port = 5;
  int32  member_count = 6;
  string picture_version = 7;   // '' when no picture set
}
```

This propagates the version through every existing surface that ships `CommunityServerInfo` — `ServerListResponse.servers` and `LoginResponse.memberships`. Clients know up-front which tiles have a picture set without an extra round-trip.

### Authentication notes

- `UPDATE_SERVER_PICTURE_REQ`: JWT-authed via the community session (existing community gate; no changes).
- `SYNC_SERVER_PICTURE_REQ`: shared-secret authed. Must be added to central's JWT-gate whitelist at `src/server/main.cpp:152` alongside `MEMBERSHIP_REGISTER_REQ` / `MEMBERSHIP_REVOKE_REQ`, otherwise central drops it with "Missing or invalid JWT" before the handler ever runs.
- `FETCH_SERVER_PICTURE_REQ`: JWT-authed via central session (existing central gate). No new whitelist entry.
- `SERVER_PICTURE_CHANGED`: pushed by central; no auth.

### Format and size

- JPEG or PNG only (the renderer validates by sniffing magic bytes before uploading; the server treats `data` as opaque bytes and trusts the client).
- Max 200 KB on the wire (matches `UpdateAvatarReq`'s cap; community rejects with `"Image exceeds 200 KB."` if larger).
- Stored as-uploaded — no transcoding, no thumbnailing. Rendered via `object-cover` so any aspect ratio displays centered into the rectangular tile.
- v1 has no cropper UI. Users re-upload if the framing is off.

## Section 4: Server-side processing

### Community — UPDATE_SERVER_PICTURE_REQ handler

```
authenticated_ already required (gate at top of route_packets)
  ↓
auto* db = manager_.db();
const auto& req = packet.update_server_picture_req();
  ↓
chatproj::Packet rsp;
rsp.set_type(chatproj::Packet::UPDATE_SERVER_PICTURE_RES);
auto* res = rsp.mutable_update_server_picture_res();
  ↓
if (!db) { … "Server misconfigured." }
if (db->owner() != username_) { … "Only the server owner can change the server picture." }
if (req.data().size() > 200 * 1024) { … "Image exceeds 200 KB." }
  ↓
std::string version = req.data().empty() ? "" : sha256_hex(req.data());
res->set_success(true);
res->set_version(version);
send_packet(rsp);
  ↓
// Forward to central. Fire-and-forget — pattern is exactly
// sync_invite_register / sync_membership_register.
chatproj::Packet pkt;
pkt.set_type(chatproj::Packet::SYNC_SERVER_PICTURE_REQ);
pkt.set_auth_token(central_jwt_secret_);
auto* sync = pkt.mutable_sync_server_picture_req();
sync->set_host(public_ip_);
sync->set_port(community_port_);
sync->set_data(req.data());
sync->set_version(version);
// Detached thread: send_to_central_blocking(host, port, framed)
```

`sha256_hex` is a small file-local helper inside `src/community/main.cpp`, using the existing OpenSSL link (`<openssl/sha.h>`). One static function, ~20 lines.

### Central — SYNC_SERVER_PICTURE_REQ handler

```
if (!auth_manager_.verifySharedSecret(packet.auth_token()))
    → drop with security log

const auto& req = packet.sync_server_picture_req();
int server_id = auth_manager_.setServerPicture(
    req.host(), req.port(), req.data(), req.version());

if (server_id == 0) return;  // no community_servers row yet

auto members = auth_manager_.getServerMembers(server_id);

chatproj::Packet bcast;
bcast.set_type(chatproj::Packet::SERVER_PICTURE_CHANGED);
auto* b = bcast.mutable_server_picture_changed();
b->set_server_id(server_id);
b->set_version(req.version());

manager_.broadcast_to_users(bcast, members);
```

`broadcast_to_users` is a new SessionManager helper sitting next to the existing `send_private` (which is single-user). If the manager already exposes a multi-user broadcast helper (e.g. `broadcast_to_friends` for friend-list updates), reuse that; otherwise add a small one — iterate `sessions_` once, deliver to sessions whose `username_` is in the members set.

### Central — FETCH_SERVER_PICTURE_REQ handler

```
if (!authenticated_) return;

const auto& req = packet.fetch_server_picture_req();
auto [version, data] = auth_manager_.getServerPicture(req.server_id());

chatproj::Packet rsp;
rsp.set_type(chatproj::Packet::FETCH_SERVER_PICTURE_RES);
auto* res = rsp.mutable_fetch_server_picture_res();
res->set_server_id(req.server_id());
res->set_version(version);
res->set_data(data);
// version + data are both empty when the server has no picture
// or the id is unknown. Renderer treats empty version as "no picture
// to render"; doesn't error.
deliver(framed);
```

No membership check on FETCH. Matches today's `FETCH_AVATAR_REQ` model — any authenticated user can fetch any user's avatar, and any authenticated user can fetch any server's picture. Server pictures are public-by-nature (visible on every member's ServerBar).

### `AuthManager` new methods

```cpp
/// Atomic update. Looks up the community_servers row by (host_ip, port)
/// and writes picture + picture_version. Returns the assigned id, or
/// 0 if no matching row exists yet (community heartbeat hasn't landed).
int setServerPicture(const std::string& host_ip, int port,
                       const std::string& data,
                       const std::string& version);

/// Returns (version, data). Both empty when the server has no picture
/// or the id is unknown.
std::pair<std::string, std::string> getServerPicture(int server_id);

/// Every username in user_communities for this server_id. Used by the
/// central broadcast helper.
std::vector<std::string> getServerMembers(int server_id);
```

The two existing `CommunityServerInfo`-returning queries (`getCommunityServers` for the directory and `getUserCommunities` for auto-rejoin) gain `cs.picture_version` in their SELECT lists and populate the new proto field. No new SQL helpers — small additions to the existing ones.

### Heartbeat DDL update

`upsertCommunityServer` already contains the `CREATE TABLE IF NOT EXISTS community_servers (...)` block. The two `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` statements above land right after the create-table call, identical pattern to the existing avatar-version migration on `users`.

## Section 5: Native (napi-rs) + Renderer

### Native — commands

```rust
// commands/servers.rs (appended)

#[napi(object)]
pub struct UpdateServerPictureArgs {
    pub server_id: String,
    pub data: Buffer,  // empty buffer = remove
}
/// Sends UPDATE_SERVER_PICTURE_REQ over the community session for
/// server_id. Ack lands as `server_picture_update_responded`.
#[napi]
pub async fn update_server_picture(args: UpdateServerPictureArgs) -> napi::Result<()>;

#[napi(object)]
pub struct FetchServerPictureArgs {
    pub server_id: i32,
}
/// Sends FETCH_SERVER_PICTURE_REQ over the JWT-authed central session.
/// Response lands as `server_picture_received`.
#[napi]
pub async fn fetch_server_picture(args: FetchServerPictureArgs) -> napi::Result<()>;
```

Mirror the existing `delete_channel_message` / `delete_dm_message` shapes — `send_for_server` for the community-targeted command, JWT-authed central packet build for the fetch.

### Native — events

Three new events emitted from `net/central.rs` and `net/community.rs`:

| Event | Payload (camelCase) | Source packet |
|---|---|---|
| `server_picture_update_responded` | `{success, message, serverId, version}` | `UPDATE_SERVER_PICTURE_RES` (community) |
| `server_picture_received` | `{serverId, version, data}` (`data` = base64 string of JPEG/PNG bytes) | `FETCH_SERVER_PICTURE_RES` (central) |
| `server_picture_changed` | `{serverId, version}` | `SERVER_PICTURE_CHANGED` (central) |

Rust converts the raw bytes into a base64 data URL prefix (`data:image/jpeg;base64,...` — sniffed from magic bytes, fall back to `image/png`) before emission so the renderer drops the data URL directly into `<img src>` without further work.

### Renderer — store additions

`chatStore.ts` extends with:

```ts
/// Per-server sha256-hex picture version. '' = no picture set.
/// Populated from CommunityServerInfo payloads (server_list_received,
/// memberships_received) and from server_picture_changed events.
serverPictureVersions: Record<string, string>;
/// Per-server cached image as a data URL. Populated lazily by the
/// fetch effect when a tile sees a non-empty version with no cached
/// bytes.
serverPictures: Record<string, string>;

setServerPictureVersion: (serverId: string, version: string) => void;
setServerPictureData: (serverId: string, version: string, dataUrl: string) => void;
```

`setServerPictureVersion` clears `serverPictures[serverId]` when the new version differs from the current cached one — invalidates the bytes so the next render triggers a fresh fetch.

`setServerPictureData` only writes if the version it was fetched for still matches the current `serverPictureVersions[serverId]`. Guards against a slow fetch landing after a newer version-changed event invalidates it.

Both maps clear via `resetForLogout` alongside the other server-related fields.

### Renderer — event wire-up

`useServerEvents` adds three listeners:
- `server_picture_update_responded` — on success the modal closes implicitly (the broadcast updates the tile so visual feedback is the change itself; no extra toast); on failure `toast.error(message)` and the modal stays open
- `server_picture_changed` — call `setServerPictureVersion(serverId, version)`. Next render of the tile triggers `fetch_server_picture`.
- `server_picture_received` — call `setServerPictureData(serverId, version, dataUrl)`.

The existing `server_list_received` and `memberships_received` listeners (auto-rejoin) extend to populate `serverPictureVersions` from the new `pictureVersion` field on each `ServerInfo` payload.

### Renderer — `useCanEditServerSettings` hook

New file `electron-client/src/features/servers/useCanEditServerSettings.ts`:

```ts
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";

/// Owner-only today. When roles ship, extends to:
///   || hasRolePermission(serverId, "EDIT_SERVER_SETTINGS").
export function useCanEditServerSettings(serverId: string | null): boolean {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  if (!serverId || !localUsername || !owner) return false;
  return owner === localUsername;
}
```

### Renderer — `ServerSettingsModal`

New file `electron-client/src/features/servers/ServerSettingsModal.tsx`.

Chrome mirrors `SettingsModal.tsx` exactly:
- `createPortal` to `document.body`
- 820×560 fixed-size rounded-2xl container with the same drop-shadow + border tokens
- Fade-in (`rgba(0,0,0,0.65)` backdrop) + scale-95→1 transition tied to a `visible` state
- `useEffect` Escape listener → `closeModal()`
- Click on backdrop → `closeModal()`; click stop-propagation on inner container
- Left sidebar: 210px wide `bg-bg-darkest`, `border-r border-border-divider`, tab pills with the same icon+label styling
- Right pane: title + close-button row, `flex-1 overflow-y-auto scrollbar-thin` content area

v1 tab list:

| id | label | content |
|---|---|---|
| `overview` | Overview | Server picture management (preview + Upload + Remove) |

Tab content:
```tsx
<div className="flex flex-col gap-4">
  <h3 className="text-[14px] font-semibold text-text-primary">Server picture</h3>
  <p className="text-[13px] text-text-secondary">
    Shown in the server bar in place of the default gradient and letter.
    Square images work best; max 200 KB.
  </p>
  <div className="flex items-center gap-6">
    <ServerPicturePreview serverId={activeServerId} size={120} />
    <div className="flex flex-col gap-2">
      <button className="px-4 py-2 rounded-lg bg-accent text-white …" onClick={pickAndUpload}>
        Upload picture
      </button>
      {hasPicture && (
        <button className="px-4 py-2 rounded-lg border border-border text-text-primary …" onClick={removePicture}>
          Remove picture
        </button>
      )}
    </div>
  </div>
</div>
```

`ServerPicturePreview` is a small inline component using the same chatStore read pattern as the ServerBar tile (`serverPictureVersions` + `serverPictures` + lazy fetch). Renders the gradient-letter fallback when no picture.

`pickAndUpload`:
1. Opens the existing file picker (`pickFiles` from `features/chat/filePicker` or similar — confirm at impl time which helper exists)
2. Validates `data.size <= 200_000`; rejects with toast.error otherwise
3. Validates magic bytes for JPEG (`FF D8 FF`) or PNG (`89 50 4E 47`); rejects with toast.error otherwise
4. `invoke("update_server_picture", { serverId, data: Buffer.from(bytes) })`

`removePicture`:
1. `window.confirm("Remove the server picture? The default gradient and letter will be used instead.")`
2. `invoke("update_server_picture", { serverId, data: Buffer.alloc(0) })`

### Renderer — `ServerActionsDropdown` entry

Adds a new dropdown item "Server Settings" above the existing "Leave Server" entry. Wrapped in `canEditServerSettings && (...)` so non-owners don't see it. Same dropdown convention as the existing entries (invite-manage, channel-settings).

Wiring it up: ServerChannelsSidebar (or wherever the dropdown's other modals are mounted today) gets a new `activeModal === "server-settings" && <ServerSettingsModal serverId={activeServerId} />` line.

### Renderer — `ServerBar` tile rendering

Two render branches inside the existing `visible.map(...)`:

```tsx
{visible.map((server) => {
  const isPending = pendingMembershipServerIds.has(server.id);
  const isActive = activeServerId === server.id;
  const pictureVersion = serverPictureVersions[server.id] ?? "";
  const pictureDataUrl = serverPictures[server.id];
  const hasPicture = pictureVersion !== "";

  // Lazy-fetch the bytes when a version is known but no cached data
  // exists. Module-level Set<"${serverId}:${version}"> guards against
  // duplicate in-flight fetches.
  useFetchServerPictureIfMissing(server.id, pictureVersion, pictureDataUrl);

  if (!hasPicture) {
    return /* existing gradient-letter tile, unchanged */;
  }

  return (
    <button
      key={server.id}
      onClick={() => !isPending && handleServerClick(server.id)}
      disabled={isPending}
      title={isPending ? "Connecting…" : server.name}
      className={`relative flex h-[38px] shrink-0 items-center justify-center overflow-hidden rounded-lg px-3.5 transition-all duration-200 ${
        isPending
          ? "cursor-wait opacity-60"
          : "hover:-translate-y-px"
      }`}
    >
      <img
        src={pictureDataUrl ?? PLACEHOLDER_DATA_URL}
        alt={server.name}
        className="absolute inset-0 h-full w-full object-cover"
      />
      {!isActive && (
        <>
          <div className="absolute inset-0 bg-black/45" />
          <span className="relative max-w-[100px] truncate text-[13px] font-semibold text-white">
            {server.name}
          </span>
        </>
      )}
      {isActive && (
        <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
      )}
    </button>
  );
})}
```

`useFetchServerPictureIfMissing` is a small hook colocated in this file:

```tsx
const inflightFetchesRef = useRef<Set<string>>(new Set());

function useFetchServerPictureIfMissing(
  serverId: string,
  version: string,
  cachedDataUrl: string | undefined,
) {
  useEffect(() => {
    if (!version || cachedDataUrl) return;
    const key = `${serverId}:${version}`;
    if (inflightFetchesRef.current.has(key)) return;
    inflightFetchesRef.current.add(key);
    invoke("fetch_server_picture", { serverId: parseInt(serverId, 10) })
      .catch(console.error)
      .finally(() => inflightFetchesRef.current.delete(key));
  }, [serverId, version, cachedDataUrl]);
}
```

The hook is called inside the map; that's fine because hooks must be called in the same order each render and the map iterates `visible` in stable order. (If lint complains, the hook can also live as a child component per tile — small refactor, same effect.)

`PLACEHOLDER_DATA_URL` is a tiny 1×1 transparent PNG so the `<img>` doesn't flash a broken-image icon during the brief lazy-fetch window. Inline constant at the top of the file.

## Error handling matrix

| Failure mode | Behavior |
|---|---|
| Non-owner sends `UPDATE_SERVER_PICTURE_REQ` to community | `UPDATE_SERVER_PICTURE_RES {success=false, "Only the server owner..."}`. Renderer surfaces toast.error. UI never offers the option to non-owners but defends against forged packets. |
| Image > 200 KB | Renderer rejects pre-upload with toast.error. Defense-in-depth: community also rejects with the same message. |
| Wrong format (not JPEG/PNG) | Renderer rejects pre-upload via magic-byte sniff. Server treats `data` as opaque, no format validation. |
| Community sends `SYNC_SERVER_PICTURE_REQ` before its first heartbeat | `community_servers` row doesn't exist yet → `setServerPicture` returns 0 → no broadcast. The picture upload was already acked to the client successfully, but the central-side state is missing. Owner re-uploads after the next heartbeat lands; not a common race in practice. |
| Central rejects `SYNC_SERVER_PICTURE_REQ` due to JWT-gate misconfiguration | Discovered at first-deploy via central log. Same diagnostic path as the auto-rejoin missing-whitelist bug we hit on 0.6.4. Adding the whitelist entry is a one-line fix. |
| `FETCH_SERVER_PICTURE_REQ` for an unknown server_id | Central returns `{version="", data=""}`. Renderer treats empty version as "no picture to render" and falls back to gradient-letter. Idempotent and harmless. |
| Slow fetch races a version-changed broadcast | `setServerPictureData` checks the version against the current `serverPictureVersions[serverId]` before writing — stale fetches are discarded. |
| Network error during upload | Renderer catches the rejected promise, surfaces toast.error, modal stays open so the user can retry. |
| Logout during upload in-flight | Modal closes on logout (`resetForLogout` clears `activeModal`); pending RPC fires-and-forgets. Server-side handler will reject if the JWT no longer authenticates. No client-side state corruption. |

## File-level change list (preview for the implementation plan)

- `proto/messages.proto` — 6 new packet types (82-87), 6 new oneof entries, 6 new messages, +1 field on `CommunityServerInfo`
- `src/server/auth_manager.{hpp,cpp}` — `setServerPicture` / `getServerPicture` / `getServerMembers`; the two `ALTER TABLE` migrations inside `upsertCommunityServer`'s DDL block; updates to `getCommunityServers` and `getUserCommunities` to include `picture_version`
- `src/server/main.cpp` — JWT-gate whitelist entry for `SYNC_SERVER_PICTURE_REQ`; new handlers for `SYNC_SERVER_PICTURE_REQ` (with broadcast) and `FETCH_SERVER_PICTURE_REQ`; `broadcast_to_users` helper if one isn't already there
- `src/community/main.cpp` — new handler for `UPDATE_SERVER_PICTURE_REQ` (owner gate + size check + forward to central); small `sha256_hex` helper
- `electron-client/native/src/commands/servers.rs` — `update_server_picture` + `fetch_server_picture` napi commands
- `electron-client/native/src/net/central.rs` — `FETCH_SERVER_PICTURE_RES` + `SERVER_PICTURE_CHANGED` packet arms → emit events
- `electron-client/native/src/net/community.rs` — `UPDATE_SERVER_PICTURE_RES` packet arm → emit event
- `electron-client/native/src/events.rs` — 3 new event names + payload structs + emit helpers; base64-encode bytes in the fetch arm before emit
- `electron-client/src/stores/chatStore.ts` — `serverPictureVersions` + `serverPictures` fields, 2 new actions, populate from existing `server_list_received` / `memberships_received` handlers, reset on logout
- `electron-client/src/features/servers/useServerEvents.ts` — 3 new listeners
- `electron-client/src/features/servers/useCanEditServerSettings.ts` (new) — owner-only check today
- `electron-client/src/features/servers/ServerSettingsModal.tsx` (new) — tabbed modal mirroring `SettingsModal`, single "Overview" tab with picture management
- `electron-client/src/features/servers/ServerActionsDropdown.tsx` — new "Server Settings" entry, owner-only visibility
- `electron-client/src/features/channels/ServerChannelsSidebar.tsx` — mount `ServerSettingsModal` conditional on `activeModal === "server-settings"`
- `electron-client/src/features/servers/ServerBar.tsx` — two render branches keyed off `pictureVersion`; `useFetchServerPictureIfMissing` hook; `PLACEHOLDER_DATA_URL` constant

Rough effort: ~700 LOC new, ~200 LOC modified.
