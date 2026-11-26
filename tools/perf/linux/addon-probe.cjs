// Used by electron-local.sh probe, inside an empty network namespace (unshare -rn): loads the built napi addon
// outside Electron, inits it with a throwaway user-data dir and asks it to sign in as a junk user, so it opens its
// central connection to wherever it resolves the central to. The caller listens on 127.0.0.1:18080 and passes when
// the connection arrives there. Nothing can leave the machine from that namespace.
//   node addon-probe.cjs <addon.node> <tmp dir>
"use strict";
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");
const [, , addonPath, tmp] = process.argv;
const mod = { exports: {} };
process.dlopen(mod, path.resolve(addonPath), os.constants.dlopen.RTLD_NOW);
const addon = mod.exports;
const userDataDir = fs.mkdtempSync(path.join(tmp, "ud-"));
addon.init({ userDataDir, appVersion: "0.0.0-probe" }, () => {}, () => {}, () => {});
console.log(`probe: DECIBELL_CENTRAL=${process.env.DECIBELL_CENTRAL}; signing in as a junk user`);
addon
  .login({ username: "probe", password: "probe" })
  .then(() => console.log("probe: login request sent"))
  .catch((e) => console.log(`probe: login failed: ${e && e.message ? e.message : e}`))
  .finally(() => setTimeout(() => process.exit(0), 500));
setTimeout(() => process.exit(0), 15000);
