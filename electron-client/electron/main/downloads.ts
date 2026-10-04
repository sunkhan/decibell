// Attachment download manager — the Downloads tab of the Transfers panel.
// Design: docs/superpowers/specs/2026-10-04-transfers-panel-design.md.
//
// Main owns a download end to end: it picks the destination (the
// configured folder, or a save dialog it runs itself), streams the bytes
// straight to `<final>.part` and renames on completion. Nothing crosses
// IPC but small records, and the renderer refers to downloads by id
// only — it never hands main a path to write, open or reveal.
//
// Pause aborts the fetch and keeps the .part; resume continues from its
// size with a Range request (attachment ids are immutable, so no
// validators are needed). Encrypted-channel files go through the same
// decrypt path the protocol uses, in chunk-aligned windows; the file key
// is snapshotted at start (the registry is cleared on disconnect) and
// kept wrapped by safeStorage until the download finishes, so a paused
// one can resume after a restart. Records persist in
// userData/downloads.json; on quit, active downloads become paused.
//
// The per-file download cap applies through downloadPacer, exactly as
// it does for inline media.

import { app, BrowserWindow, dialog, ipcMain, net, safeStorage, shell } from "electron";
import * as fs from "node:fs";
import * as fsp from "node:fs/promises";
import * as path from "node:path";
import { randomUUID } from "node:crypto";
import { getAttachmentTarget } from "./attachmentRegistry";
import { getAttachmentKey } from "./attachmentKeys";
import type { AttachmentKeyInfo } from "./attachmentCrypto";
import { fetchDecryptedWith } from "./attachmentFetch";
import { downloadRateBps, pacedBody } from "./downloadPacer";

const PART = ".part";
const MAX_RECORDS = 100;
const EMIT_INTERVAL_MS = 250;
const SPEED_SAMPLE_MS = 500;
/// Encrypted files are fetched in windows of whole sealed chunks: this
/// many uncapped (the decrypt path's own ceiling), about half a second
/// of the cap otherwise so progress keeps moving.
const SEALED_WINDOW_BYTES = 4 * 1024 * 1024;

export type DownloadState = "active" | "paused" | "done" | "failed" | "cancelled";

export interface DownloadContext {
  serverName: string;
  channelId: string;
  channelName: string;
  messageId: number;
  sender: string;
}

interface DownloadRecord {
  id: string;
  serverId: string;
  attachmentId: number;
  /// Display name: the final file's basename.
  filename: string;
  /// Final path. The bytes live at `path + ".part"` until done.
  path: string;
  /// Picked in a save dialog: the user already agreed to replace
  /// whatever is at `path`, so finishing doesn't re-pick a free name.
  chosen: boolean;
  mime: string;
  kind: string;
  totalBytes: number;
  receivedBytes: number;
  state: DownloadState;
  error?: string;
  startedAt: number;
  finishedAt?: number;
  context: DownloadContext;
  /// Encrypted-channel attachment. `wrappedKey` (safeStorage) is kept
  /// only while unfinished, and only when a keychain is available.
  encrypted?: { chunkBytes: number; wrappedKey?: string };
}

/// What the renderer gets: the record minus the key, plus live numbers.
export interface DownloadView {
  id: string;
  serverId: string;
  attachmentId: number;
  filename: string;
  mime: string;
  kind: string;
  totalBytes: number;
  receivedBytes: number;
  state: DownloadState;
  error?: string;
  startedAt: number;
  finishedAt?: number;
  context: DownloadContext;
  speedBps: number;
  /// Seconds left, -1 when unknown.
  etaS: number;
  /// Finished, but the file is no longer on disk.
  missing: boolean;
}

interface Run {
  abort: AbortController;
  stop: "pause" | "cancel" | "remove" | "quit" | null;
  lastEmit: number;
  sampleAt: number;
  sampleBytes: number;
  speedBps: number;
}

/// Insertion order is start order.
const records = new Map<string, DownloadRecord>();
const runs = new Map<string, Run>();
/// Key snapshots, by record id (see the header).
const keys = new Map<string, AttachmentKeyInfo>();

let downloadDir = "";
let askEachTime = false;

