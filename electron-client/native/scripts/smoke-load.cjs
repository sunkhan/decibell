#!/usr/bin/env node
// Smoke test for a built addon (index.js → index.<platform>.node), outside
// Electron: it loads, answers ping / pingAsync, takes `init(...)` with a temp
// userData dir and the three buses (as electron/main/addon.ts passes them),
// delivers an event through the JSON bus with the `{name, payload}` envelope,
// and shuts down.
//
// The event: `logout` with nobody signed in runs the local sign-out
// (decibell-core api::auth::sign_out_locally), which emits `logged_out`
// {reason: null} and `e2ee_status_changed`. It never reaches the network:
// central's "forget this device" is refused locally ("Not signed in"), and
// DECIBELL_CENTRAL points at a closed port anyway (debug builds honour it).
// No login is attempted. It also toggles a stream-frame sink on and off (the
// sink-removed hook into the napi sink).
//
//   cd electron-client/native && node scripts/smoke-load.cjs
//
// Exit 0 on success; 1 with a message otherwise.

"use strict";

process.env.DECIBELL_CENTRAL = "127.0.0.1:1";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const TIMEOUT_MS = 5000;

function fail(msg) {
  console.error(`[smoke] FAIL: ${msg}`);
  process.exit(1);
}

async function main() {
  const addon = require(path.join(__dirname, "..", "index.js"));
  const exports = Object.keys(addon);
  console.log(`[smoke] loaded, ${exports.length} exports`);

  if (addon.ping() !== "pong") fail("ping() did not return 'pong'");
  if ((await addon.pingAsync()) !== "pong-async") fail("pingAsync() did not return 'pong-async'");
  console.log("[smoke] ping / pingAsync ok");

  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "decibell-smoke-"));
  const events = [];
  let resolveFirst;
  const firstLoggedOut = new Promise((r) => {
    resolveFirst = r;
  });

  addon.init(
    // e2eeLocalKey / localKeyProtected left out: napi-rs 2 reads an absent
    // (undefined) Option field as None but refuses null for it.
    {
      userDataDir,
      appVersion: "0.0.0-smoke",
    },
    (env) => {
      events.push(env);
      if (env && env.name === "logged_out") resolveFirst(env);
    },
    (_frame) => {},
    (_thumb) => {},
  );
  console.log(`[smoke] init ok (userDataDir ${userDataDir})`);

  addon.setStreamFrameSink({ key: "1", enabled: true });
  addon.setStreamFrameSink({ key: "1", enabled: false });
  addon.clearStreamFrameSinks();

  await addon.logout();
  const timeout = new Promise((r) => setTimeout(() => r(null), TIMEOUT_MS));
  const env = await Promise.race([firstLoggedOut, timeout]);
  if (!env) fail(`no logged_out event within ${TIMEOUT_MS} ms (got: ${JSON.stringify(events)})`);

  const keys = Object.keys(env).sort().join(",");
  if (keys !== "name,payload") fail(`envelope keys are ${keys}, expected name,payload`);
  if (JSON.stringify(env.payload) !== JSON.stringify({ reason: null })) {
    fail(`logged_out payload ${JSON.stringify(env.payload)}, expected {"reason":null}`);
  }
  // Let the second emit (same sign-out) arrive too.
  await new Promise((r) => setTimeout(r, 100));
  for (const e of events) console.log(`[smoke] event ${JSON.stringify(e)}`);
  if (!events.some((e) => e.name === "e2ee_status_changed")) fail("no e2ee_status_changed event");

  await addon.shutdown();
  console.log("[smoke] shutdown ok");
  fs.rmSync(userDataDir, { recursive: true, force: true });
  console.log("[smoke] PASS");
  process.exit(0);
}

main().catch((e) => fail(e && e.stack ? e.stack : String(e)));
