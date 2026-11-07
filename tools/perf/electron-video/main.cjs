// Electron side of the video comparison (qt-client spike (a)): plays the same .dbv streams the Qt client plays, the
// way the Electron client plays a watched stream. The main process feeds whole encoded frames to the renderer over
// IPC (the client: native addon -> main -> webContents.send), and the renderer runs StreamVideoPlayer's path:
// WebCodecs VideoDecoder (prefer-hardware, optimizeForLatency) and drawImage into a canvas capped at the on-screen
// size.
//
//   electron.exe tools/perf/electron-video --files=a.dbv[,b.dbv] [--seconds=8] [--out=result.json] [--maximized]
//
// .dbv (qt-client/src/video/streamfile.h): DBV1 synthetic streams (tools/streamgen) play at their fps; DBV2 real
// streams recorded by the client's debug build (DECIBELL_STREAM_DUMP) play on their recorded arrival times, with the
// decoder configuration records they carried.
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
const maximized = process.argv.includes('--maximized');

app.commandLine.appendSwitch('enable-features', [
  'PlatformHEVCEncoderSupport', 'PlatformHEVCDecoderSupport', 'D3D11VideoDecoder', 'D3D11VideoDecoderUseSharedHandle',
  'MediaFoundationClearPlayback', 'UseGpuMemoryBufferVideoFrames'].join(','));
app.commandLine.appendSwitch('ignore-gpu-blocklist');
app.commandLine.appendSwitch('enable-accelerated-video-decode');
app.commandLine.appendSwitch('disable-features', 'AudioServiceSandbox');
app.commandLine.appendSwitch('audio-service-sandbox-type', 'none');
app.commandLine.appendSwitch('no-sandbox');

// Read lazily (a file handle, not the whole file) like the Qt side. `kind`: 'h264' | 'hevc' | 'av1'.
function openStream(file) {
  const fd = fs.openSync(file, 'r');
  const head = Buffer.alloc(20);
  fs.readSync(fd, head, 0, 20, 0);
  const magic = head.toString('latin1', 0, 4);
  if (magic !== 'DBV1' && magic !== 'DBV2') throw new Error(`${file}: not a .dbv`);
  const version = magic === 'DBV2' ? 2 : 1;
  const c = head[4];
  const kind = version === 1 ? ['', 'h264', 'hevc', 'av1'][c] : c === 3 ? 'hevc' : c === 4 ? 'av1' : 'h264';
  const s = { fd, version, kind, width: head.readUInt32LE(8), height: head.readUInt32LE(12), fps: head.readUInt32LE(16), pos: 20 };
  const header = () => {
    const h = Buffer.alloc(version === 2 ? 16 : 8);
    if (fs.readSync(fd, h, 0, h.length, s.pos) < h.length) return null;
    const flags = h.readUInt32LE(4);
    const f = { size: h.readUInt32LE(0), key: (flags & 1) === 1, atMs: -1, description: null, wrapped: false };
    s.pos += h.length;
    if (version === 2) {
      f.atMs = Number(h.readBigUInt64LE(8)) / 1000;
      if (flags & 2) {
        const n = Buffer.alloc(4);
        fs.readSync(fd, n, 0, 4, s.pos);
        const len = n.readUInt32LE(0);
        f.description = new Uint8Array(len);
        fs.readSync(fd, f.description, 0, len, s.pos + 4);
        s.pos += 4 + len;
      }
    }
    return f;
  };
  s.next = () => {
    let f = header();
    if (!f) {
      s.pos = 20;
      f = header();
      f.wrapped = true;
    }
    f.data = new Uint8Array(f.size);
    fs.readSync(fd, f.data, 0, f.size, s.pos);
    s.pos += f.size;
    return f;
  };
  return s;
}

app.whenReady().then(() => {
  const win = new BrowserWindow({
    // The client area is 1280 x 800, as the Qt window's.
    width: 1280, height: 800, useContentSize: true, backgroundColor: '#000000', autoHideMenuBar: true,
    webPreferences: { nodeIntegration: true, contextIsolation: false, backgroundThrottling: false },
  });
  if (maximized) win.maximize();
  win.loadFile(path.join(__dirname, 'index.html'));
  const streams = files.map(openStream);
  win.webContents.once('did-finish-load', () => {
    win.webContents.send('streams', streams.map((s) => ({ kind: s.kind, recorded: s.version === 2, width: s.width, height: s.height, fps: s.fps })));
    setTimeout(() => win.webContents.send('bench-start'), 2000);
    setTimeout(() => win.webContents.send('bench-stop'), 2000 + seconds * 1000);
  });
  // The renderer paces each stream and asks for the next frame; it arrives over webContents.send, as a frame from
  // the native addon does. (Pacing here instead fails: Node timers in the main process tick at Windows' 15.6 ms
  // timer granularity, 77 frames a second for a 120 fps stream; Chromium's renderer timers are high-resolution.)
  ipcMain.on('want-frame', (_e, index, fedAt) => {
    if (quitting || win.isDestroyed()) return;
    const f = streams[index].next();
    win.webContents.send('frame', { index, data: f.data, key: f.key, fedAt, atMs: f.atMs, wrapped: f.wrapped, description: f.description });
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
