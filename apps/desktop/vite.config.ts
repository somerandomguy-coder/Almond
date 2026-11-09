import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react(), tailwindcss()],

  resolve: {
    // Loro's default entry imports a .wasm ES module (needs a wasm plugin). The base64 build
    // instantiates synchronously from an inlined module and works in Vite, WebViews and vitest.
    alias: [{ find: /^loro-crdt$/, replacement: "loro-crdt/base64" }],
  },

  build: {
    // Tauri webviews (WebView2, WebKitGTK, Android System WebView) are all modern.
    target: "es2022",
    chunkSizeWarningLimit: 6000,
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
