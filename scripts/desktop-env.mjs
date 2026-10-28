import { existsSync } from "node:fs";
import { availableParallelism, homedir, totalmem } from "node:os";
import { delimiter, join } from "node:path";

export function desktopEnv() {
  const env = { ...process.env };
  const paths = (env.PATH || "").split(delimiter);
  const cargoBin = join(homedir(), ".cargo/bin");
  if (
    !paths.some((path) => existsSync(join(path, "cargo"))) &&
    existsSync(join(cargoBin, "cargo"))
  ) {
    env.PATH = [cargoBin, ...paths].join(delimiter);
  }
  if (process.platform === "darwin" && !env.CARGO_BUILD_JOBS) {
    // Leave memory for the running app and other tools during a native build.
    env.CARGO_BUILD_JOBS = String(
      Math.min(
        availableParallelism(),
        Math.max(1, Math.floor(totalmem() / (4 * 1024 ** 3))),
      ),
    );
  }
  const xcode = "/Applications/Xcode.app/Contents/Developer";
  if (
    process.platform === "darwin" &&
    !env.DEVELOPER_DIR &&
    existsSync(xcode)
  ) {
    env.DEVELOPER_DIR = xcode;
  }
  return env;
}
