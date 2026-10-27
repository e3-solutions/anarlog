import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { setTimeout } from "node:timers/promises";
import { pathToFileURL } from "node:url";

export function runCommand(args, spawn = spawnSync) {
  return spawn("xcrun", args, {
    encoding: "utf8",
    timeout:
      args[1] === "wait"
        ? 11 * 60 * 1000
        : args[1] === "history"
          ? 60_000
          : undefined,
    killSignal: "SIGKILL",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function readResponse(result) {
  try {
    return JSON.parse(result.stdout);
  } catch {
    return null;
  }
}

export function notarizationEnvironment(env = process.env) {
  return Object.fromEntries(
    ["APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID"].map((name) => [
      name,
      env[name]?.trim(),
    ]),
  );
}

function notarizationAuth(input) {
  const env = notarizationEnvironment(input);
  if (!env.APPLE_ID || !env.APPLE_PASSWORD || !env.APPLE_TEAM_ID) {
    throw new Error("Apple notarization credentials are required");
  }
  return [
    "--apple-id",
    env.APPLE_ID,
    "--password",
    env.APPLE_PASSWORD,
    "--team-id",
    env.APPLE_TEAM_ID,
    "--output-format",
    "json",
    "--no-progress",
  ];
}

export function preflightNotarization({
  env = process.env,
  run = runCommand,
} = {}) {
  // Read-only authentication check: no archive is uploaded or resubmitted.
  const result = run(["notarytool", "history", ...notarizationAuth(env)]);
  if (result.status === 0 && Array.isArray(readResponse(result)?.history))
    return;
  // Never forward Apple's raw output: it may contain account or credential data.
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  const reason = /\b401\b/.test(output)
    ? "Apple rejected the credentials (HTTP 401)"
    : /\b403\b/.test(output)
      ? "Apple denied access to the developer team (HTTP 403)"
      : result.error?.code === "ETIMEDOUT" || /timed? out|timeout/i.test(output)
        ? "Apple's authentication check timed out"
        : "Apple's submission history could not be verified";
  throw new Error(
    `Notarization preflight failed: ${reason}; compilation has not started. Check Apple notarization credentials and connectivity before retrying.`,
  );
}

export async function notarizeDmg({
  dmgPath,
  env = process.env,
  run = runCommand,
  sleep = setTimeout,
  log = console.log,
}) {
  if (!dmgPath) throw new Error("DMG path is required");
  const auth = notarizationAuth(env);
  const submitted = run([
    "notarytool",
    "submit",
    dmgPath,
    ...auth,
    "--no-wait",
  ]);
  const id = readResponse(submitted)?.id;
  if (
    submitted.status !== 0 ||
    !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(id ?? "")
  ) {
    // An upload may have succeeded despite a lost response. Never blindly resubmit.
    throw new Error(
      "Notarization submission failed or returned no valid ID; inspect Apple's submission history before retrying",
    );
  }
  log(`Apple notarization submission: ${id}`);
  for (let attempt = 1; attempt <= 4; attempt++) {
    const result = run(["notarytool", "wait", id, ...auth, "--timeout", "10m"]);
    const response = readResponse(result);
    if (response && response.id !== id) {
      throw new Error("Apple returned a different notarization submission ID");
    }
    if (result.status === 0 && response?.status === "Accepted") {
      log(`Apple accepted notarization submission ${id}`);
      return id;
    }
    if (response?.status && response.status !== "In Progress") {
      throw new Error(
        `Apple did not accept notarization submission ${id}: ${response.status}`,
      );
    }
    const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
    const transient =
      /NSURLErrorDomain\s+Code=-(?:1001|1005|1009)\b|HTTPError\(statusCode: (?:Optional\()?(?:429|500|502|503|504)\b|Timeout reached|timed out while waiting/i.test(
        output,
      );
    if (!transient || attempt === 4) {
      throw new Error(
        `Notarization wait failed for ${id} (attempt ${attempt}/4); no artifact may be published without Accepted status`,
      );
    }
    log(
      `Notarization wait interrupted; retrying submission ${id} (${attempt}/4)`,
    );
    await sleep(30_000 * attempt);
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    if (process.argv[2] === "--preflight") preflightNotarization();
    else await notarizeDmg({ dmgPath: process.argv[2] });
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
