# Roster: online members page too

2026-10-18. Left open by review item A1 (`docs/reviews/2026-10-17-pre-1.0-review.md`): `MEMBER_LIST_REQ`
paged only offline members. The first page carried every online member, so a community with more than about
20,000 members online at once produced a first page over the client's frame cap; the client skips such a
frame, and the member list never loaded. The page couldn't simply be cut, because the sidebar took page 1
as the complete online set (its "Online — N" header counted it).

## Wire (`proto/messages.proto`, "Roster protocol")

- **One sequence.** Pages run through every online member, then every offline member, each part in roster
  order. A page holds at most `limit` members (clamped to 1–1000, default 100) and may cross from the online
  part into the offline part. It also stops at the frame budget (`kMaxResponseBytes` less room for `extra`)
  and says so with `has_more`. The cursor stays opaque: `o|f \x1f uid \x1f name`, or a bare `f` (the offline
  part from its start) once the online part is all sent.
- **Roster order**, which the sidebar uses too: the display name (nickname, else username) folded the way
  message search folds text (`search_fold`: lower case, accents removed, ı as i), compared in code point
  order, then the display name itself, then the uid. The client mirrors it exactly
  (`features/members/rosterOrder.ts`), so every page extends the list at its end. Cross-checked against the
  server's C++ on 20,000 generated names (ASCII, Latin, Greek, Cyrillic, combining marks, surrogate pairs
  against U+E000–U+FFFF).
- **Counts on every roster packet.** `MemberListResponse.online_count` / `total_members`, and the same two
  fields on `MEMBER_UPSERT` / `MEMBER_REMOVE` (the counts after the change). A client can't derive them from
  deltas about members it hasn't loaded (an upsert for an unloaded member could be a flip or a nickname
  change), so the server sends them and the revision gap check keeps them exact.
- **`online_complete`**: the pages so far hold every online member. While it is false, every member of the
  page is online.
- **`extra`**: on a first page, the requester's own row when the page doesn't reach it. Permissions, the
  timeout pill and the nickname editor read our own record, and must not wait on paging.
- **Lookups**: `MemberListRequest.uids` (at most 200) returns those members wherever they sort, in `extra`,
  with `lookup_uids` echoing the request. For views that show someone the pages haven't reached.
- A refused `MEMBER_LIST_REQ` (rate limited) is answered (`success` false, `first_page` and `lookup_uids`
  echoed), never dropped: the client holds a loading state or a lookup until the reply.

## Community server: the roster index

`SessionManager` keeps every member in roster order in two `std::set`s, online and offline, plus a uid map.
Only `emit_member_upsert` / `emit_member_remove` change it, so pages and counts agree with what the deltas
told clients. A page costs O(log members + page) instead of loading and sorting the whole membership
(which every page did before, and every client does on every connect). Online = a member with a visible
session; a kicked member leaves the counts with their `MEMBER_REMOVE`, though their session is still
closing. Built on first use, and rebuilt when another connection commits to the database (`PRAGMA
data_version`: a hand edit, the test harness's seeded rows); the server's own writes all go through the
emits. A claimed legacy placeholder now gets a `MEMBER_REMOVE` (clients used to keep it beside the account's
new row). Role ids come from one batched query per page (`member_role_ids_for`).

## Client

- **The roster holds more than the sidebar shows.** Pages, deltas and lookups all land in
  `membersByServer`; deltas are applied for every member, loaded or not, and keep held records current. The
  sidebar (`visibleRoster`) draws each part only up to its loaded edge (`MemberRosterMeta.onlineEdge` /
  `offlineEdge`, the last member of that part the pages brought): a member coming online who sorts past the
  edge is held but not drawn until the pages reach their place, so the list never shows a gap. The offline
  part starts once the online part is complete. The loading row sits at the end of what's loaded (under the
  online part until it's complete) and loads the next page as it scrolls into view.
- **Headers are the server's counts**: "Online — 1,234", "Offline — total − online". Server settings'
  Members tab reads the same counts.
- **Online / offline is the community's** (`MemberInfo.is_online`: connected here and not invisible).
  Counts from the server only add up if the parts follow the server's split. Central's status still draws
  the dot inside the online part (away, Do Not Disturb), and an online member central hasn't heard from shows
  online. Before, the split followed central's status for loaded members, so someone online on Decibell but
  not connected to this community showed under Online; now they show under Offline. Needs the owner's
  confirmation (CLAUDE.md's user-status decision says member lists show central's status).
- **Page sizes**: 500 for the first page (all the online members of most communities, as before), 200 per
  later page.
- **Gaps**: a revision gap or a reconnect asks for page 1 once (`refetching`); deltas wait for it and scroll
  paging holds. A later page whose revision isn't ours means a delta went missing: refetch. A refused first
  page is asked again after 1 s.
- **Lookups** (`features/members/memberLookup.ts`, `useMember` / `useMemberInterest` in
  `hooks/useDisplayName.ts`): `useDisplayName` and the profile popup, profile modal, user context menu and the
  Overview tab's owner name ask for a member the roster lacks, batched per server (50 ms), once per roster
  generation (every first page starts a new one). A refused lookup is retried up to three times. Only for uids
  the community itself named; the friend list never goes through it.

## Consumers of "the roster holds every online member"

| Consumer | Status |
|---|---|
| Member sidebar (`MembersList`) | Fixed: loaded stretch per part, server counts, roster order |
| Display names / nicknames (`useDisplayName`: messages, replies, search results, voice tiles and lists, stream view, audit log) | Fixed: lookup when missing |
| Profile popup / modal (roles, joined date), user context menu (name, hierarchy for moderation) | Fixed: `useMember` |
| Own permissions, timeout pill, own nickname (permissions.ts, ChatPanel, RolesTab, PermissionsTab) | Fixed: own row in page 1's `extra` |
| Server settings Members tab counts | Fixed: server counts |
| Server settings opening (`ServerSettingsModal`) | Fixed: no longer re-fetches page 1, which replaced what the sidebar had paged in (deltas keep the roster live) |
| Overview tab owner name | Fixed: lookup |
| Overview tab transfer-ownership picker | Fixed: a popover searching the server (`MEMBER_SEARCH_REQ`), results kept out of the roster store |
| Invite friends filtering | Documented: loaded roster only by owner decision (no friend list to the community, so no lookups either); a member can still be listed and the invite comes back already_member |
| Message search `from:` picker | Fine: already merges a server search |
| Settings member / role / permission pickers | Fine: server search (`MEMBER_SEARCH_REQ`), separate |
| Mention autocomplete, typing indicators | Don't exist yet; would need a server search / lookups |
| Native | Fine: maps packets only, holds no roster |

## Not done

- ~~No virtualization in the sidebar~~ Done (follow-up): the sidebar draws only the items within 480 px of the
  viewport, with spacers keeping the scroll height; every item has a fixed whole-pixel height, measured from the
  CSS (`features/members/memberWindow.ts`). No library.
- An offline member whose rename the community hasn't seen yet sorts under the old name (the label is
  central's current name). Online members' renames reach the community live.
