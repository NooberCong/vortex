import { defineConfig } from "vitest/config";
import { WxtVitest } from "wxt/testing";

export default defineConfig({
  // `WxtVitest` supplies the `@/` alias, the entrypoint auto-imports, and — the reason it
  // is here — a `wxt/browser` backed by `fakeBrowser`, whose storage is a real
  // implementation rather than a stub. Storage semantics are where the ledger's bugs
  // live, so testing against a mock that just records calls would prove nothing.
  plugins: [WxtVitest()],
  test: {
    environment: "happy-dom",
    include: ["tests/**/*.test.ts"],
    restoreMocks: true,
  },
});
