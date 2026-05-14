#!/usr/bin/env node
// Build-time: emit a JSON map of codepoint → svg-string from the
// @twemoji/svg npm package. The renderer imports this JSON and
// inlines SVG content via dangerouslySetInnerHTML, which avoids the
// per-emoji <img> resource bookkeeping + parsed-SVG-DOM cost that
// dominated the picker's residual RAM (~300 MB after a full scroll).
//
// The bundled JSON is ~5 MB and lives at:
//   electron-client/src/components/emoji/twemoji-data.json
// gitignored alongside public/twemoji/ — regenerated on every
// `npm run dev` / `npm run build`.

const fs = require("node:fs");
const path = require("node:path");

const srcDir = path.join(
  __dirname,
  "..",
  "node_modules",
  "@twemoji",
  "svg",
);
const destFile = path.join(
  __dirname,
  "..",
  "src",
  "components",
  "emoji",
  "twemoji-data.json",
);

if (!fs.existsSync(srcDir)) {
  console.warn(
    `[twemoji-map] ${srcDir} does not exist. Did you run \`npm install\`?`,
  );
  process.exit(0);
}

const map = {};
for (const file of fs.readdirSync(srcDir)) {
  if (!file.endsWith(".svg")) continue;
  const cp = file.replace(/\.svg$/, "");
  let content = fs.readFileSync(path.join(srcDir, file), "utf8");
  // Strip the optional <?xml ... ?> prolog so the SVG can be inlined
  // directly as the body of a span. (Most @twemoji/svg files don't
  // ship a prolog; the regex is a defensive no-op when missing.)
  content = content.replace(/<\?xml[^?]+\?>\s*/g, "");
  map[cp] = content;
}

fs.mkdirSync(path.dirname(destFile), { recursive: true });
fs.writeFileSync(destFile, JSON.stringify(map));
console.log(
  `[twemoji-map] wrote ${Object.keys(map).length} emojis → ${destFile}`,
);
