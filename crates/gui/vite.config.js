import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// Phase Client entry (`client.html`) is built only on request: `PHASE_CLIENT_BUILD=1`
// together with the `client` cargo feature. A plain build is Phase LAN alone.
const withClient = process.env.PHASE_CLIENT_BUILD === "1";

const input = { main: fileURLToPath(new URL("./index.html", import.meta.url)) };
if (withClient) input.client = fileURLToPath(new URL("./client.html", import.meta.url));

// Tauri settings: a fixed port (tauri.conf.json → devUrl waits for it),
// Rust code changes don't restart Vite.
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
    // Phase Client reads the set manifest straight from crates/core: one source.
    fs: { allow: withClient ? [".", "../core/assets"] : ["."] },
  },
  build: {
    // The Tauri window on Windows is WebView2 (Chromium).
    target: "chrome120",
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: { input },
  },
});
