import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const desktopDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const buildArgs = process.argv.slice(2);
if (
  buildArgs.some((arg) => arg === "--config" || arg.startsWith("--config="))
) {
  throw new Error(
    "build-e3 owns Tauri config order; pass target and feature flags only",
  );
}
const required = (name) => {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required for an E3 release`);
  return value;
};

const endpoint = required("E3_UPDATER_ENDPOINT");
const publicKey = required("E3_UPDATER_PUBLIC_KEY");
const identifier = required("E3_APP_IDENTIFIER");
required("TAURI_SIGNING_PRIVATE_KEY");

const url = new URL(endpoint);
if (
  url.protocol !== "https:" ||
  url.username ||
  url.password ||
  url.hostname === "desktop.anarlog.so"
) {
  throw new Error(
    "E3_UPDATER_ENDPOINT must be an E3-controlled HTTPS endpoint",
  );
}
for (const placeholder of ["{{target}}", "{{arch}}", "{{current_version}}"]) {
  if (!endpoint.includes(placeholder)) {
    throw new Error(`E3_UPDATER_ENDPOINT must include ${placeholder}`);
  }
}
if (
  publicKey ===
  "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEFFNUI0ODhBNzhBMTEwQQpSV1FLRVlxbmlMVGxDdXJvaUVTTnlwNEF5R1Bodzh3WThMZUpNMVk4K0hhbnV4TDZ1TFFucC9Gbwo="
) {
  throw new Error(
    "E3_UPDATER_PUBLIC_KEY must differ from the upstream release key",
  );
}
if (identifier.startsWith("com.hyprnote.")) {
  throw new Error(
    "E3_APP_IDENTIFIER must not reuse an upstream bundle identifier",
  );
}

const tempDir = await mkdtemp(path.join(os.tmpdir(), "anarlog-e3-build-"));
const configPath = path.join(tempDir, "e3-updater.json");
const config = {
  identifier,
  plugins: {
    updater: {
      active: true,
      endpoints: [endpoint],
      pubkey: publicKey,
    },
  },
};

try {
  await writeFile(configPath, JSON.stringify(config));
  const args = [
    "tauri",
    "build",
    "--config",
    "src-tauri/tauri.conf.stable.json",
  ];
  if (process.platform === "darwin") {
    args.push("--config", "src-tauri/tauri.conf.stable-macos.json");
  }
  args.push(...buildArgs, "--config", configPath);
  const child = spawn("pnpm", args, { cwd: desktopDir, stdio: "inherit" });
  const exitCode = await new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", resolve);
  });
  if (exitCode !== 0) process.exitCode = exitCode ?? 1;
} finally {
  await rm(tempDir, { recursive: true, force: true });
}