class UserError extends Error {}

function resolvedDir(): string {
  return downloadDir || app.getPath("downloads");
}

// ---- file names -------------------------------------------------------

const WINDOWS_RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i;
const MAX_NAME_BYTES = 200;

/// The name comes from another user: make it a plain, portable file name.
export function sanitizeFilename(raw: string): string {
  let name = String(raw ?? "")
    .normalize("NFC")
    // Path separators, characters Windows refuses, control characters.
    // eslint-disable-next-line no-control-regex
    .replace(/[/\\:*?"<>|\x00-\x1f\x7f]/g, "_")
    .trim()
    // No hidden files, no "." / "..", and Windows drops trailing dots
    // and spaces silently.
    .replace(/^[.\s]+/, "")
    .replace(/[.\s]+$/, "");
  if (WINDOWS_RESERVED.test(name)) name = `_${name}`;
  if (Buffer.byteLength(name) > MAX_NAME_BYTES) {
    const ext = path.extname(name);
    const keepExt = ext.length > 1 && Buffer.byteLength(ext) <= 20 ? ext : "";
    let base = keepExt ? name.slice(0, -keepExt.length) : name;
    while (Buffer.byteLength(base) + Buffer.byteLength(keepExt) > MAX_NAME_BYTES) {
      base = Array.from(base).slice(0, -1).join("");
    }
    name = base.replace(/[.\s]+$/, "") + keepExt;
  }
  return name || "attachment";
}

/// `name`, `name (1).ext`, … — the first that's free on disk, has no
/// .part of its own, and keeps the whole path within Windows' MAX_PATH.
function uniquePath(dir: string, name: string): string {
  const ext = path.extname(name);
  let base = ext ? name.slice(0, -ext.length) : name;
  const room = 240 - dir.length - ext.length - PART.length - 8;
  if (base.length > room) base = base.slice(0, Math.max(8, room));
  for (let n = 0; n < 10_000; n += 1) {
    const candidate = path.join(dir, n === 0 ? `${base}${ext}` : `${base} (${n})${ext}`);
    if (!fs.existsSync(candidate) && !fs.existsSync(candidate + PART)) return candidate;
  }
  return path.join(dir, `${base} (${Date.now()})${ext}`);
}

// ---- persistence ------------------------------------------------------

function storeFile(): string {
  return path.join(app.getPath("userData"), "downloads.json");
}

let saveTimer: NodeJS.Timeout | null = null;

function scheduleSave(): void {
  if (saveTimer) return;
  saveTimer = setTimeout(() => {
    saveTimer = null;
    saveNow();
  }, 500);
}

function saveNow(): void {
  const file = storeFile();
  const tmp = `${file}.tmp`;
  try {
    fs.writeFileSync(tmp, JSON.stringify({ version: 1, downloads: [...records.values()] }), {
      mode: 0o600,
    });
    fs.renameSync(tmp, file);
  } catch (e) {
    console.warn("[downloads] save failed:", (e as Error).message);
  }
}

function isRecord(v: unknown): v is DownloadRecord {
  const r = v as DownloadRecord;
  return (
    !!r &&
    typeof r.id === "string" &&
    typeof r.serverId === "string" &&
    typeof r.attachmentId === "number" &&
    typeof r.path === "string" &&
    path.isAbsolute(r.path) &&
    typeof r.filename === "string" &&
    typeof r.totalBytes === "number" &&
    typeof r.receivedBytes === "number" &&
    ["active", "paused", "done", "failed", "cancelled"].includes(r.state) &&
    !!r.context &&
    typeof r.context === "object"
  );
}

function load(): void {
  try {
    const data = JSON.parse(fs.readFileSync(storeFile(), "utf8")) as { downloads?: unknown[] };
    for (const r of data.downloads ?? []) {
      if (!isRecord(r)) continue;
      // Running when the app last stopped (a crash skips the quit hook).
      if (r.state === "active") r.state = "paused";
      records.set(r.id, r);
    }
  } catch {
    // No file yet, or unreadable: start empty.
  }
}

/// Over the cap: drop the oldest finished records, never live ones.
function trim(): void {
  if (records.size <= MAX_RECORDS) return;
  for (const [id, r] of records) {
    if (records.size <= MAX_RECORDS) break;
    if (r.state === "done" || r.state === "failed" || r.state === "cancelled") {
      if (r.state !== "done") fs.rm(r.path + PART, { force: true }, () => {});
      records.delete(id);
      keys.delete(id);
    }
  }
}

// ---- keys -------------------------------------------------------------

function wrapKey(info: AttachmentKeyInfo): string | undefined {
  try {
    if (!safeStorage.isEncryptionAvailable()) return undefined;
    return safeStorage.encryptString(info.key.toString("base64")).toString("base64");
  } catch {
    return undefined;
  }
}

function resolveKey(rec: DownloadRecord): AttachmentKeyInfo | null {
  const held = keys.get(rec.id);
  if (held) return held;
  const enc = rec.encrypted;
  if (!enc) return null;
  const registered = getAttachmentKey(rec.serverId, rec.attachmentId);
  let key: Buffer | null = registered?.key ?? null;
  if (!key && enc.wrappedKey) {
    try {
      key = Buffer.from(safeStorage.decryptString(Buffer.from(enc.wrappedKey, "base64")), "base64");
    } catch {
      key = null;
    }
  }
  if (!key || key.length !== 32) return null;
  const info: AttachmentKeyInfo = {
    key,
    chunkBytes: enc.chunkBytes,
    sizeBytes: rec.totalBytes,
    mime: rec.mime,
    filename: rec.filename,
  };
  keys.set(rec.id, info);
  return info;
}

// ---- events -----------------------------------------------------------

function view(rec: DownloadRecord, run?: Run, checkDisk = false): DownloadView {
  const speedBps = run?.speedBps ?? 0;
  const left = rec.totalBytes - rec.receivedBytes;
  return {
    id: rec.id,
    serverId: rec.serverId,
    attachmentId: rec.attachmentId,
    filename: rec.filename,
    mime: rec.mime,
    kind: rec.kind,
    totalBytes: rec.totalBytes,
    receivedBytes: rec.receivedBytes,
    state: rec.state,
    error: rec.error,
    startedAt: rec.startedAt,
    finishedAt: rec.finishedAt,
    context: rec.context,
    speedBps,
    etaS: run && speedBps > 0 && left >= 0 ? Math.ceil(left / speedBps) : -1,
    missing: checkDisk && rec.state === "done" && !fs.existsSync(rec.path),
  };
}

function broadcast(name: string, payload: unknown): void {
  for (const win of BrowserWindow.getAllWindows()) {
    if (!win.isDestroyed()) win.webContents.send("decibell:event", { name, payload });
  }
}

function emit(rec: DownloadRecord): void {
  broadcast("downloads_changed", view(rec, runs.get(rec.id)));
}

// ---- transfer ---------------------------------------------------------

function httpError(status: number): UserError {
  if (status === 404 || status === 410) return new UserError("No longer on the server");
  if (status === 401 || status === 403) return new UserError("Not allowed by the server");
  if (status === 502) return new UserError("Couldn't decrypt the file");
  return new UserError(`Server error ${status}`);
}

function describe(e: unknown): string {
  if (e instanceof UserError) return e.message;
  const code = (e as NodeJS.ErrnoException)?.code;
  if (code === "ENOSPC") return "Disk full";
  if (code === "EACCES" || code === "EPERM" || code === "EROFS") return "Can't write to the folder";
  return "Network error";
}

function abortError(): Error {
  return new DOMException("Aborted", "AbortError");
}

function progress(rec: DownloadRecord, run: Run, received: number): void {
  rec.receivedBytes = received;
  const now = performance.now();
  const dt = now - run.sampleAt;
  if (dt >= SPEED_SAMPLE_MS) {
    const inst = ((received - run.sampleBytes) * 1000) / dt;
    run.speedBps = run.speedBps === 0 ? inst : run.speedBps * 0.7 + inst * 0.3;
    run.sampleAt = now;
    run.sampleBytes = received;
  }
  if (now - run.lastEmit >= EMIT_INTERVAL_MS) {
    run.lastEmit = now;
    emit(rec);
  }
}

async function runPlain(rec: DownloadRecord, run: Run, fh: fsp.FileHandle, offset: number): Promise<void> {
  const target = getAttachmentTarget(rec.serverId);
  if (!target) throw new UserError(`Not connected to ${rec.context.serverName || "the server"}`);
  const headers: Record<string, string> = { Authorization: `Bearer ${target.jwt}` };
  if (offset > 0) headers.Range = `bytes=${offset}-`;
  const resp = await net.fetch(
    `https://${target.host}:${target.port}/attachments/${rec.attachmentId}`,
    { method: "GET", headers, signal: run.abort.signal },
  );
  if (resp.status === 416 && offset > 0 && offset === rec.totalBytes) return;
  if (resp.status !== 200 && resp.status !== 206) throw httpError(resp.status);
  let pos = offset;
  if (resp.status === 200) {
    // Fresh start, or the server ignored the Range: start over.
    if (offset > 0) await fh.truncate(0);
    pos = 0;
    const len = Number(resp.headers.get("content-length"));
    if (Number.isFinite(len) && len >= 0) rec.totalBytes = len;
  } else {
    const total = Number(/\/(\d+)\s*$/.exec(resp.headers.get("content-range") ?? "")?.[1]);
    if (Number.isFinite(total) && total > 0) rec.totalBytes = total;
  }
  run.sampleBytes = pos;
  progress(rec, run, pos);
  if (!resp.body) return;
  const reader = pacedBody(resp.body, rec.serverId, String(rec.attachmentId)).getReader();
  // A cancelled paced stream resolves its pending read as done — check
  // the signal, not just `done`, before calling the file complete.
  const onAbort = () => {
    reader.cancel().catch(() => {});
  };
  run.abort.signal.addEventListener("abort", onAbort);
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (run.abort.signal.aborted) throw abortError();
      if (done) break;
      await fh.write(value, 0, value.byteLength, pos);
      pos += value.byteLength;
      progress(rec, run, pos);
    }
  } finally {
    run.abort.signal.removeEventListener("abort", onAbort);
  }
}

