import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

import { beforeAll, describe, expect, it } from "vitest";

const root = process.cwd();
const dist = join(root, "dist");

/**
 * The shipped bundle, checked against what it is not allowed to contain.
 *
 * `src/lib/fixture.ts` is a scripted `vortexd` — forty simulated jobs, a lease scheduler and
 * a work-stealing loop — that exists so the quality floor in 05 can be looked at without
 * forty real downloads. It is reached through one `if (import.meta.env.MODE !== "production")`,
 * which folds to `if (false)` in a production build, and the module goes with it.
 *
 * That is worth a test rather than a comment because the failure is silent and bad: a
 * refactor that moves one import ships a download manager that invents downloads. It builds
 * fine, it starts fine, and the first sign of trouble is a user watching a file that does
 * not exist reach 100%.
 *
 * The build below is deliberately spawned with the environment vitest already set —
 * `NODE_ENV=test`. The first version of this gate used `import.meta.env.DEV`, which Vite
 * derives from `NODE_ENV` as well as from the mode, and under a test runner it stayed true
 * and shipped the fixture. Leaving the hostile environment in place is what keeps that
 * from coming back.
 */
describe("the production bundle", () => {
  let bundle = "";

  beforeAll(() => {
    execFileSync("npx", ["vite", "build"], {
      cwd: root,
      stdio: "pipe",
      shell: process.platform === "win32",
    });
    bundle = files(dist)
      .filter((path) => path.endsWith(".js"))
      .map((path) => readFileSync(path, "utf8"))
      .join("\n");
  }, 180_000);

  it("contains no trace of the fixture", () => {
    for (const term of [
      "Simulated",
      "releases.ubuntu.com",
      "Session 3",
      "stealingFrom: false",
      "the whole remaining lease",
    ]) {
      expect(bundle, `"${term}" survived into the shipped bundle`).not.toContain(term);
    }
  });

  it("still contains the real transport", () => {
    // The other half of the assertion above: proving the fixture is gone is only
    // meaningful if the thing it stands in for is present.
    expect(bundle).toContain("vortex://event");
    expect(bundle).toContain("__TAURI_INTERNALS__");
  });

  it("is small enough to paint a list in under 400 ms", () => {
    // The cold-start budget in 05 is 400 ms to a painted list, on a machine that is also
    // starting a daemon. Parsing is not the whole of that, but it is the part a build can
    // regress without anyone noticing.
    const scripts = files(dist).filter((p) => p.endsWith(".js"));
    const bytes = scripts.reduce((sum, path) => sum + statSync(path).size, 0);
    expect(bytes).toBeLessThan(400_000);
  });
});

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}
