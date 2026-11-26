// Vite build of the preview page. Run by build.sh (which makes the node_modules link and
// .work/app.css first): node build.mjs <harness dir> <repo root>
import path from "node:path";
import { build } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const [here, repo] = process.argv.slice(2).map((p) => path.resolve(p));
if (!here || !repo) {
  console.error("usage: node build.mjs <harness dir> <repo root>");
  process.exit(2);
}
const work = path.join(here, ".work");

await build({
  configFile: false,
  envFile: false,
  root: path.join(here, "runtime"),
  base: "./",
  mode: "production",
  logLevel: "warn",
  publicDir: false,
  cacheDir: path.join(work, "vite-cache"),
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: [
      // Real components: import Switch from "@app/components/Switch";
      { find: /^@app\//, replacement: path.join(repo, "electron-client/src") + "/" },
      { find: /^@harness-app-css$/, replacement: path.join(work, "app.css") },
    ],
  },
  server: { fs: { allow: [repo] } },
  build: {
    outDir: path.join(work, "dist"),
    emptyOutDir: true,
    target: "chrome120",
    sourcemap: false,
    reportCompressedSize: false,
    chunkSizeWarningLimit: 100_000,
  },
});
console.log(`built ${path.join(work, "dist/index.html")}`);
