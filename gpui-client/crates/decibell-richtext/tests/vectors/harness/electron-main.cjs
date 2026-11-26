// electron electron-main.cjs <bundle.js> <job.json> <out.json>
// Runs the harness bundle in a hidden renderer: the reference is what Electron's renderer computes
// (Blink's URL parser, V8's regex engine, a real DOM for RichInput), not Node.
const { app, BrowserWindow } = require("electron");
const fs = require("fs");

const [bundlePath, jobPath, outPath] = process.argv.slice(-3);
app.disableHardwareAcceleration();
app.whenReady().then(async () => {
  const w = new BrowserWindow({ show: false, webPreferences: { offscreen: true, contextIsolation: false } });
  await w.loadURL("data:text/html,<!doctype html><html><body></body></html>");
  const bundle = fs.readFileSync(bundlePath, "utf8");
  const job = fs.readFileSync(jobPath, "utf8");
  try {
    await w.webContents.executeJavaScript(bundle + "\n;0");
    const res = await w.webContents.executeJavaScript(
      `(() => { const job = ${job}; return JSON.stringify(job.sweep ? { sweep: __hostSweep() } : __runSuites(job.suites)); })()`,
    );
    fs.writeFileSync(outPath, res);
    app.exit(0);
  } catch (e) {
    console.error("harness error:", e);
    app.exit(3);
  }
});
setTimeout(() => {
  console.error("harness timeout");
  app.exit(2);
}, 30 * 60 * 1000);
