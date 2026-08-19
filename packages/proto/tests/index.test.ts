import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));

/**
 * The barrel is hand-written and the bindings are generated, which is exactly the shape
 * that rots: someone adds a type in Rust, `cargo test` writes the file, and no consumer
 * can see it. This is the only thing keeping the two in step.
 */
describe("the barrel", () => {
  const generated = readdirSync(here("../src/bindings"))
    .filter((name) => name.endsWith(".ts") && name !== "constants.ts")
    .map((name) => name.replace(/\.ts$/, ""));
  const barrel = readFileSync(here("../src/index.ts"), "utf8");

  it("re-exports every generated type", () => {
    expect(generated.length).toBeGreaterThan(20);
    const missing = generated.filter(
      (name) => !barrel.includes(`export type { ${name} }`),
    );
    expect(missing, "run the generator, then add these to src/index.ts").toEqual([]);
  });

  it("exports nothing that no longer exists", () => {
    const exported = [...barrel.matchAll(/export type \{ (\w+) \}/g)].map((m) => m[1]!);
    expect(exported.filter((name) => !generated.includes(name))).toEqual([]);
  });
});
