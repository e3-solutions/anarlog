import { spawnSync } from "node:child_process";
import { statfsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { desktopEnv } from "./desktop-env.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const env = desktopEnv();

let failures = 0;
function check(label, executable, args, repair) {
  const result = spawnSync(executable, args, {
    cwd: root,
    env,
    encoding: "utf8",
  });
  if (result.status === 0) {
    console.log(`OK ${label}: ${result.stdout.trim().split("\n")[0]}`);
  } else {
    failures++;
    console.error(`MISSING ${label}: ${repair}`);
  }
}

check(
  "Rust",
  "rustc",
  ["--version"],
  'Install the pinned toolchain with rustup, then source "$HOME/.cargo/env".',
);
check("Cargo", "cargo", ["--version"], 'Source "$HOME/.cargo/env".');
check(
  "Process Compose",
  "process-compose",
  ["version"],
  "brew install f1bonacc1/tap/process-compose",
);
if (process.platform === "darwin") {
  check(
    "Xcode",
    "xcodebuild",
    ["-version"],
    "Install full Xcode compatible with your macOS from developer.apple.com/download/all/. Command Line Tools alone cannot build on-device transcription.",
  );
  check(
    "Metal compiler",
    "xcrun",
    ["metal", "--version"],
    "DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -downloadComponent MetalToolchain",
  );
}
const disk = statfsSync(root);
const free = (disk.bavail * disk.bsize) / 1024 ** 3;
console.log(`Free disk space: ${free.toFixed(1)} GiB`);
if (free < 25) {
  console.warn(
    "Allow at least 25 GiB free for the first native build, in addition to space for Xcode installation.",
  );
}
console.log(
  "Daily development: pnpm dev:desktop; after Rust edits: process-compose -p 18081 process restart desktop",
);
process.exitCode = failures ? 1 : 0;
