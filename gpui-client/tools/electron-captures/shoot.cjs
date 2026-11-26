// Captures every page × theme of the built preview (.work/dist) with one offscreen window.
//
//   electron-client/node_modules/.bin/electron shoot.cjs --user-data-dir=<.work/profile> \
//       [--pages a,b] [--themes t1,t2] [--out <dir>] [--query k=v]...
//
// Writes <out>/<page>-<theme>.png (the viewport only: 1280 × 768 unless the page exports `size`)
// and <out>/<page>-<theme>.json when the page's setup() returned something.
// Env: CAPTURE_DPR (default 1), CAPTURE_TIMEOUT_S (whole run, default 300),
//      CAPTURE_GRAYSCALE_TEXT=1 (grayscale instead of LCD-subpixel text).
"use strict";
const { app, BrowserWindow, ipcMain, protocol, net } = require("electron");
const fs = require("node:fs");
const path = require("node:path");

const HERE = __dirname;
const WORK = path.join(HERE, ".work");
const DIST = path.join(WORK, "dist", "index.html");
const THEMES = ["nocturne", "matinee", "graphite", "graphite-light", "console", "console-light", "console-split"];
const DEFAULT_OUT = path.resolve(HERE, "../../target/electron-captures");

function parseArgs(argv) {
  const o = { pages: null, themes: THEMES, out: DEFAULT_OUT, query: {} };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => {
      const eq = a.indexOf("=");
      return eq >= 0 ? a.slice(eq + 1) : argv[++i];
    };
    if (a.startsWith("--pages")) o.pages = val().split(",").filter(Boolean);
    else if (a.startsWith("--themes")) o.themes = val().split(",").filter(Boolean);
    else if (a.startsWith("--out")) o.out = path.resolve(val());
    else if (a.startsWith("--query")) {
      const [k, ...v] = val().split("=");
      o.query[k] = v.join("=");
    }
  }
  return o;
}

// Everything after the script path (Chromium switches like --user-data-dir are ignored here).
const scriptIdx = process.argv.findIndex((a) => a.endsWith("shoot.cjs"));
const opts = parseArgs(process.argv.slice(scriptIdx + 1));
for (const t of opts.themes) if (!THEMES.includes(t)) console.warn(`[shoot] unknown theme "${t}"`);

const dpr = process.env.CAPTURE_DPR || "1";
app.commandLine.appendSwitch("force-device-scale-factor", dpr);
// en-US as the UI language (`navigator.language`). `Intl`'s default locale is ICU's, from the
// environment: run.sh pins it (LC_ALL=en_US.UTF-8), as the GPUI captures pin `Regional::en_us()`.
app.commandLine.appendSwitch("lang", "en-US");
// Text is LCD-subpixel antialiased by default (fontconfig's rgba, as in the real app on this
// machine); CAPTURE_GRAYSCALE_TEXT=1 renders grayscale text, for diffs against grayscale GPUI text.
if (process.env.CAPTURE_GRAYSCALE_TEXT === "1") app.commandLine.appendSwitch("disable-lcd-text");
// Never the app's profile: a throwaway one under the gitignored work dir.
const profile = path.join(WORK, "profile");
app.setPath("userData", profile);
app.setPath("sessionData", profile);

// The content pages' pictures (`decibell-content-fixture` writes them to .work/content-files):
// the renderer's `decibell-attachment://attach/<server>/<id>[?variant=thumb&size=N]` URLs and the
// fixture's https picture URLs answer from there, as the app's protocol and the web would. An id
// the manifest calls "hold" never answers (the placeholder stays), "fail" answers 404. A GIF is
// its first frame alone, so the shot doesn't depend on when it was taken.
const CONTENT = path.join(WORK, "content-files");
protocol.registerSchemesAsPrivileged([
  { scheme: "decibell-attachment", privileges: { standard: true, secure: true, supportFetchAPI: true, corsEnabled: true, stream: true } },
]);
function contentManifest() {
  try {
    return JSON.parse(fs.readFileSync(path.join(CONTENT, "manifest.json"), "utf8"));
  } catch {
    return null;
  }
}
function pictureResponse(name) {
  const bytes = fs.readFileSync(path.join(CONTENT, name));
  const type = bytes[0] === 0x89 ? "image/png" : bytes[0] === 0x47 ? "image/gif" : bytes[0] === 0xff ? "image/jpeg" : "application/octet-stream";
  return new Response(bytes, { status: 200, headers: { "content-type": type, "access-control-allow-origin": "*" } });
}
function installContentProtocols() {
  const manifest = contentManifest();
  protocol.handle("decibell-attachment", (req) => {
    const url = new URL(req.url);
    const id = url.pathname.split("/").filter(Boolean).pop() ?? "";
    const state = manifest?.attachments?.[id];
    // Headers, then a body that never comes: the picture stays loading (a handler that never
    // resolves is failed by Electron instead).
    if (state === "hold") return new Response(new ReadableStream({ start() {} }), { status: 200, headers: { "content-type": "image/jpeg" } });
    if (state !== "ok") return new Response("not found", { status: 404 });
    const thumb = url.searchParams.get("variant") === "thumb" ? url.searchParams.get("size") ?? "640" : "0";
    return pictureResponse(`att-${id}-${thumb}`);
  });
  if (!manifest) return;
  protocol.handle("https", (req) => {
    const name = manifest.remote?.[req.url];
    if (name) return pictureResponse(name);
    // Nothing else of the web is reached from a capture.
    return new Response("not found", { status: 404 });
  });
}

