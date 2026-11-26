#!/usr/bin/env node
// End-to-end smoke for a built addon against the LOCAL TEST STACK
// (tools/dev/README.md: `tools/dev/local-stack.sh up`), outside Electron:
// it drives real commands the way the renderer does and asserts on the
// events that come back.
//
//   init → login (perf1) → login_succeeded + memberships_received →
//   request_server_list → request_dm_conversations → native auto-connects
//   to Daily Gaming (community_auth_responded) → get_attachment_target +
//   list_members → request_channel_history (#general, newest page) →
//   send_channel_message (#off-topic) and its echo (message_received with
//   our nonce) → request_friend_list → a few refused calls (their error text
//   and code reach JS unchanged) → logout (logged_out) → shutdown
//
// Then it prints the milestone sequence and every event name with its count,
// so two builds can be compared (ids / timestamps left out).
//
// It refuses to run unless DECIBELL_CENTRAL points at a loopback address:
// it must never reach the live central. Needs a DEBUG build (release builds
// ignore DECIBELL_CENTRAL unless built with the `local-central` feature).
//
//   cd electron-client/native && DECIBELL_CENTRAL=127.0.0.1:18080 node scripts/smoke-stack.cjs
//
// The account defaults to perf1 (a member of Daily Gaming only); override
// with SMOKE_USER. The password comes from the stack's accounts.json
// ($DECIBELL_DEV_DIR, default ~/.cache/decibell-dev). Each run posts one
// message to #off-topic. Exit 0 on success; 1 with a message otherwise.

"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function fail(msg) {
  console.error(`[stack] FAIL: ${msg}`);
  process.exit(1);
}

// ── Loopback guard ──────────────────────────────────────────────────
const central = process.env.DECIBELL_CENTRAL || "";
{
  const m = /^(?:\[([^\]]+)\]|([^:]+)):(\d+)$/.exec(central);
  const host = m ? m[1] || m[2] : null;
  const loopback =
    host !== null &&
    (/^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(host) || host === "localhost" || host === "::1");
  if (!loopback) {
    fail(`DECIBELL_CENTRAL must be a loopback host:port (got ${JSON.stringify(central)}); refusing to run`);
  }
}

const devDir = process.env.DECIBELL_DEV_DIR || path.join(os.homedir(), ".cache", "decibell-dev");
const accountsPath = path.join(devDir, "run", "accounts.json");
if (!fs.existsSync(accountsPath)) fail(`no ${accountsPath}: is the local stack set up and seeded?`);
const accounts = JSON.parse(fs.readFileSync(accountsPath, "utf8"));
const USER = process.env.SMOKE_USER || "perf1";
const account = accounts.accounts.find((a) => a.name === USER);
if (!account) fail(`no account ${USER} in ${accountsPath}`);
const community = accounts.communities.find((c) => c.name === "Daily Gaming");
if (!community) fail("no Daily Gaming in accounts.json");
const SERVER_ID = String(community.server_id);

const STEP_TIMEOUT_MS = 15000;

