import { fileURLToPath } from "node:url";

import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vitest/config";

const dir = (path: string) => fileURLToPath(new URL(path, import.meta.url));

/**
 * Tauri serves this over `tauri://` in production and from the dev server in development.
 *
 * `clearScreen: false` and the fixed port are Tauri conventions: the CLI runs Vite as a
 * child process and needs to know where it landed, and a cleared screen swallows the Rust
 * compiler's output.
 */
export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: {
      $lib: dir("./src/lib"),
      $components: dir("./src/components"),
    },
  },
  clearScreen: false,
  server: {
    port: 5183,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    // WebView2 on Windows 10+ and WKWebView on macOS 12+ both handle this comfortably,
    // and the smaller output is one of the things that keeps cold start under 400 ms.
    target: "es2022",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