async function runSealed(
  rec: DownloadRecord,
  run: Run,
  fh: fsp.FileHandle,
  offset: number,
  key: AttachmentKeyInfo,
): Promise<void> {
  const total = key.sizeBytes;
  rec.totalBytes = total;
  let pos = offset;
  run.sampleBytes = pos;
  progress(rec, run, pos);
  const chunk = key.chunkBytes;
  while (pos < total) {
    if (!getAttachmentTarget(rec.serverId)) {
      throw new UserError(`Not connected to ${rec.context.serverName || "the server"}`);
    }
    const rate = downloadRateBps();
    const want = rate > 0 ? Math.min(SEALED_WINDOW_BYTES, Math.floor(rate / 2)) : SEALED_WINDOW_BYTES;
    // End on a sealed-chunk boundary (so no window refetches its
    // neighbour's edge), at least one boundary past `pos`.
    let end = Math.floor((pos + want) / chunk) * chunk;
    if (end <= pos) end = (Math.floor(pos / chunk) + 1) * chunk;
    end = Math.min(total, end);
    const res = await fetchDecryptedWith(
      key,
      rec.serverId,
      String(rec.attachmentId),
      "",
      `bytes=${pos}-${end - 1}`,
      run.abort.signal,
    );
    if (run.abort.signal.aborted) throw abortError();
    if (res.status !== 206 && res.status !== 200) throw httpError(res.status);
    if (res.body.byteLength === 0) throw new UserError("The server sent an empty range");
    await fh.write(res.body, 0, res.body.byteLength, pos);
    pos += res.body.byteLength;
    progress(rec, run, pos);
  }
}

