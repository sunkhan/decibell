// Electron side of the video comparison (qt-client spike (a)): plays the same .dbv test streams (qt-client/tools/
// streamgen) the way the Electron client plays a watched stream. The main process feeds whole encoded frames to
// the renderer over IPC at the stream's frame rate (the client: native addon -> main -> webContents.send), and the
// renderer runs StreamVideoPlayer's path: WebCodecs VideoDecoder (prefer-hardware, optimizeForLatency) and
// drawImage into a canvas capped at the on-screen size.
//
//   electron.exe tools/perf/electron-video --files=a.dbv[,b.dbv] [--seconds=8] [--out=result.json]
//
// Chromium switches: the client's Windows set (electron-client/electron/main/index.ts).
const { app, BrowserWindow, ipcMain } = require('electron');
const fs = require('fs');
const path = require('path');

const arg = (name, dflt) => {
  const a = process.argv.find((x) => x.startsWith(`--${name}=`));
  return a ? a.slice(name.length + 3) : dflt;
};
const files = arg('files', '').split(',').filter(Boolean);
const seconds = Number(arg('seconds', '8'));
const out = arg('out', '');

app.commandLine.appendSwitch('enable-features', [
  'PlatformHEVCEncoderSupport', 'PlatformHEVCDecoderSupport', 'D3D11VideoDecoder', 'D3D11VideoDecoderUseSharedHandle',
  'MediaFoundationClearPlayback', 'UseGpuMemoryBufferVideoFrames'].join(','));
app.commandLine.appendSwitch('ignore-gpu-blocklist');
app.commandLine.appendSwitch('enable-accelerated-video-decode');
app.commandLine.appendSwitch('disable-features', 'AudioServiceSandbox');
app.commandLine.appendSwitch('audio-service-sandbox-type', 'none');
app.commandLine.appendSwitch('no-sandbox');

// .dbv: "DBV1", u8 codec (1 H.264, 2 HEVC, 3 AV1), 3 zero bytes, u32 width, height, fps; then u32 size, u32 flags,
// bytes. Read lazily (a file handle, not the whole file) like the Qt side.
function openStream(file) {
  const fd = fs.openSync(file, 'r');
  const head = Buffer.alloc(20);
  fs.readSync(fd, head, 0, 20, 0);
  if (head.toString('latin1', 0, 4) !== 'DBV1') throw new Error(`${file}: not a .dbv`);
  const s = { fd, codec: head[4], width: head.readUInt32LE(8), height: head.readUInt32LE(12), fps: head.readUInt32LE(16), pos: 20 };
  s.next = () => {
    const h = Buffer.alloc(8);
    if (fs.readSync(fd, h, 0, 8, s.pos) < 8) { s.pos = 20; fs.readSync(fd, h, 0, 8, s.pos); }
    const size = h.readUInt32LE(0), key = (h.readUInt32LE(4) & 1) === 1;
    const data = new Uint8Array(size);
    fs.readSync(fd, data, 0, size, s.pos + 8);
    s.pos += 8 + size;
    return { data, key };
  };
  return s;
}

app.whenReady().then(() => {
  const win = new BrowserWindow({
    // The client area is 1280 x 800, as the Qt window's.
    width: 1280, height: 800, useContentSize: true, backgroundColor: '#000000', autoHideMenuBar: true,
    webPreferences: { nodeIntegration: true, contextIsolation: false, backgroundThrottling: false },
  });
  win.loadFile(path.join(__dirname, 'index.html'));
  const streams = files.map(openStream);
  win.webContents.once('did-finish-load', () => {
    win.webContents.send('streams', streams.map((s) => ({ codec: s.codec, width: s.width, height: s.height, fps: s.fps })));
    setTimeout(() => win.webContents.send('bench-start'), 2000);
    setTimeout(() => win.webContents.send('bench-stop'), 2000 + seconds * 1000);
  });
  // The renderer paces each stream and asks for the next frame; it arrives over webContents.send, as a frame from
  // the native addon does. (Pacing here instead fails: Node timers in the main process tick at Windows' 15.6 ms
  // timer granularity, 77 frames a second for a 120 fps stream; Chromium's renderer timers are high-resolution.)
  ipcMain.on('want-frame', (_e, index, fedAt) => {
    if (quitting || win.isDestroyed()) return;
    const f = streams[index].next();
    win.webContents.send('frame', { index, data: f.data, key: f.key, fedAt });
  });
  ipcMain.on('bench-report', (_e, report) => {
    if (out) fs.appendFileSync(out, JSON.stringify(report) + '\n');
    quit();
  });
  // Never outlive the run, whatever happens to the report (a run left behind keeps decoding a 120 fps stream).
  setTimeout(quit, (seconds + 15) * 1000);
});

// app.exit() from the IPC handler left the processes running (seen on Windows): close the window, then end the
// process outright. A frame request still in flight then met a destroyed window, and the uncaught "Object has been
// destroyed" put up Electron's error dialog, which kept the process alive: requests are ignored once quitting, and
// nothing in this harness may ever open a dialog.
let quitting = false;
process.on('uncaughtException', (e) => {
  if (out) fs.appendFileSync(out + '.errors', `${(e && e.stack) || e}\n`);
  quit();
});
function quit() {
  quitting = true;
  for (const w of BrowserWindow.getAllWindows()) if (!w.isDestroyed()) w.destroy();
  app.exit(0);
  setTimeout(() => process.exit(0), 500);
}
