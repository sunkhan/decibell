# WIP: `wip/fix-servers` (communities, channels, roles, permissions: PARITY §4–§5)

Stopped 2026-11-27 ~17:40 at the owner's request. Branch `wip/fix-servers`, worktree `.worktrees/fix-servers`, head
`6e76c59e` (+ this note). Everything below compiles: `cargo check --workspace --all-targets`, `-p decibell-app
--features dev-local --all-targets` and the ship flavour (`--no-default-features --features ship --release`) were
clean at `ee14806e`; clippy and the unit tests of the touched crates (server-settings, servers, ui, shell,
chat-content) clean there too. The Windows type-check ran clean once with its own target dir
(`CARGO_TARGET_DIR=$HOME/.cache/decibell-dev/rustup-win/targets/fix-servers tools/heavy.sh tools/win-check.sh`) before
the last three commits: rerun it.

## The six items

1. **Alt+↑ / Alt+↓ role reordering: done.** `decibell-server-settings/src/server/roles.rs` (`RolesTab::move_by`,
   `row_focus`, the key handler in `role_row`). Self-test `roles-reorder` PASS (also covers drag, the RGB colour
   picker driven as UI, holders added through Add members and removed with ✕).
2. **Other §1 gaps: done.** Welcome ring turns while the window is visible, present-only ≤ 30 fps
   (`decibell-servers/src/welcome.rs`, quirk #26 with the measured cost); Create role no longer dimmed at 250.
3. **Leaving / joining: done.** `servers-leave-rejoin` PASS (a leave sticks 8 s; leave + immediate re-join through
   the invite stays). `server-owner-leave` PASS. A refused auto-rejoin can't be produced on the stack (HANDOFF §5).
4. **Missing tests: done, one left.** PASS in a full run on 2026-11-27 (each also passed earlier, so twice):
   `moderation-live`, `transfer-ownership-live` (Rustaceans bob → `stsettings` → bob), `server-picture-live`,
   `membership-added-live`, `server-settings-guard`, `channel-settings-more`, `overview-meta-live`,
   `servers-join-manual`, `welcome-gating`, `invite-manage-live` (clipboard kept and put back), `welcome-banner`,
   `content-invite`; touched tests still PASS: `bans-live`, `channel-settings-echo`, `roles-edit-echo`,
   `server-settings-open`, `members-search`, `category-sync`, `settings-narrow`, `servers-join-link`, `servers-idle`.
   **Left**: an invite card's Join *press* (PARITY 4.6 stays partial: the content tests' account is shared).
   **Open failure**: `server-settings-idle` fails here every time ("Overview: not idle (1 drawn, ~145 presented in
   5 s)"); the frozen parity build and the main tree's head pass it. A bisect reverting `welcome.rs`, `host.rs` and
   `decibell-ui/src/settings.rs` still failed, so the cause is elsewhere or in the base `504f272a` itself (the head
   has modal / popover / settings fixes the base lacks). **Next step**: build `504f272a` alone and run it
   (`tools/selftest.sh --bin … server-settings-idle`, twice); if it passes, bisect `overview.rs` (the focusable
   owner-picker trigger, the public switch's probe wrapper) and `ui.rs` / `members.rs`. The test's code now keeps no
   channel open under the window (`selftests.rs` `settings_idle`); that alone didn't fix it.
5. **Admin actions Electron has that this client lacked: done.** Compared tab by tab (HANDOFF §5 "Admin actions"):
   topic box disabled without Manage Channels (`channel/overview.rs`), ↑ / ↓ stepping in Storage's headroom
   (`server/storage.rs` `step_min_free`), Server Settings reopening on its last tab (`host.rs` `last_tab`, quirk
   #28), keyboard access (`ui.rs` `Focuses` / `focusable` / `menu_keys`, applied in members, bans, overview, roles,
   channel permissions). Left (recorded in HANDOFF §5): focus doesn't scroll a plain overflow list to the focused row;
   a role row scrolled out of the windowed list loses focus.
6. **PARITY rows 4.1–5.9: written** (`PARITY.md`); 4.6 partial, 4.8 needs owner (second-device leave race), the rest
   done. Final double run of every test was not finished (stopped mid-way).

Docs written: quirks #26–#28 (`docs/electron-quirks.md`), HANDOFF §5 (leave race checked live, refused rejoin not
reproducible, admin actions), CONVENTIONS (keyboard, throwaway-target tests, the ring, invites clipboard).

## The stack (`tools/dev/local-stack.sh status`)

Found: Daily Gaming 299, Rustaceans 29, Design Lab 44. Now: Daily Gaming 301, Rustaceans 29, Design Lab 45. The
extra members are **not** this branch's: `ht6b07f7dba`, `ht6b07f7dbb` (Daily Gaming) and `taa6c44bbd2` (Design Lab)
are other agents' throwaways (this branch's are `stsettings` and `sv<8 hex>`; none is a member anywhere). No
`st-*` / `sv-*` role or channel is left, no ban beyond the seeded `griefer_9000`, Design Lab has no picture, bob owns
Rustaceans, Daily Gaming is listed, Rustaceans' name and storage headroom are as found. If a run is interrupted:
`settings-cleanup` (as the community's owner, `DECIBELL_ST_USER=stsettings`, `DECIBELL_ST_PICTURE=1`) removes all of it;
a Rustaceans left owned by `stsettings` is given back by the `settings-throwaway` child's `give-back` phase.

## What the next session needs to know

- Popovers close when the window stops being the active one: a self-test that opens one holds
  `decibell_harness::focus::hold_window_focus(Some(true))` (other agents' windows took the desktop mid-click).
- A picker's rows move when the server answers the typed search (250 ms debounce): wait for it before pressing.
- `actions::chat::open_channel` only loads history; set `nav.set_active_channel` to show a channel.
- An unlisted community joined by host and port stays under the provisional `host:port` id until the next sign-in
  (the core re-keys only onto ids central has an address for); tests accept either id.
- Arrived after the brief: self-tests only through `tools/selftest.sh` on the branch head; the Windows type-check with
  its own target dir (above); the head fixed a Members-tab crash (a `hidden` / `group_hover → flex` swap in
  `members.rs`: never switch `display` by hover) — merging this branch onto the head must keep that fix (this branch
  also edits `members.rs`: the row ⋯ / + focus, the menu keys).