async function finalize(rec: DownloadRecord): Promise<void> {
  let dest = rec.path;
  // Something may have taken the reserved name meanwhile.
  if (!rec.chosen && fs.existsSync(dest)) dest = uniquePath(path.dirname(dest), path.basename(dest));
  await fsp.rename(rec.path + PART, dest);
  rec.path = dest;
  rec.filename = path.basename(dest);
}

async function run(rec: DownloadRecord): Promise<void> {
  if (runs.has(rec.id)) return;
  const r: Run = {
    abort: new AbortController(),
    stop: null,
    lastEmit: 0,
    sampleAt: performance.now(),
    sampleBytes: rec.receivedBytes,
    speedBps: 0,
  };
  runs.set(rec.id, r);
  rec.state = "active";
  rec.error = undefined;
  rec.finishedAt = undefined;
  emit(rec);
  scheduleSave();

  const part = rec.path + PART;
  let fh: fsp.FileHandle | null = null;
  try {
    const key = rec.encrypted ? resolveKey(rec) : null;
    if (rec.encrypted && !key) {
      throw new UserError("Open the file's channel again, then retry");
    }
    let offset = 0;
    try {
      offset = (await fsp.stat(part)).size;
    } catch {
      offset = 0;
    }
    if (rec.totalBytes > 0 && offset > rec.totalBytes) offset = 0;
    fh = await fsp.open(part, offset > 0 ? "r+" : "w");
    if (key) await runSealed(rec, r, fh, offset, key);
    else await runPlain(rec, r, fh, offset);
    await fh.sync();
    await fh.close();
    fh = null;
    const size = (await fsp.stat(part)).size;
    if (rec.totalBytes > 0 && size !== rec.totalBytes) {
      throw new UserError("The download came out the wrong size");
    }
    await finalize(rec);
    rec.receivedBytes = size;
    rec.state = "done";
    rec.finishedAt = Date.now();
    if (rec.encrypted) delete rec.encrypted.wrappedKey;
    keys.delete(rec.id);
  } catch (e) {
    if (fh) await fh.close().catch(() => {});
    if (r.stop === "cancel" || r.stop === "remove") {
      await fsp.rm(part, { force: true }).catch(() => {});
      rec.state = "cancelled";
      rec.receivedBytes = 0;
    } else if (r.stop === "pause" || r.stop === "quit") {
      rec.state = "paused";
    } else {
      rec.state = "failed";
      rec.error = describe(e);
      if (!(e instanceof UserError)) console.warn("[downloads] failed:", (e as Error).message);
    }
  } finally {
    runs.delete(rec.id);
    if (r.stop === "remove") {
      records.delete(rec.id);
      keys.delete(rec.id);
      broadcast("downloads_removed", { ids: [rec.id] });
    } else if (r.stop !== "quit") {
      emit(rec);
    }
    if (r.stop !== "quit") scheduleSave();
  }
}

