#!/usr/bin/env node
// Pre-build step: copy Twemoji SVGs from the @twemoji/svg npm package
// into public/twemoji/ so Vite serves them from the app origin. This
// replaces the previous jsdelivr CDN fetch that hammered the renderer
// with hundreds of network round-trips (and 404s on the ZWJ sequences
// missing from twemoji 15.x) every time the emoji picker mounted.
//
// Idempotent — only copies files that don't already exist or whose
// content differs from source. Cheap to run on every dev start.

const fs = require("node:fs");
const path = require("node:path");

const srcDir = path.join(
  __dirname,
  "..",
  "node_modules",
  "@twemoji",
  "svg",
);
const destDir = path.join(__dirname, "..", "public", "twemoji");

if (!fs.existsSync(srcDir)) {
  console.warn(
    `[copy-twemoji] ${srcDir} does not exist. Did you run \`npm install\`?`,
  );
  process.exit(0);
}

fs.mkdirSync(destDir, { recursive: true });

let copied = 0;
let skipped = 0;
for (const file of fs.readdirSync(srcDir)) {
  if (!file.endsWith(".svg")) continue;
  const src = path.join(srcDir, file);
  const dst = path.join(destDir, file);
  if (fs.existsSync(dst)) {
    // Skip if file already matches — saves ~1800 file writes on every
    // dev restart. Comparing by size is sufficient since SVG content
    // doesn't randomize and the upstream package is pinned by version.
    const srcStat = fs.statSync(src);
    const dstStat = fs.statSync(dst);
    if (srcStat.size === dstStat.size) {
      skipped += 1;
      continue;
    }
  }
  fs.copyFileSync(src, dst);
  copied += 1;
}
console.log(
  `[copy-twemoji] copied ${copied} SVGs, skipped ${skipped} unchanged → ${destDir}`,
);
