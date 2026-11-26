// Electron main process of the decibell-tex reference harness. Configuration comes from the
// environment (run.sh sets it): HARNESS_CORPUS, HARNESS_OUT, HARNESS_DSF (default 2),
// HARNESS_FILTER (regex on ids), HARNESS_BATCH (default 100), KATEX_DIST (default: the
// repo's electron-client/node_modules/katex/dist).
"use strict";
const { app, BrowserWindow, session } = require("electron");
const fs = require("fs");
const path = require("path");
const zlib = require("zlib");
const { pathToFileURL } = require("url");

const env = process.env;
const corpusPath = env.HARNESS_CORPUS;
const outPath = env.HARNESS_OUT;
const dsf = env.HARNESS_DSF || "2";
const filter = env.HARNESS_FILTER ? new RegExp(env.HARNESS_FILTER) : null;
const batchSize = Number(env.HARNESS_BATCH || 100);
const katexDist = env.KATEX_DIST || path.resolve(__dirname, "../../../../../electron-client/node_modules/katex/dist");

// The castlabs Electron build rejects a promise for its (absent) CDM component at startup
// ("No component available"); it has nothing to do with the harness.
process.on("unhandledRejection", (e) => {
    if (!/No component available/.test(String(e && e.message || e))) console.error("harness: unhandled rejection", e);
});

app.commandLine.appendSwitch("force-device-scale-factor", dsf);
app.commandLine.appendSwitch("disable-gpu");
app.disableHardwareAcceleration();

function fail(msg) {
    console.error(`harness: ${msg}`);
    app.exit(1);
}

app.whenReady().then(async () => {
    try {
        if (!corpusPath || !outPath) return fail("HARNESS_CORPUS and HARNESS_OUT are required");
        const ses = session.fromPartition("harness"); // in-memory partition
        ses.webRequest.onBeforeRequest((details, cb) => {
            const ok = details.url.startsWith("file:") || details.url.startsWith("data:") || details.url.startsWith("devtools:");
            cb({ cancel: !ok });
        });
        const win = new BrowserWindow({
            show: false, width: 1280, height: 800,
            webPreferences: { session: ses, backgroundThrottling: false, contextIsolation: true, nodeIntegration: false, sandbox: true, spellcheck: false },
        });
        await win.loadFile(path.join(__dirname, "page.html"));
        const css = pathToFileURL(path.join(katexDist, "katex.css")).href;
        const js = pathToFileURL(path.join(katexDist, "katex.js")).href;
        await win.webContents.executeJavaScript(`new Promise((res, rej) => {
            const l = document.createElement("link"); l.rel = "stylesheet"; l.href = ${JSON.stringify(css)};
            l.onload = () => { const s = document.createElement("script"); s.src = ${JSON.stringify(js)}; s.onload = res; s.onerror = rej; document.head.appendChild(s); };
            l.onerror = rej; document.head.appendChild(l);
        })`);
        const info = await win.webContents.executeJavaScript("harness.init()");
        console.error(`harness: dpr=${info.dpr} faces loaded=${info.loaded} ${info.ua}`);
        if (info.loaded < 20) return fail(`only ${info.loaded} KaTeX faces loaded`);
        const version = await win.webContents.executeJavaScript("katex.version");

        let corpus = fs.readFileSync(corpusPath, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
        if (filter) corpus = corpus.filter((e) => filter.test(e.id));
        const lines = [JSON.stringify({ meta: { katex: version, dpr: info.dpr, ua: info.ua, hostFontPx: 16, inlineWidth: 20000, displayWidth: 1000, count: corpus.length, calibration: info.calibration } })];
        const t0 = Date.now();
        for (let i = 0; i < corpus.length; i += batchSize) {
            const batch = corpus.slice(i, i + batchSize).map(({ id, tex, display }) => ({ id, tex, display }));
            const results = await win.webContents.executeJavaScript(`harness.run(${JSON.stringify(batch)}, 0)`);
            for (const r of results) lines.push(JSON.stringify(r));
            if ((i / batchSize) % 10 === 0) console.error(`harness: ${Math.min(i + batchSize, corpus.length)}/${corpus.length}`);
        }
        fs.mkdirSync(path.dirname(path.resolve(outPath)), { recursive: true });
        const body = lines.join("\n") + "\n";
        fs.writeFileSync(outPath, outPath.endsWith(".gz") ? zlib.gzipSync(body, { level: 9 }) : body);
        console.error(`harness: wrote ${corpus.length} records to ${outPath} in ${((Date.now() - t0) / 1000).toFixed(1)} s`);
        win.destroy();
        app.exit(0);
    } catch (e) {
        fail(e && e.stack || String(e));
    }
});