// ---- commands ---------------------------------------------------------

interface StartArgs {
  serverId: string;
  attachmentId: number;
  filename: string;
  mime: string;
  kind: string;
  sizeBytes: number;
  saveAs: boolean;
  context: DownloadContext;
}

function cleanContext(c: Partial<DownloadContext> | undefined): DownloadContext {
  return {
    serverName: String(c?.serverName ?? ""),
    channelId: String(c?.channelId ?? ""),
    channelName: String(c?.channelName ?? ""),
    messageId: Number(c?.messageId) || 0,
    sender: String(c?.sender ?? ""),
  };
}

/// Pick and reserve the final path (its .part exists on return). Null
/// when the user dismisses the save dialog.
async function reserveDestination(
  name: string,
  ask: boolean,
): Promise<{ path: string; chosen: boolean } | null> {
  if (ask) {
    const win = BrowserWindow.getFocusedWindow() ?? BrowserWindow.getAllWindows()[0];
    const opts = { defaultPath: path.join(resolvedDir(), name) };
    const res = win ? await dialog.showSaveDialog(win, opts) : await dialog.showSaveDialog(opts);
    if (res.canceled || !res.filePath) return null;
    const chosen = path.resolve(res.filePath);
    for (const r of records.values()) {
      if (r.path === chosen && (r.state === "active" || r.state === "paused")) {
        throw new UserError("That file is already being downloaded");
      }
    }
    await fsp.writeFile(chosen + PART, new Uint8Array(0));
    return { path: chosen, chosen: true };
  }
  const dir = resolvedDir();
  await fsp.mkdir(dir, { recursive: true });
  // `wx` loses a race with a simultaneous start of the same name
  // cleanly, and we pick again.
  for (;;) {
    const candidate = uniquePath(dir, name);
    try {
      await fsp.writeFile(candidate + PART, new Uint8Array(0), { flag: "wx" });
      return { path: candidate, chosen: false };
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
    }
  }
}

