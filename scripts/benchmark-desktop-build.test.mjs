import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(
  new URL("benchmark-desktop-build.py", import.meta.url),
);

test("failed Rust edit leaves the checkout untouched and cannot produce a successful speedup result", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "anarlog-benchmark-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const bin = join(directory, "bin");
  const plugin = join(directory, "plugins/misc/src/lib.rs");
  const manifest = join(directory, "apps/desktop/src-tauri/Cargo.toml");
  for (const path of [
    bin,
    join(directory, "plugins/misc/src"),
    join(directory, "apps/desktop/src-tauri"),
  ]) {
    mkdirSync(path, { recursive: true });
  }
  const source = 'const PLUGIN_NAME: &str = "misc";\n';
  const cargo = 'crate-type = ["rlib"]\n';
  writeFileSync(plugin, source);
  writeFileSync(manifest, cargo);
  for (const name of [
    "git",
    "rustc",
    "swift",
    "xcodebuild",
    "xcrun",
    "sysctl",
    "cargo",
  ]) {
    writeFileSync(
      join(bin, name),
      `#!${process.execPath}\n${name === "cargo" ? `const fs = require("node:fs"); process.exit(fs.readFileSync("plugins/misc/src/lib.rs", "utf8").includes("misc-benchmark-2") ? 23 : 0);` : name === "git" ? 'process.stdout.write(process.argv.includes("ls-files") ? "plugins/misc/src/lib.rs\\0apps/desktop/src-tauri/Cargo.toml\\0" : "fixture");' : 'console.log("fixture");'}\n`,
      { mode: 0o755 },
    );
  }
  const output = join(directory, "results");
  const result = spawnSync(
    "python3",
    [
      script,
      "--root",
      directory,
      "--output",
      output,
      "--variant",
      "baseline-original",
      "--samples",
      "2",
    ],
    {
      env: { ...process.env, PATH: `${bin}:${process.env.PATH}` },
      encoding: "utf8",
      timeout: 15_000,
    },
  );
  assert.equal(result.status, 1, result.stderr);
  const metrics = JSON.parse(
    readFileSync(join(output, "results.json"), "utf8"),
  );
  assert.equal(metrics.status, "failed");
  assert.equal(metrics.measurements.at(-1).exit_code, 23);
  assert.equal(metrics.rust_edit_median_seconds, undefined);
  assert.equal(readFileSync(plugin, "utf8"), source);
  assert.equal(readFileSync(manifest, "utf8"), cargo);
});
