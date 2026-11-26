// Compares the message list's row geometry between the two clients for the same fixture:
// the GPUI list's dump (`DECIBELL_CHAT_ROWS_DIR=<dir> decibell --selftest chat-rows` writes
// `<dir>/chat-fixture-rows.json` and `chat-features-rows.json`) against the Electron pages'
// (`run.sh --pages chat-fixture,chat-features` writes `<out>/chat-*-<theme>.json` with every
// row's offsetTop / offsetHeight and fractional box).
//
//   node tools/electron-captures/chat-rows.mjs <gpui dir> <electron dir> [theme] [channel...]
//
// Channels default to chat-fixture and chat-features; the content pages name theirs
// (content-attachments-tall, content-embeds-tall: `<gpui dir>/<channel>-rows.json` against
// `<electron dir>/<channel>-<theme>.json`).
//
// Rows are matched by message id. Prints, per channel: rows compared, rows whose height differs,
// the largest difference, and whether every row's top (relative to the first common row) is the
// same pixel in both clients. Exits 1 when a height differs by 1/64 px or more.
import fs from 'node:fs';
import path from 'node:path';

const [, , gpuiDir, electronDir, theme = 'nocturne', ...channels] = process.argv;
if (!gpuiDir || !electronDir) {
  console.error('usage: chat-rows.mjs <gpui dir> <electron dir> [theme] [channel...]');
  process.exit(2);
}
let failed = false;
for (const name of channels.length > 0 ? channels : ['chat-fixture', 'chat-features']) {
  const gpui = JSON.parse(fs.readFileSync(path.join(gpuiDir, `${name}-rows.json`), 'utf8'));
  const electron = JSON.parse(fs.readFileSync(path.join(electronDir, `${name}-${theme}.json`), 'utf8'));
  const theirs = new Map(electron.rows.map((row) => [row.id, row]));
  const common = gpui.rows.filter((row) => theirs.has(row.id));
  console.log(`${name}: GPUI ${gpui.rows.length} rows in a ${gpui.list.width} × ${gpui.list.height} list, Electron ${electron.rows.length} rows in ${electron.list?.clientWidth} (+bar) × ${electron.list?.clientHeight}; ${common.length} in both`);
  if (common.length === 0) { failed = true; continue; }
  const base = { ours: common[0].top, theirs: theirs.get(common[0].id) };
  let worst = 0, differing = 0, tops = 0;
  for (const row of common) {
    const other = theirs.get(row.id);
    const dh = row.height - other.height;
    if (Math.abs(dh) >= 1 / 64) {
      differing++;
      if (differing <= 12) console.log(`  row ${row.id}: height ${row.height} here, ${other.height} in Electron (${dh > 0 ? '+' : ''}${dh.toFixed(4)})`);
    }
    worst = Math.max(worst, Math.abs(dh));
    // offsetTop is the rounded top: compare on whole pixels.
    const ourTop = Math.round(row.top - base.ours + (base.theirs.top % 1));
    const theirTop = other.offsetTop - base.theirs.offsetTop;
    if (ourTop !== theirTop) tops++;
  }
  const last = common[common.length - 1];
  const span = { ours: last.top + last.height - base.ours, theirs: theirs.get(last.id).top + theirs.get(last.id).height - base.theirs.top };
  console.log(`  heights: ${common.length - differing} / ${common.length} equal, worst difference ${worst.toFixed(4)} px`);
  console.log(`  tops (offsetTop from the first common row): ${common.length - tops} / ${common.length} equal`);
  console.log(`  span of the common rows: ${span.ours} px here, ${span.theirs} px in Electron`);
  if (differing > 0) failed = true;
}
process.exit(failed ? 1 : 0);