async function start(a: StartArgs): Promise<{ id: string } | null> {
  if (!a || typeof a.serverId !== "string" || !Number.isInteger(a.attachmentId) || a.attachmentId <= 0) {
    throw new Error("bad download request");
  }
  const context = cleanContext(a.context);
  if (!getAttachmentTarget(a.serverId)) {
    throw new UserError(`Not connected to ${context.serverName || "the server"}`);
  }
  let dest: { path: string; chosen: boolean } | null;
  try {
    dest = await reserveDestination(sanitizeFilename(a.filename), a.saveAs || askEachTime);
  } catch (e) {
    throw e instanceof UserError ? e : new UserError(describe(e));
  }
  if (!dest) return null;
  const finalPath = dest.path;

  const key = getAttachmentKey(a.serverId, a.attachmentId);
  const rec: DownloadRecord = {
    id: randomUUID(),
    serverId: a.serverId,
    attachmentId: a.attachmentId,
    filename: path.basename(finalPath),
    path: finalPath,
    chosen: dest.chosen,
    mime: String(a.mime || "application/octet-stream"),
    kind: ["image", "video", "audio", "document"].includes(a.kind) ? a.kind : "document",
    totalBytes: key?.sizeBytes ?? Math.max(0, Number(a.sizeBytes) || 0),
    receivedBytes: 0,
    state: "active",
    startedAt: Date.now(),
    context,
  };
  if (key) {
    keys.set(rec.id, key);
    rec.encrypted = { chunkBytes: key.chunkBytes, wrappedKey: wrapKey(key) };
  }
  records.set(rec.id, rec);
  trim();
  void run(rec);
  return { id: rec.id };
}

function pause(id: string): void {
  const r = runs.get(id);
  if (!r) return;
  r.stop = "pause";
  r.abort.abort();
}

function resume(id: string): void {
  const rec = records.get(id);
  if (!rec || runs.has(id)) return;
  if (rec.state === "done" && fs.existsSync(rec.path)) return;
  if (rec.state === "done" || rec.state === "cancelled") {
    // Download again: same name if it's still free, otherwise a fresh one.
    if (fs.existsSync(rec.path) || fs.existsSync(rec.path + PART)) {
      rec.path = uniquePath(path.dirname(rec.path), path.basename(rec.path));
      rec.filename = path.basename(rec.path);
    }
    rec.receivedBytes = 0;
    if (rec.state === "done" && rec.encrypted && !keys.has(rec.id)) {
      // The finished record dropped its wrapped key; the registry may
      // have it again.
      const k = getAttachmentKey(rec.serverId, rec.attachmentId);
      if (k) {
        keys.set(rec.id, k);
        rec.encrypted.wrappedKey = wrapKey(k);
      }
    }
  }
  void run(rec);
}

function cancel(id: string): void {
  const r = runs.get(id);
  if (r) {
    r.stop = "cancel";
    r.abort.abort();
    return;
  }
  const rec = records.get(id);
  if (!rec || (rec.state !== "paused" && rec.state !== "failed")) return;
  fs.rm(rec.path + PART, { force: true }, () => {});
  rec.state = "cancelled";
  rec.receivedBytes = 0;
  rec.error = undefined;
  emit(rec);
  scheduleSave();
}

function remove(ids: string[]): void {
  const gone: string[] = [];
  for (const id of ids) {
    const r = runs.get(id);
    if (r) {
      r.stop = "remove";
      r.abort.abort();
      continue;
    }
    const rec = records.get(id);
    if (!rec) continue;
    if (rec.state !== "done") fs.rm(rec.path + PART, { force: true }, () => {});
    records.delete(id);
    keys.delete(id);
    gone.push(id);
  }
  if (gone.length > 0) {
    broadcast("downloads_removed", { ids: gone });
    scheduleSave();
  }
}

