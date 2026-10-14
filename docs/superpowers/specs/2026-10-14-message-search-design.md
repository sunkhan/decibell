# Message search

2026-10-14. Owner's ask: a powerful search over text channels (later threads too) with filters for
sender, date, content, files and channel, without putting heavy load on the server.

Owner decisions (2026-10-14):

- **Encrypted channels: filters only.** Sender, date, channel and file-type filters find their
  messages (the server already knows those). Words in their text aren't searched, and the results
  say which channels were skipped. This fits the earlier decision to keep channel encryption off by
  default so server-side search works for everyone who never opens settings.
- **Discord-style flow.** A search box in the channel header (Ctrl+F) with filter suggestions that
  become chips, results in a right-hand panel with sorting, infinite scroll and Jump.
- **Proposal page before the UI is ported.** Server and native shipped first; the renderer was
  designed in a scratch copy and captured in every theme
  (https://claude.ai/artifact/UZfpnk8sLX8eXfWodykBej). The owner approved it the same day ("design
  looks great") and answered its three questions: the panel **floats over the chat below 1024 px**;
  a search covers **every channel the user can read** by default; **no recent searches** are kept.
- **Backend identity is the uid** (owner rule, restated while this was designed): the server keys
  senders by account id, so a rename never drops someone's older messages from a sender filter.

## What can be searched

One request searches one community. Every filter narrows the results:

| Filter | Wire | Meaning |
|---|---|---|
| Words | `query` | Free text. Words, `"exact phrases"`, `-excluded` words. Words of 3+ characters match as prefixes (`deploy` finds `deployment`; suits Turkish suffixes too). Accents fold (`cafe` = `café`), case is ignored, and the Turkish dotless ı matches i (`kisa` = `kısa`; owner request, 2026-10-14). Also matches attachment file names. |
| Inside words | `match_within_words` | Optional (owner request, 2026-10-14): the words and phrases match anywhere inside words (`abc` finds `xabcy`), folded the same way, punctuation kept (`c++`). A scan of the text, not an index lookup (see below); sorts newest first. |
| From | `authors` | Usernames. The server resolves each member to their uid and matches every message that account sent, under any earlier name. A non-member is matched by the name on their messages. |
| In | `channel_ids` | Text channels. Empty = every text channel the requester can read. |
| Date | `after` / `before` | Unix seconds, `after` inclusive, `before` exclusive. The client turns `before:`, `after:` and `during:` days into local-midnight bounds. |
| Has | `has` bits | `FILE` (any attachment), `IMAGE`, `VIDEO`, `AUDIO`, `DOCUMENT`, `LINK`. Each one set must hold. |
| Sort | `sort` | Newest (default), Oldest, Relevance (only with words; otherwise treated as Newest). Relevance ranks the newest 2,000 matches by `bm25`: scoring every match of a common word took ~1 s on 1M messages, 51 ms this way, and in chat the recent matches are the ones people look for. |

A request needs at least one of: words, a sender, a has-filter, a date, or a channel. Searching
"everything in every channel" is refused.

There are no mentions, pins or threads in Decibell yet, so there are no `mentions:` / `pinned:`
filters. **Threads, later**: a thread will be a channel whose parent is a text channel. Scoping stays a
list of channel ids, so `in:#general` will expand to its threads on the server and results already
carry their own `channel_id`. Nothing in the index is per-channel-type.

## Access

Only text channels where the requester has VIEW_CHANNEL and READ_HISTORY are searched. Requested
channels outside that set are dropped silently (as an unreadable history page is empty). The set is
resolved on the io thread at request time and checked again when the page is assembled.

## Index (schema v14)

- `messages_fts`: an FTS5 table with columns `content` and `files`, `content=''` with
  `contentless_delete=1`, tokenizer `unicode61 remove_diacritics 2`. Contentless means the index does
  not keep a second copy of the text. Contentless-delete means deleting a row is a tombstone by rowid,
  not a re-tokenisation of the old text. That was the cost that got the old FTS triggers dropped
  (wipes, prunes and ban purges tokenised every deleted row). Needs SQLite ≥ 3.43 with FTS5, so the
  community server builds its own pinned copy (`cmake/DecibellSQLite.cmake`: the 3.53.4
  amalgamation, hash-checked, FTS5 on, linked statically): Debian 12, which hosts the owner's
  server, ships 3.40. `-DDECIBELL_SYSTEM_SQLITE=ON` links the system library instead and refuses
  anything older than 3.43 at configure time. Should an older library ever be linked anyway, the
  server drops the triggers, logs it and answers searches with "Search isn't available on this
  server." An operator writing to the database by hand needs a sqlite3 CLI ≥ 3.43 too: an older one
  can read it, but a write to `messages` fires the index triggers and fails.
- Kept current by triggers, so every write path (send, edit, delete, wipe, channel delete, retention,
  ban purge) stays right without code in each:
  - insert of a plaintext row (`envelope IS NULL`) indexes its content;
  - edit re-indexes content plus the current file names, or removes the row if it became sealed;
  - delete removes it;
  - binding an unencrypted attachment to a message (`message_id` 0 → id) re-indexes with file names.
  Sealed rows, encrypted attachments and every row of an encrypted channel are never indexed: the
  placeholder text of a sealed row can never match, and neither can plaintext left in a channel from
  before its encryption was switched on (back when that was possible).
- Backfill once, in the migration, inside one transaction. The index has a version
  (`server_meta.search_index_version`); a different stored version rebuilds it once at startup.
  Version 2 stores ı as i, since unicode61 treats the dotless ı as a letter of its own rather than
  an accented i. The cost is a `replace()` in each trigger: 44–47 µs per insert either way,
  within noise.
- `messages.sender_uid` (backfilled from `members.uid` by name, then set from the session's JWT uid on
  every send; a pre-Theme-A member's rows are back-filled when they first authenticate with a uid),
  indexed `(sender_uid, id)`. Rows whose sender no longer matches any member keep uid 0. They can only
  be matched by name, never claimed by whoever holds that name now.

The same column fixes three existing rename bugs: deleting your own message, editing it (except a
sealed message in an encrypted channel, whose envelope binds the name it was sent under), and the ban
"delete messages" purge now go by uid.

## Keeping the load down

- **Off the io thread.** One `SearchWorker` thread owns its own read-only SQLite connection. WAL lets
  it read while the main connection writes, and it never takes the DB mutex. The io thread (which also
  relays voice) only validates, resolves the channel set, and later hydrates at most one page (≤ 50
  rows by primary key).
- **Bounded work per query.** A progress handler aborts a query after 3 s. If the page filled, the
  count stops there and reads "N+". If some results came in, they are returned with `partial`. If none
  did, the answer is "This search is too broad. Add a filter."
- **Bounded concurrency.** One query runs at a time server-wide, and at most 32 wait. A session has
  at most one: a new search replaces its queued one and cancels its running one, and a disconnect
  cancels too. A full queue answers "Search is busy" with a retry hint.
- **Rate limit.** A per-session `search_bucket_` (6 burst, 1 per s). A refused request is answered
  with `retry_after_ms`, not dropped, so the UI never hangs.
- **Every query drives from an index, in id order, and stops at limit + 1.**
  1. Words or LINK: the FTS index, walked by rowid (newest or oldest first), with the filters checked
     per candidate by primary key. Relevance sorts by `bm25` (file names weigh less than text).
  2. One or more members (uids): `(sender_uid, id)`.
  3. Has-file kinds: the attachments `message_id` index, newest first.
  4. A single channel: `(channel_id, id)`.
  5. Otherwise: the primary key, walked newest first.
  Date bounds are turned into id bounds by a binary search over the primary key (≈ 25 point lookups),
  because server timestamps grow with ids. The timestamp checks stay in the query so the bounds are
  exact.
- **Counted once.** The first page keeps stepping the same statement to count matches, up to 10,000
  and at most 250 ms more (then "N+"). Later pages don't count. Finding a page is cheap. An exact count
  of a common word under a narrow filter is the expensive part, so it gets the tighter bound.
- **Bounded answers.** 25 results by default (≤ 50), and the page is cut at 1.5 MB (a message can
  be 64 KB). Cursor paging for Newest / Oldest. Offset paging for Relevance, capped at 1,000 results.
- **Client side.** Searches run on Enter, never per keystroke. The filter suggestions come from the
  loaded roster and channel list, with the existing debounced `MEMBER_SEARCH_REQ` for big servers.
  Results already shown are kept while the next page loads.

## Inside words: a scan, not an index

The word index only knows where words start, so `xabcy` can't be found from `abc` with it. Three
options were measured on the 1M-message database:

| | Extra disk | Each sent message | Inside-word search |
|---|---|---|---|
| Trigram index, with positions | +104% (~310 MB) | 15 → 390 µs | < 1 ms |
| Trigram index, no positions | +36% (~108 MB) | 14 → 490 µs | < 15 ms, but substrings longer than 3 characters need re-checking against the text with Unicode folding |
| **Scan, no index (chosen)** | **none** | **unchanged** | **4–635 ms** |

For an optional mode, an index would make every message slower and the database much bigger
forever. The scan costs nothing until it's used, and then it's bounded like every search (worker
thread, 3 s budget, one at a time, newest first, stopping at the page plus the capped count).
The worker registers `decibell_fold` (lower case, Latin-1 and Latin Extended-A accents stripped,
ı/İ → i, Greek and Cyrillic capitals lowered, combining marks dropped) and checks
`instr(decibell_fold(content), fragment)` plus unencrypted file names. The fragment is folded by
the same function, so the two sides always agree. Measured through the server: a fragment of a
common word 7 ms; a rare fragment in one channel 29 ms, from one sender 4 ms, across the whole
server 434 ms; a fragment that matches nothing, whole server, 635 ms (the full scan, about
0.6 µs per message).

## Measured (1M messages, 30 channels, 300 senders, 62k attachments)

Synthetic community (Zipf-distributed words, 2 years of history), round trips on localhost, median of 5:

| Search | ms | Search | ms |
|---|---|---|---|
| rare word | 0.9 | common word (10,000+ results) | 24 |
| mid-frequency word (3,972) | 4.5 | common word, relevance | 51 |
| two words | 18 | 3-letter prefix | 27 |
| from: busiest member | 4.6 | from: quietest member | 1.3 |
| has:image | 18 | has:link | 15 |
| one day a year ago | 1.0 | before: 18 months ago | 2.0 |
| one channel, no words | 0.9 | rare word after a date | 0.7 |
| common word + one sender (1,138, exact) | 213 | common word in one channel | 136 |

The two slow rows are spent on the exact count, now capped at 250 ms. Index: +66 MB on a 230 MB
database (+29%), built from scratch in about 5 s at startup. Send path: an insert with the index trigger
costs 34 µs instead of 12 µs. FTS5 merge settings (automerge 0/8/16) made no measurable difference,
so the defaults stay.

## Wire

`MESSAGE_SEARCH_REQ = 156` / `MESSAGE_SEARCH_RES = 157`, payload slots 158 / 159.
`MessageSearchRequest { seq, query, channel_ids, authors, after, before, has, sort, cursor, offset,
limit }` and `MessageSearchResponse { seq, success, message, messages (ChannelMessage, in sort
order), has_more, next_cursor, next_offset, total, total_capped, text_skipped_channel_ids, partial,
retry_after_ms, first_page }`. Community-only and client-first safe: an older server never answers,
and the client's 8 s no-answer timer says the server needs updating.

## Native

`search_messages(args)` sends the request (every arg optional except `serverId`). The response goes
straight to a `message_search_received` event unless a message carries an envelope. Then it goes
through the ordered channel crypto worker (`ChannelJob::Search`), which opens each message with its
own `channel_id` (history uses the response's single channel). Decrypted payloads have the same shape
as history messages.

Found while surveying: `AttachmentPayload` never carried `encrypted`, so the renderer never merged the
real metadata of encrypted-channel attachments or registered their keys. Fixed alongside, since
search results take the same path.

## Client UI

- **Search box** (`features/search/SearchBox.tsx`) at the right end of every text channel's
  header, after the members button: 244 px, the members list's 260 px less the header's 16 px right
  padding, so its left edge lines up with the list's border and the members button stays over the
  chat (owner, 2026-10-14). The chips + text row
  scrolls sideways when it outgrows the box, with a fade at the left edge once scrolled. Ctrl+F / ⌘F
  focuses it from the server view. Filters are chips before the text. Typing `from:`, `in:`, `has:`,
  `before:`, `during:` or `after:` opens a picker: members (loaded roster plus `MEMBER_SEARCH_REQ`
  for offline ones), readable text channels (current first, lock for encrypted), the six kinds, or
  a calendar (`DatePicker.tsx`, locale week start, no future days). A finished filter followed by a
  space becomes a chip; Backspace at the start removes the last one. The menu never takes focus
  from the input. With words typed, "Search for …" is preselected; in an empty box nothing is,
  so Enter can't insert a filter by accident. The box's contents are kept per server for the
  session, never stored.
- **Results panel** (`SearchResultsPanel.tsx`) in the member list's place on the server it belongs
  to: count ("N+" when capped), Newest / Oldest / Relevant (off without words), notices for
  skipped encrypted channels and partial results, results grouped by consecutive channel, matched
  words marked as the index matched them (`highlight.tsx` → `MessageText`'s `highlight` option;
  file names too), compact attachments, reply context, infinite scroll. A click or Enter on a
  result jumps (`features/chat/goToMessage.ts`) and rings it. Esc outside an input closes the panel;
  closing keeps the results, and running the same search again just reopens them.
- **Below 1024 px** the panel floats over the chat under its header (owner decision) and Jump closes
  it so the message is in view.
- **Inside words** is a switch row under "Match" in the box's menu, and shows as a
  `match: inside words` chip (it travels with the query like the filters; Backspace or ✕ removes
  it). Results mark the fragments inside words; Relevant is off in this mode.
- **State** (`searchStore.ts`): one search at a time, answers matched by `seq`, an 8 s no-answer
  timer (the server predates search), automatic retries on `retry_after_ms` (up to 3), "load more"
  reusing the seq. Nothing persists: no recent searches (owner decision).

## Not in this version

- Text in encrypted channels (owner decision above). A later option: scan recently fetched history on
  the device.
- CJK word search: `unicode61` treats a run of CJK characters as one token. A trigram index would fix
  it at about 3× the index size.
- Showing a renamed member's older messages under their current name, and offering them Edit / Delete
  on those messages. The server accepts both by uid now, but the client only knows usernames, so this
  needs the uid on the wire.
