import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri は固定ポートの devUrl を見るので、ポートは動かさない。
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    // インストーラを 20MB 以下に保つ (PLAN.md 5.7)。ソースマップは配らない。
    target: "es2022",
    sourcemap: false,
    minify: "esbuild",
  },
});
