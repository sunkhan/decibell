// Cuts a recorded stream (.dbv, DBV2: electron-client/native/src/media/stream_dump.rs) to a clean piece for replay:
// from the first keyframe (and its decoder description, if the stream has one) for N seconds, arrival times rebased
// to the cut's start, ending on a whole frame (a recording still being written, or killed, ends mid-frame). Prints
// what the stream is: frames, rate, keyframes, bitrate, arrival spacing.
//   node tools/perf/dbv-cut.mjs <in.dbv> <out.dbv> [seconds=60]
import fs from 'node:fs';

const [, , input, output, secondsArg = '60'] = process.argv;
const limitUs = Number(secondsArg) * 1e6;
const buf = fs.readFileSync(input);
if (buf.toString('latin1', 0, 4) !== 'DBV2') throw new Error('not a DBV2 recording');

const frames = [];
for (let pos = 20; pos + 16 <= buf.length; ) {
  const size = buf.readUInt32LE(pos), flags = buf.readUInt32LE(pos + 4);
  const at = Number(buf.readBigUInt64LE(pos + 8));
  let p = pos + 16, desc = null;
  if (flags & 2) {
    if (p + 4 > buf.length) break;
    const n = buf.readUInt32LE(p);
    desc = buf.subarray(p + 4, p + 4 + n);
    p += 4 + n;
  }
  if (p + size > buf.length) break; // the unfinished last frame
  frames.push({ flags, at, desc, data: buf.subarray(p, p + size) });
  pos = p + size;
}
const first = frames.findIndex((f) => f.flags & 1);
if (first < 0) throw new Error('no keyframe in the recording');
const t0 = frames[first].at;
const cut = frames.slice(first).filter((f) => f.at - t0 <= limitUs);

const parts = [buf.subarray(0, 20)];
let bytes = 0, keys = 0;
const gaps = [];
cut.forEach((f, i) => {
  const head = Buffer.alloc(16);
  head.writeUInt32LE(f.data.length, 0);
  head.writeUInt32LE(f.flags, 4);
  head.writeBigUInt64LE(BigInt(f.at - t0), 8);
  parts.push(head);
  if (f.flags & 2) {
    const n = Buffer.alloc(4);
    n.writeUInt32LE(f.desc.length, 0);
    parts.push(n, f.desc);
  }
  parts.push(f.data);
  bytes += f.data.length;
  if (f.flags & 1) keys++;
  if (i) gaps.push((f.at - cut[i - 1].at) / 1000);
});
fs.writeFileSync(output, Buffer.concat(parts));
const span = (cut[cut.length - 1].at - t0) / 1e6;
const sorted = [...gaps].sort((a, b) => a - b);
const pct = (p) => sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))].toFixed(2);
console.log(JSON.stringify({
  recorded_frames: frames.length,
  kept_frames: cut.length,
  seconds: +span.toFixed(2),
  fps: +(cut.length / span).toFixed(1),
  keyframes: keys,
  with_description: cut.filter((f) => f.flags & 2).length,
  mbit_s: +((bytes * 8) / span / 1e6).toFixed(1),
  arrival_gap_ms: { p50: pct(0.5), p95: pct(0.95), p99: pct(0.99), max: sorted[sorted.length - 1].toFixed(2) },
}));
