import { existsSync } from "node:fs";
import { homedir } from "node:os";
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
