#!/usr/bin/env node
// Post-build step on Windows: copy FFmpeg DLLs from the vcpkg
// installation directory next to the napi addon so both
// `npm run dev` and `electron-builder` packaging find them on PATH.
//
// On Linux/macOS this is a no-op — those platforms link against
// system FFmpeg / system libraries discovered by the build script.
//
// Required env: VCPKG_ROOT pointing at the vcpkg install root.
// Reads from %VCPKG_ROOT%\installed\x64-windows\bin\*.dll and writes
// the FFmpeg-family DLLs (avcodec / avutil / avformat / swresample /
// swscale) into this directory.

if (process.platform !== "win32") {
  process.exit(0);
}

const fs = require("node:fs");
const path = require("node:path");

const vcpkgRoot = process.env.VCPKG_ROOT;
if (!vcpkgRoot) {
  console.warn(
    "[copy-dlls] VCPKG_ROOT not set; skipping. Set it before `npm run build:native` to bundle FFmpeg DLLs.",
  );
  process.exit(0);
}

const binDir = path.join(vcpkgRoot, "installed", "x64-windows", "bin");
if (!fs.existsSync(binDir)) {
  console.warn(`[copy-dlls] ${binDir} does not exist; nothing to copy.`);
  process.exit(0);
}

// FFmpeg + its direct dependencies. The version-suffix portion
// (e.g. avcodec-62.dll) shifts across FFmpeg releases, so we
// glob by prefix. Anything else in the vcpkg bin dir is unrelated
// to our build and shouldn't ride along.
const wanted = [
  /^avcodec-\d+\.dll$/i,
  /^avutil-\d+\.dll$/i,
  /^avformat-\d+\.dll$/i,
  /^swresample-\d+\.dll$/i,
  /^swscale-\d+\.dll$/i,
];

const dest = __dirname;
let copied = 0;
for (const file of fs.readdirSync(binDir)) {
  if (!wanted.some((re) => re.test(file))) continue;
  const src = path.join(binDir, file);
  const dst = path.join(dest, file);
  fs.copyFileSync(src, dst);
  copied += 1;
}
console.log(`[copy-dlls] copied ${copied} FFmpeg DLLs from ${binDir} → ${dest}`);