function list(): DownloadView[] {
  return [...records.values()].reverse().map((rec) => view(rec, runs.get(rec.id), true));
}

/// Quit: everything running becomes paused, saved synchronously.
export function pauseAllForQuit(): void {
  for (const [id, r] of runs) {
    r.stop = "quit";
    const rec = records.get(id);
    if (rec) rec.state = "paused";
    r.abort.abort();
  }
  if (saveTimer) {
    clearTimeout(saveTimer);
    saveTimer = null;
  }
  saveNow();
}

function windowFor(e: Electron.IpcMainInvokeEvent): BrowserWindow | undefined {
  return BrowserWindow.fromWebContents(e.sender) ?? undefined;
}

/// User-facing failures reach the renderer as `{ error }` rather than a
/// rejected invoke (Electron prefixes those with handler noise).
async function userResult<T>(fn: () => Promise<T>): Promise<T | { error: string }> {
  try {
    return await fn();
  } catch (e) {
    if (e instanceof UserError) return { error: e.message };
    throw e;
  }
}

export function registerDownloadHandlers(): void {
  load();
  const ids = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : typeof v === "string" ? [v] : [];

  ipcMain.handle("decibell:downloads:start", (_e, a: StartArgs) => userResult(() => start(a)));
  ipcMain.handle("decibell:downloads:list", () => list());
  ipcMain.handle("decibell:downloads:pause", (_e, id: unknown) => ids(id).forEach(pause));
  ipcMain.handle("decibell:downloads:resume", (_e, id: unknown) => ids(id).forEach(resume));
  ipcMain.handle("decibell:downloads:cancel", (_e, id: unknown) => ids(id).forEach(cancel));
  ipcMain.handle("decibell:downloads:remove", (_e, id: unknown) => remove(ids(id)));
  ipcMain.handle("decibell:downloads:clearFinished", () => {
    remove(
      [...records.values()]
        .filter((r) => r.state === "done" || r.state === "failed" || r.state === "cancelled")
        .map((r) => r.id),
    );
  });
  ipcMain.handle("decibell:downloads:open", async (_e, id: unknown) => {
    const rec = typeof id === "string" ? records.get(id) : undefined;
    if (!rec || rec.state !== "done" || !fs.existsSync(rec.path)) return { error: "File not found" };
    const err = await shell.openPath(rec.path);
    return err ? { error: err } : null;
  });
  ipcMain.handle("decibell:downloads:showInFolder", (_e, id: unknown) => {
    const rec = typeof id === "string" ? records.get(id) : undefined;
    if (!rec) return;
    if (fs.existsSync(rec.path)) shell.showItemInFolder(rec.path);
    else if (fs.existsSync(rec.path + PART)) shell.showItemInFolder(rec.path + PART);
    else void shell.openPath(path.dirname(rec.path));
  });
  ipcMain.handle("decibell:downloads:openFolder", async () => {
    const dir = resolvedDir();
    await fsp.mkdir(dir, { recursive: true }).catch(() => {});
    const err = await shell.openPath(dir);
    return err ? { error: err } : null;
  });
  ipcMain.handle("decibell:downloads:configure", (_e, c: { dir?: unknown; askEachTime?: unknown }) => {
    const dir = typeof c?.dir === "string" ? c.dir : "";
    downloadDir = dir && path.isAbsolute(dir) ? path.resolve(dir) : "";
    askEachTime = c?.askEachTime === true;
  });
  ipcMain.handle("decibell:downloads:defaultFolder", () => app.getPath("downloads"));
  ipcMain.handle("decibell:downloads:pickFolder", async (e) => {
    const opts: Electron.OpenDialogOptions = {
      properties: ["openDirectory", "createDirectory"],
      defaultPath: resolvedDir(),
    };
    const win = windowFor(e);
    const res = win ? await dialog.showOpenDialog(win, opts) : await dialog.showOpenDialog(opts);
    return res.canceled || res.filePaths.length === 0 ? null : path.resolve(res.filePaths[0]);
  });
}
