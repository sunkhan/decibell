// The Chromium side of decibell-text's fidelity harness (see ../README.md).
//
// Lays a corpus out in a hidden Electron window with the client's fonts and the renderer's
// message-body CSS, and writes what Chromium did: line starts, line widths, line tops and
// the paragraph height per case. With --shots it also saves a PNG per case.
//
//   electron main.js --corpus corpus.json --out chromium.json [--dsf 1] [--shots dir] [--page file.html]
//
// Nothing here touches the Decibell app, its profile or the network: run.sh starts it with a
// throwaway --user-data-dir.

const { app, BrowserWindow, ipcMain } = require("electron");
const fs = require("fs");
const path = require("path");

function arg(name, fallback) {
  const at = process.argv.indexOf(`--${name}`);
  return at >= 0 ? process.argv[at + 1] : fallback;
}

const corpusPath = arg("corpus");
const outPath = arg("out");
const shotsDir = arg("shots", null);
const dsf = arg("dsf", "1");
const page = arg("page", "page.html");
if (!corpusPath || !outPath) {
  console.error("usage: electron main.js --corpus <json> --out <json> [--dsf n] [--shots dir]");
  process.exit(2);
}

app.commandLine.appendSwitch("force-device-scale-factor", dsf);
app.commandLine.appendSwitch("disable-http-cache");
// A hidden window must still lay out and paint at full speed.
app.commandLine.appendSwitch("disable-renderer-backgrounding");
app.commandLine.appendSwitch("disable-background-timer-throttling");

app.whenReady().then(async () => {
  const corpus = JSON.parse(fs.readFileSync(corpusPath, "utf8"));
  const win = new BrowserWindow({
    width: 1100,
    height: 900,
    show: !!shotsDir,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      backgroundThrottling: false,
    },
  });
  win.webContents.on("console-message", (_e, _level, message) => console.error("page:", message));

  ipcMain.handle("corpus", () => ({
    corpus,
    fonts: path.resolve(__dirname, "../../../../assets/fonts/static"),
    shots: !!shotsDir,
  }));
  ipcMain.handle("shot", async (_e, name, rect) => {
    const image = await win.webContents.capturePage(rect);
    fs.mkdirSync(shotsDir, { recursive: true });
    fs.writeFileSync(path.join(shotsDir, `${name}@${dsf}x.png`), image.toPNG());
  });
  ipcMain.handle("done", (_e, result) => {
    fs.writeFileSync(outPath, JSON.stringify(result));
    app.quit();
  });
  await win.loadFile(path.join(__dirname, page));
});

app.on("window-all-closed", () => app.quit());