// Electron 33 logs a harmless "No component available" rejection at startup.
process.on("unhandledRejection", (e) => console.warn("[shoot] unhandled rejection (ignored):", String(e?.message ?? e)));
app.on("window-all-closed", () => {}); // never quit on our own window's close

const totalTimeout = Number(process.env.CAPTURE_TIMEOUT_S || 300) * 1000;
setTimeout(() => {
  console.error(`[shoot] timed out after ${totalTimeout / 1000} s`);
  app.exit(3);
}, totalTimeout);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const withTimeout = (p, ms, what) =>
  Promise.race([p, sleep(ms).then(() => Promise.reject(new Error(`${what}: timed out after ${ms} ms`)))]);

ipcMain.handle("harness:input", async (e, events) => {
  for (const ev of events) {
    if (ev.type === "wait") await sleep(ev.ms);
    else e.sender.sendInputEvent(ev);
  }
  await sleep(16);
});

async function load(win, query) {
  let lastErr;
  for (let attempt = 1; attempt <= 4; attempt++) {
    try {
      await win.loadFile(DIST, { query });
      return;
    } catch (e) {
      lastErr = e; // ERR_FAILED (-2) / ERR_ABORTED (-3) on a busy or fresh window
      await sleep(200 * attempt);
    }
  }
  throw lastErr;
}

// Waits for the next painted frame after a forced invalidate, so capturePage sees the final state.
function nextPaint(win, ms = 500) {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(t);
      win.webContents.off("paint", done);
      resolve();
    };
    const t = setTimeout(done, ms);
    win.webContents.on("paint", done);
    win.webContents.invalidate();
  });
}

async function main() {
  await app.whenReady();
  installContentProtocols();
  if (!fs.existsSync(DIST)) throw new Error(`${DIST} is missing: run build.sh first`);
  fs.mkdirSync(opts.out, { recursive: true });

  const win = new BrowserWindow({
    show: false,
    width: 1280,
    height: 768,
    useContentSize: true,
    frame: false,
    webPreferences: {
      offscreen: true,
      backgroundThrottling: false,
      contextIsolation: true,
      sandbox: true,
      preload: path.join(HERE, "preload.cjs"),
    },
  });
  win.webContents.setFrameRate(60);
  win.webContents.on("console-message", (_e, level, message) => {
    if (level >= 2) console.log(`[page] ${message}`);
  });

  // Page list: what the build knows (default), filtered by --pages.
  await load(win, { page: "", theme: "nocturne" });
  const known = await win.webContents.executeJavaScript("window.__pages || []");
  const pages = opts.pages ?? known;
  for (const p of pages) if (!known.includes(p)) console.warn(`[shoot] page "${p}" is not in the build (have: ${known.join(", ")})`);

  let failures = 0;
  for (const page of pages) {
    for (const theme of opts.themes) {
      const name = `${page}-${theme}`;
      try {
        const size = await sizeOf(win, page);
        win.setContentSize(size.width, size.height);
        await load(win, { page, theme, ...opts.query });
        const report = await withTimeout(
          win.webContents.executeJavaScript(
            "window.__ready ? window.__ready.then((r) => JSON.stringify(r)) : Promise.reject(new Error('no window.__ready: the bundle did not run'))",
          ),
          30_000,
          name,
        );
        await nextPaint(win);
        const img = await win.webContents.capturePage();
        const got = img.getSize();
        const want = { width: Math.round(size.width * dpr), height: Math.round(size.height * dpr) };
        if (got.width !== want.width || got.height !== want.height)
          console.warn(`[shoot] ${name}: captured ${got.width}×${got.height}, expected ${want.width}×${want.height}`);
        fs.writeFileSync(path.join(opts.out, `${name}.png`), img.toPNG());
        if (report && report !== "null") fs.writeFileSync(path.join(opts.out, `${name}.json`), JSON.stringify(JSON.parse(report), null, 2) + "\n");
        console.log(`[shoot] ${name}.png ${got.width}×${got.height}`);
      } catch (e) {
        failures++;
        console.error(`[shoot] ${name} FAILED: ${e?.message ?? e}`);
      }
    }
  }
  win.destroy();
  return failures;
}

// A page's `size` export, read from its module without mounting it (cached per page).
const sizes = new Map();
async function sizeOf(win, page) {
  if (sizes.has(page)) return sizes.get(page);
  let size = { width: 1280, height: 768 };
  await load(win, { page: "", theme: "nocturne" });
  const s = await win.webContents.executeJavaScript(`window.__pageSize(${JSON.stringify(page)})`);
  if (s && s.width && s.height) size = s;
  sizes.set(page, size);
  return size;
}

main().then(
  (failures) => {
    console.log(failures ? `[shoot] done, ${failures} failed` : `[shoot] done → ${opts.out}`);
    app.exit(failures ? 1 : 0);
  },
  (e) => {
    console.error("[shoot] fatal:", e?.stack ?? e);
    app.exit(2);
  },
);
