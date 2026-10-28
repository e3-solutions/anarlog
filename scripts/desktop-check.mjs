import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import { desktopEnv } from "./desktop-env.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const child = spawn(
  "cargo",
  [
    "--config",
    "apps/desktop/src-tauri/.cargo/config.toml",
    "check",
    "--locked",
    "--no-default-features",
    "-p",
    "desktop",
    "--features",
    "dev,direct-distribution",
  ],
  {
    cwd: root,
    env: desktopEnv(),
    stdio: "inherit",
  },
);
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}
child.on("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  process.exitCode = code ?? (signal === "SIGINT" ? 130 : 1);
});
