// Writes ramp_expected.txt: avatar / name colour slots computed by the renderer's own
// electron-client/src/utils/colors.ts (Node ≥ 23 runs the TypeScript directly), for
// `identity::tests::ramp_matches_the_renderer`.
//   node gpui-client/crates/decibell-ui/tests/gen_ramp_expected.mjs
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const colors = await import(pathToFileURL(path.resolve(here, '../../../../electron-client/src/utils/colors.ts')).href);
const slot = (css) => Number(css.match(/--color-name-(\d)\)/)[1]);

const inputs = [
  '', 'a', 'ada', 'Ada', 'basil', 'aurora', 'Daily Gaming', 'Rustaceans', 'Design Lab',
  'ıİğüşöç', 'Ünïcødé', '日本語の名前', 'emoji 🎉🔥', '👩‍👩‍👧', 'a'.repeat(64),
  'The quick brown fox jumps over the lazy dog, again and again and again',
  'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz', 'tab\there',
];
for (let uid = 1; uid <= 40; uid++) inputs.push(`uid:${uid}`);
for (const uid of [100, 999, 1234, 65535, 123456789, 2147483647, 9007199254740991]) inputs.push(`uid:${uid}`);

let out = '# slot<TAB>input, from electron-client/src/utils/colors.ts stringToColor (regenerate: gen_ramp_expected.mjs)\n';
for (const input of inputs) out += `${slot(colors.stringToColor(input))}\t${input.replace(/\t/g, '\\t')}\n`;
// userColor keys on the uid when there is one.
if (slot(colors.userColor(7, 'x')) !== slot(colors.stringToColor('uid:7'))) throw new Error('personKey changed');
fs.writeFileSync(path.join(here, 'ramp_expected.txt'), out);
console.log(`wrote ${inputs.length} slots`);
