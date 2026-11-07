#!/usr/bin/env node
// Writes the spike's synthetic busy channel (design doc §11, D5): the same data for the Qt client and the Electron
// preview harness, so screenshots and measurements compare like for like. Deterministic (seeded), never real
// community content.
//
//   node qt-client/tools/gen-fixture.mjs [--messages 10000] [--seed 7]   → qt-client/assets/fixture/chat.json
//   node qt-client/tools/gen-fixture.mjs --images                        also fetch the pictures (picsum.photos ids)
//
// Shape (all times in ms since the epoch; `now` pins "Today" / "Yesterday" labels for reproducible captures):
//   { now, self: {uid, name}, server: {id, name, public}, channels: [{id, name, type}], members: [{uid, name, online,
//     status, avatar}], messages: [{id, senderUid, ts, content, replyTo, attachments: [{kind, file, width, height}]}] }
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.resolve(here, '../assets/fixture');
const arg = (name, def) => { const i = process.argv.indexOf(name); return i > 0 ? process.argv[i + 1] : def; };
const COUNT = parseInt(arg('--messages', '10000'), 10);
let seed = parseInt(arg('--seed', '7'), 10) >>> 0;
const rnd = () => { seed = (seed + 0x6d2b79f5) >>> 0; let t = seed; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
const pick = (a) => a[Math.floor(rnd() * a.length)];
const between = (lo, hi) => lo + Math.floor(rnd() * (hi - lo + 1));

// Pictures: picsum.photos ids are stable. Avatars square, attachments in a spread of aspect ratios.
const AVATAR_IDS = [64, 65, 91, 129, 177, 203, 237, 275, 338, 342];
const IMAGES = [
  [10, 1600, 900], [11, 1280, 960], [15, 900, 1200], [16, 1920, 1080], [17, 800, 800], [18, 2000, 800],
  [19, 1200, 1600], [20, 1600, 1066], [22, 640, 480], [24, 1080, 1350], [25, 1500, 1000], [26, 1000, 1500],
  [27, 1920, 1200], [28, 1200, 675], [29, 3000, 2000], [30, 720, 1280], [31, 1400, 1400], [32, 1024, 576],
];
const avatarFile = (id) => `img/avatar-${id}.jpg`;
const imageFile = ([id, w, h]) => `img/photo-${id}-${w}x${h}.jpg`;

const NAMES = ['sunkhan', 'johnny brawO', 'battalocelot', 'berkayacan', 'dolpompx', 'efil', 'emreaydn', 'Fiary',
  'maghteriton', 'n.rosess', 'asdasd', 'kekrem', 'qalite', 'deniz', 'zeynep_k', 'mert.the.long.name.example'];
const members = NAMES.map((name, i) => ({
  uid: 1001 + i,
  name,
  online: i < 4,
  status: i < 4 ? ['online', 'online', 'away', 'dnd'][i] : 'offline',
  avatar: i % 3 === 2 ? null : avatarFile(AVATAR_IDS[i % AVATAR_IDS.length]),
}));
// A handful of people do most of the talking, as in a real busy channel.
const WEIGHTS = members.map((m, i) => (i < 4 ? 12 : i < 8 ? 4 : 1));
const totalW = WEIGHTS.reduce((a, b) => a + b, 0);
const sender = () => { let r = rnd() * totalW; for (let i = 0; i < members.length; i++) { r -= WEIGHTS[i]; if (r < 0) return members[i]; } return members[0]; };

const WORDS = ('the a stream tonight build new server patch voice channel lag fps frame bitrate encoder works fine ' +
  'broken again fixed it now ok yes no maybe later tomorrow today game ranked match lobby queue join leave win lost ' +
  'gg nice clip watch this look at that why how when where who what config driver update reboot test release ' +
  'linux windows wayland nvidia audio mic headset echo noise loud quiet sorry thanks great cool wow lol ' +
  'selam naber tamam evet hayır sonra bugün yarın oyun maç ses kanal güncelleme').split(' ');
const EMOJI = ['😂', '🔥', '👍', '❤️', '🎮', '✨', '😭', '🙏', '👀', '💀', '🎉', '😅'];
const LANGS = { rust: 'fn main() {\n    let rate = 120;\n    println!("{rate} fps");\n}', ts: 'const fps = await probe();\nif (fps < 60) retry();', cpp: 'int main() {\n  return decibell::run();\n}', sh: 'cargo test --lib\nnpx napi build --platform' };

const sentence = (lo, hi) => { const n = between(lo, hi); const w = []; for (let i = 0; i < n; i++) w.push(pick(WORDS)); const s = w.join(' '); return s[0].toUpperCase() + s.slice(1); };
function content() {
  const r = rnd();
  if (r < 0.40) return sentence(1, 8);
  if (r < 0.58) return sentence(10, 30) + pick(['.', '?', '!', '']);
  if (r < 0.64) { const p = []; for (let i = 0; i < between(2, 4); i++) p.push(sentence(15, 40) + '.'); return p.join(' '); }
  if (r < 0.71) { const w = sentence(3, 10).split(' '); const i = between(0, w.length - 1); w[i] = '`' + pick(['cargo build', 'fps=120', 'nvenc', 'd3d11va', '--release', 'config.json', 'v0.9.4']) + '`'; return w.join(' '); }
  if (r < 0.74) { const lang = pick(Object.keys(LANGS)); return sentence(2, 8) + ':\n```' + lang + '\n' + LANGS[lang] + '\n```'; }
  if (r < 0.80) { const w = sentence(4, 14).split(' '); const i = between(0, w.length - 1); const m = pick([['**', '**'], ['*', '*'], ['__', '__'], ['~~', '~~']]); w[i] = m[0] + w[i] + m[1]; return w.join(' '); }
  if (r < 0.88) return sentence(2, 10) + ' ' + pick(EMOJI) + (rnd() < 0.3 ? pick(EMOJI) : '');
  if (r < 0.90) return Array.from({ length: between(1, 3) }, () => pick(EMOJI)).join('');
  if (r < 0.95) return pick(['check this ', 'link: ', '', 'docs here ']) + `https://example.com/${pick(['patch-notes', 'clips/' + between(100, 999), 'docs/voice', 'issue/' + between(1, 500)])}`;
  return sentence(1, 6) + '\nand ' + sentence(2, 8).toLowerCase();
}

// Ten thousand messages across ~90 days, in conversations: bursts of quick replies, then quiet hours.
const NOW = Date.UTC(2026, 10, 7, 17, 30); // 2026-11-07 17:30 UTC; the clients format in local time
const messages = [];
let ts = NOW - 90 * 24 * 3600e3;
let id = 7_000_000_000;
for (let i = 0; i < COUNT; i++) {
  ts += rnd() < 0.08 ? between(1, 10) * 3600e3 : rnd() < 0.5 ? between(5, 90) * 1000 : between(2, 15) * 60e3;
  const m = { id: String(++id), senderUid: sender().uid, ts: Math.round(ts), content: content(), replyTo: null, attachments: [] };
  if (i > 5 && rnd() < 0.06) m.replyTo = messages[messages.length - between(1, 5)].id;
  if (rnd() < 0.07) {
    const n = rnd() < 0.7 ? 1 : between(2, 3);
    for (let k = 0; k < n; k++) { const im = pick(IMAGES); m.attachments.push({ kind: 'image', file: imageFile(im), width: im[1], height: im[2] }); }
    if (rnd() < 0.4) m.content = '';
  }
  messages.push(m);
}
// Squeeze the timeline so the newest message is a few minutes before `now`.
const shift = NOW - 4 * 60e3 - messages[messages.length - 1].ts;
for (const m of messages) m.ts += shift;

const fixture = {
  now: NOW,
  self: { uid: 1001, name: 'sunkhan' },
  server: { id: '1', name: 'DailyGaming', public: true },
  channels: [
    ...['general', 'sessizler', 'ahmet', 'notes', 'kara-liste', 'unlu-dusunur'].map((n, i) => ({ id: String(10 + i), name: n, type: 'text' })),
    { id: '16', name: 'enc', type: 'text', encrypted: true },
    ...['expensive-belt-channel', 'kekrem-kanali', 'dandiq', 'qalite', 'test'].map((n, i) => ({ id: String(20 + i), name: n, type: 'voice' })),
    { id: '30', name: 'dasdaqw', type: 'category' },
    { id: '31', name: 'anilar', type: 'text' },
  ],
  members,
  messages,
};
fs.mkdirSync(OUT, { recursive: true });
fs.writeFileSync(path.join(OUT, 'chat.json'), JSON.stringify(fixture));
console.log(`fixture: ${messages.length} messages, ${members.length} members → ${path.join(OUT, 'chat.json')}`);

if (process.argv.includes('--images')) {
  const want = [...AVATAR_IDS.map((a) => [avatarFile(a), `https://picsum.photos/id/${a}/128/128.jpg`]),
    ...IMAGES.map((im) => [imageFile(im), `https://picsum.photos/id/${im[0]}/${im[1]}/${im[2]}.jpg`])];
  for (const [file, url] of want) {
    const p = path.join(OUT, file);
    if (fs.existsSync(p)) continue;
    const r = await fetch(url);
    if (!r.ok) { console.error(`${url}: HTTP ${r.status}`); continue; }
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, Buffer.from(await r.arrayBuffer()));
    console.log('fetched', file);
  }
}
