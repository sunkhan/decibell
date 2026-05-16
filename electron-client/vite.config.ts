import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Renderer-only config. Electron main + preload are built with tsc
// (see tsconfig.node.json) so this file never sees electron/* code.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  base: "./",
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist/renderer",
    emptyOutDir: true,
    target: "chrome120",
    // Emit .js.map files so sentry-cli can upload them on tagged
    // releases. The maps are excluded from the packaged build via
    // electron-builder.yml so they reach Sentry's artifact store
    // but not the user's machine.
    sourcemap: true,
  },
});