async function main() {
  const addon = require(path.join(__dirname, "..", "index.js"));
  console.log(`[stack] loaded, ${Object.keys(addon).length} exports; central ${central}, user ${USER}`);

  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "decibell-stack-"));
  const events = [];
  const waiters = [];
  const onEvent = (env) => {
    events.push(env);
    for (let i = waiters.length - 1; i >= 0; i--) {
      const w = waiters[i];
      if (w.pred(env)) {
        waiters.splice(i, 1);
        w.resolve(env);
      }
    }
  };
  // Wait for an event (already seen ones count, from index `from`).
  const waitFor = (label, pred, from = 0) => {
    const seen = events.slice(from).find(pred);
    if (seen) return Promise.resolve(seen);
    return new Promise((resolve, reject) => {
      const w = { pred, resolve };
      waiters.push(w);
      setTimeout(() => {
        const i = waiters.indexOf(w);
        if (i >= 0) {
          waiters.splice(i, 1);
          reject(new Error(`timed out waiting for ${label} (events so far: ${events.map((e) => e.name).join(", ")})`));
        }
      }, STEP_TIMEOUT_MS);
    });
  };
  const milestones = [];
  const mark = (line) => {
    milestones.push(line);
    console.log(`[stack] ${line}`);
  };

  addon.init({ userDataDir, appVersion: "0.0.0-stack-smoke" }, onEvent, () => {}, () => {});
  mark("init ok");

  // ── Sign in ───────────────────────────────────────────────────────
  let from = events.length;
  await addon.login({ username: USER, password: accounts.password });
  const loggedIn = await Promise.race([
    waitFor("login_succeeded", (e) => e.name === "login_succeeded", from),
    waitFor("login_failed", (e) => e.name === "login_failed", from).then((e) => {
      throw new Error(`login_failed: ${JSON.stringify(e.payload)}`);
    }),
  ]);
  if (loggedIn.payload.username !== USER) fail(`login_succeeded for ${loggedIn.payload.username}`);
  if (Number(loggedIn.payload.uid) !== account.uid) fail(`login uid ${loggedIn.payload.uid}, expected ${account.uid}`);
  mark(`login_succeeded username=${loggedIn.payload.username} uid=${Number(loggedIn.payload.uid)}`);
  const memberships = await waitFor("memberships_received", (e) => e.name === "memberships_received", from);
  const names = memberships.payload.memberships.map((m) => m.name).sort();
  mark(`memberships_received [${names.join(", ")}]`);
  if (!names.includes("Daily Gaming")) fail("Daily Gaming missing from memberships");

  // What the renderer asks for right after login_succeeded.
  from = events.length;
  await addon.requestServerList();
  const list = await waitFor("server_list_received", (e) => e.name === "server_list_received", from);
  mark(`server_list_received [${list.payload.servers.map((s) => s.name).sort().join(", ")}]`);
  from = events.length;
  await addon.requestDmConversations();
  await waitFor("dm_conversations_received", (e) => e.name === "dm_conversations_received", from);
  mark("dm_conversations_received");

  // ── Daily Gaming: native connects to every membership on its own ─────
  const auth = await waitFor(
    "community_auth_responded (Daily Gaming)",
    (e) => e.name === "community_auth_responded" && e.payload.serverId === SERVER_ID,
  );
  if (!auth.payload.success) fail(`community auth refused: ${JSON.stringify(auth.payload)}`);
  const channels = auth.payload.channels;
  mark(
    `community_auth_responded server=${auth.payload.serverName} success=${auth.payload.success} ` +
      `channels=${channels.length} owner=${auth.payload.ownerUsername}`,
  );
  const target = await addon.getAttachmentTarget({ serverId: SERVER_ID });
  if (!target || target.port !== community.attachment_port) fail(`attachment target ${JSON.stringify(target && { ...target, jwt: "…" })}`);
  mark(`get_attachment_target port=${target.port} jwt=${target.jwt ? "set" : "empty"} certFingerprint=${target.certFingerprint === community.cert_sha256 ? "matches" : "DIFFERS"}`);
  from = events.length;
  await addon.listMembers({ serverId: SERVER_ID, after: "", limit: 100 });
  const members = await waitFor(
    "member_list_received",
    (e) => e.name === "member_list_received" && e.payload.serverId === SERVER_ID,
    from,
  );
  mark(`member_list_received online+offline page of ${members.payload.members.length}`);

  // ── History: the newest page of #general ──────────────────────────
  const general = channels.find((c) => c.name === "general");
  const offTopic = channels.find((c) => c.name === "off-topic");
  if (!general || !offTopic) fail(`channels: ${channels.map((c) => c.name).join(", ")}`);
  from = events.length;
  await addon.requestChannelHistory({ serverId: SERVER_ID, channelId: general.id, beforeId: 0, limit: 50 });
  const hist = await waitFor(
    "channel_history_received (#general)",
    (e) => e.name === "channel_history_received" && e.payload.channelId === general.id,
    from,
  );
  if (hist.payload.messages.length !== 50) fail(`#general page has ${hist.payload.messages.length} messages`);
  mark(`channel_history_received #general messages=${hist.payload.messages.length} hasMore=${hist.payload.hasMore} beforeId=${Number(hist.payload.beforeId)}`);

  // ── Send one message to #off-topic and see its echo ───────────────
  const nonce = `stack-smoke-${process.pid}-${Date.now()}`;
  const text = `stack smoke (${new Date().toISOString()})`;
  from = events.length;
  await addon.sendChannelMessage({ serverId: SERVER_ID, channelId: offTopic.id, message: text, nonce });
  const echo = await waitFor(
    "message_received echo",
    (e) => e.name === "message_received" && e.payload.nonce === nonce,
    from,
  );
  if (echo.payload.content !== text || echo.payload.sender !== USER) fail(`echo ${JSON.stringify(echo.payload)}`);
  mark(`message_received echo context=${echo.payload.context} sender=${echo.payload.sender} content=matches id>0=${Number(echo.payload.id) > 0}`);

  // ── Friends ───────────────────────────────────────────────────────
  from = events.length;
  await addon.requestFriendList();
  const friends = await waitFor("friend_list_received", (e) => e.name === "friend_list_received", from);
  mark(`friend_list_received friends=${friends.payload.friends.length}`);

  // ── Refusals: the error text and code reach JS as before ─────────
  const refused = async (label, fn) => {
    try {
      await fn();
    } catch (e) {
      mark(`${label} rejected code=${e.code} message=${JSON.stringify(e.message)}`);
      return;
    }
    fail(`${label} did not reject`);
  };
  await refused("request_channel_history(unknown server)", () =>
    addon.requestChannelHistory({ serverId: "999", channelId: "x", beforeId: 0, limit: 1 }),
  );
  await refused("set_status(bogus)", () => addon.setStatus({ status: "bogus" }));
  await refused("parse_invite_link(http)", () => addon.parseInviteLink({ url: "http://example.test/x" }));
  await refused("voice_mod(bogus)", () => addon.voiceMod({ serverId: SERVER_ID, uid: 1, action: "bogus" }));
  const parsed = addon.parseInviteLink({ url: "decibell://invite/127.0.0.11:8082/abc" });
  mark(`parse_invite_link ${JSON.stringify(parsed)}`);

  // ── Sign out, shut down ───────────────────────────────────────────
  from = events.length;
  await addon.logout();
  const out = await waitFor("logged_out", (e) => e.name === "logged_out", from);
  mark(`logged_out ${JSON.stringify(out.payload)}`);
  await addon.shutdown();
  mark("shutdown ok");

  const counts = new Map();
  for (const e of events) counts.set(e.name, (counts.get(e.name) || 0) + 1);
  console.log("[stack] event names (count):");
  for (const name of [...counts.keys()].sort()) console.log(`[stack]   ${name} ×${counts.get(name)}`);
  fs.rmSync(userDataDir, { recursive: true, force: true });
  console.log("[stack] PASS");
  process.exit(0);
}

main().catch((e) => fail(e && e.stack ? e.stack : String(e)));
