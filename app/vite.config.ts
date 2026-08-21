import { readFileSync } from "node:fs";

import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// 版はビルド時に埋め込む。実行時に取りに行くと IPC の口を 1 つ増やすことになり、
// このアプリは root で動くので、静的な文字列のために広げたくない。
const { version } = JSON.parse(readFileSync("./package.json", "utf8"));

// Tauri は固定ポートの devUrl を見るので、ポートは動かさない。
export default defineConfig({
  plugins: [svelte()],
  define: { __APP_VERSION__: JSON.stringify(version) },
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
