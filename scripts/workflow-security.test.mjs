import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

test("staging and release result steps treat shell metacharacters as literal data", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "workflow-security-"));
  const hostile = "fix/$(touch${IFS}injected)`touch${IFS}injected`";
  try {
    for (const workflow of ["handle_staging.yaml", "handle_release.yaml"]) {
      const source = await readFile(
        new URL(`../.github/workflows/${workflow}`, import.meta.url),
        "utf8",
      );
      const step = source
        .split("      - id: result\n")[1]
        .split("      - uses:")[0];
      const [configuration, run] = step.split("        run: |\n");
      const values = {
        "inputs.channel": hostile,
        "steps.resolve.outputs.branch": hostile,
        "steps.resolve.outputs.sha": "0123456789abcdef",
        "steps.resolve.outputs.target_sha": "0123456789abcdef",
        "steps.resolve.outputs.resolved": "true",
        "steps.resolve.outputs.error": "",
        "steps.trigger.outputs.run_id": "123",
        "github.run_id": "456",
      };
      const interpolate = (text) =>
        text.replace(/\$\{\{\s*(.*?)\s*\}\}/g, (_, expression) => {
          assert.ok(
            expression in values,
            `Unknown fixture expression ${expression}`,
          );
          return values[expression];
        });
      const env = {
        PATH: process.env.PATH,
        REPO: "example/repository",
        GITHUB_OUTPUT: path.join(directory, workflow),
      };
      for (const line of configuration.split("\n")) {
        const assignment = line.match(/^          ([A-Z_]+): (.*)$/);
        if (assignment) env[assignment[1]] = interpolate(assignment[2]);
      }
      const result = spawnSync(
        "bash",
        ["-e", "-c", interpolate(run.replace(/^          /gm, ""))],
        { cwd: directory, env, encoding: "utf8" },
      );
      assert.equal(result.status, 0, result.stderr);
      const output = await readFile(env.GITHUB_OUTPUT, "utf8");
      const data = JSON.parse(output.trim().slice("data=".length));
      assert.ok(data[0].value.includes(hostile));
      await assert.rejects(readFile(path.join(directory, "injected")), {
        code: "ENOENT",
      });
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

function pilotStepRun(source, id) {
  const step = source.split(`      - id: ${id}\n`)[1]?.split(/\n      - /)[0];
  assert.ok(step, `Missing pilot step ${id}`);
  const run = step.split("        run: |\n")[1];
  assert.ok(run, `Missing run script for ${id}`);
  return run.replace(/^          /gm, "");
}

test("native-check mode exports only generated source and cannot access signing secrets or package apps", async () => {
  const source = await readFile(
    new URL("../.github/workflows/e3_capture_pilot.yaml", import.meta.url),
    "utf8",
  );
  assert.match(
    source,
    /native_checks_only:\n(?:.*\n)*?        type: boolean\n        default: false/,
  );
  const steps = source
    .split("    steps:\n")[1]
    .split(/^      - /m)
    .slice(1);
  for (const step of steps) {
    const generatedSourceUpload = step.startsWith(
      "name: Preserve generated preview bindings and permissions\n",
    );
    if (generatedSourceUpload) {
      assert.doesNotMatch(step, /secrets\.|run:/);
      const paths = step
        .split("          path: |\n")[1]
        ?.split("          retention-days:")[0]
        .trim()
        .split("\n")
        .map((line) => line.trim());
      assert.deepEqual(paths, [
        "plugins/transcription/js/bindings.gen.ts",
        "plugins/transcription/permissions/",
      ]);
      continue;
    }
    const restricted =
      /secrets\.|apple_cert|notarize|codesign|stapler|tauri build|prepare-binaries|archive-pilot|actions\/upload-artifact/.test(
        step,
      );
    if (restricted) {
      // This release policy applies even to always() recovery uploads.
      assert.match(
        step,
        /(?:^|\n)\s*if: (?:always\(\) && )?inputs\.native_checks_only != true(?: && [^\n]+)?\n/,
      );
    }
  }
});

test("pilot signing preflight reports secret names without exposing values", async () => {
  const source = await readFile(
    new URL("../.github/workflows/e3_capture_pilot.yaml", import.meta.url),
    "utf8",
  );
  const script = pilotStepRun(source, "signing-preflight");
  const required = [
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "KEYCHAIN_PASSWORD",
    "APPLE_ID",
    "APPLE_PASSWORD",
    "APPLE_TEAM_ID",
  ];
  const directory = await mkdtemp(path.join(os.tmpdir(), "pilot-signing-"));
  const secret = 'sensitive-$(touch injected)`touch injected`"';
  try {
    const invoke = (mode, secrets = {}, deferred = false) =>
      spawnSync("bash", ["-e", "-c", script], {
        cwd: directory,
        env: {
          PATH: process.env.PATH,
          SIGNING_MODE: mode,
          DEFER_NOTARIZATION: String(deferred),
          ...secrets,
        },
        encoding: "utf8",
      });
    const missing = invoke("developer-id");
    assert.notEqual(missing.status, 0);
    for (const name of required) assert.ok(missing.stderr.includes(name));
    const provided = Object.fromEntries(required.map((name) => [name, secret]));
    const signed = invoke("developer-id", provided);
    assert.equal(signed.status, 0, signed.stderr);
    assert.ok(!(signed.stdout + signed.stderr).includes(secret));
    assert.equal(invoke("ad-hoc").status, 0);
    assert.notEqual(invoke("unsupported").status, 0);
    const signingOnly = Object.fromEntries(
      required
        .filter((name) => !["APPLE_ID", "APPLE_PASSWORD"].includes(name))
        .map((name) => [name, secret]),
    );
    assert.equal(invoke("developer-id", signingOnly, true).status, 0);
    assert.notEqual(invoke("developer-id", signingOnly).status, 0);
    assert.notEqual(invoke("developer-id", {}, true).status, 0);
    assert.notEqual(invoke("ad-hoc", {}, true).status, 0);
    await assert.rejects(readFile(path.join(directory, "injected")), {
      code: "ENOENT",
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("deferred notarization preserves signed checkpoints without publishing a ready artifact", async () => {
  const source = await readFile(
    new URL("../.github/workflows/e3_capture_pilot.yaml", import.meta.url),
    "utf8",
  );
  assert.match(
    source,
    /defer_notarization:\n(?:.*\n)*?        type: boolean\n        default: false/,
  );
  const steps = source
    .split("    steps:\n")[1]
    .split(/^      - /m)
    .slice(1);
  const find = (needle) => {
    const found = steps.filter((step) => step.includes(needle));
    assert.equal(found.length, 1, `Expected one step containing ${needle}`);
    return found[0];
  };
  const protectedSteps = [
    find("id: notarization-preflight\n"),
    find("id: notarize-pilot\n"),
    find(
      "name: e3-shared-preview-${{ inputs.preview_instance }}-notary-submission-",
    ),
    find("id: archive-pilot\n"),
    find("name: e3-shared-recordings-preview-"),
  ];
  for (const step of protectedSteps) {
    assert.match(step, /if: [^\n]*inputs\.defer_notarization != true/);
  }
  for (const needle of [
    "uses: ./.github/actions/apple_cert",
    "cloudsync_dylib=",
    "id: build-pilot\n",
    "id: checkpoint-signed-pilot\n",
    "name: e3-shared-preview-${{ inputs.preview_instance }}-signed-NOT-NOTARIZED-",
  ]) {
    const step = find(needle);
    assert.doesNotMatch(
      step.split("        run:")[0],
      /if: [^\n]*defer_notarization/,
    );
  }
  const preflight = find("id: signing-preflight\n");
  for (const name of ["APPLE_ID", "APPLE_PASSWORD"]) {
    assert.ok(
      preflight.includes(
        `${name}: \${{ inputs.defer_notarization != true && secrets.${name} || '' }}`,
      ),
    );
  }
  assert.match(
    find("id: checkpoint-signed-pilot\n"),
    /codesign --verify --deep --strict/,
  );
  assert.match(find("id: archive-pilot\n"), /stapler validate/);
  assert.match(find("id: archive-pilot\n"), /spctl --assess/);
});

async function previewBuildEnv(directory) {
  const config = "tauri.shared-recordings-preview.json";
  const bundle = "ai.e3group.meeting-capture-shared-preview";
  const parent = path.join(directory, "apps/desktop/src-tauri");
  await mkdir(parent, { recursive: true });
  await writeFile(
    path.join(parent, config),
    JSON.stringify({ identifier: bundle }),
  );
  return {
    PREVIEW_CONFIG: config,
    PREVIEW_BUNDLE_ID: bundle,
    PREVIEW_PRODUCT: "E3 Shared Recordings Preview",
    VITE_SUPABASE_URL: "https://fixture.supabase.co",
    VITE_SUPABASE_ANON_KEY: "sb_publishable_fixture",
  };
}

test("preview signing rejects pilot identities and server keys while preserving certificate names as JSON data", async () => {
  const source = await readFile(
    new URL("../.github/workflows/e3_capture_pilot.yaml", import.meta.url),
    "utf8",
  );
  const script = pilotStepRun(source, "build-pilot");
  const python = script.match(/python3 - <<'PY'\n([\s\S]*?)\nPY\n/)[1];
  const directory = await mkdtemp(
    path.join(os.tmpdir(), "pilot-signing-json-"),
  );
  const identity =
    'Developer ID Application: Test "$(touch injected)`touch injected`';
  try {
    for (const mode of ["developer-id", "ad-hoc"]) {
      const result = spawnSync("python3", ["-c", python], {
        cwd: directory,
        env: {
          PATH: process.env.PATH,
          RUNNER_TEMP: directory,
          SIGNING_MODE: mode,
          SIGNING_IDENTITY: identity,
          ...(await previewBuildEnv(directory)),
        },
        encoding: "utf8",
      });
      assert.equal(result.status, 0, result.stderr);
      const override = JSON.parse(
        await readFile(
          path.join(directory, "e3-shared-preview-signing.json"),
          "utf8",
        ),
      );
      assert.deepEqual(override, {
        bundle: {
          macOS: {
            signingIdentity: mode === "developer-id" ? identity : "-",
            hardenedRuntime: true,
          },
        },
      });
    }
    for (const invalid of [
      { PREVIEW_BUNDLE_ID: "ai.e3group.meeting-capture-pilot" },
      { VITE_SUPABASE_ANON_KEY: "sb_secret_must_not_ship" },
    ]) {
      const result = spawnSync("python3", ["-c", python], {
        cwd: directory,
        env: {
          PATH: process.env.PATH,
          RUNNER_TEMP: directory,
          SIGNING_MODE: "developer-id",
          SIGNING_IDENTITY: identity,
          ...(await previewBuildEnv(directory)),
          ...invalid,
        },
        encoding: "utf8",
      });
      assert.notEqual(result.status, 0);
      assert.doesNotMatch(
        result.stdout + result.stderr,
        /sb_secret_must_not_ship/,
      );
    }
    await assert.rejects(readFile(path.join(directory, "injected")), {
      code: "ENOENT",
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("pilot builds defer notarization credentials until after the signed checkpoint", async () => {
  const source = await readFile(
    new URL("../.github/workflows/e3_capture_pilot.yaml", import.meta.url),
    "utf8",
  );
  const directory = await mkdtemp(path.join(os.tmpdir(), "pilot-adhoc-"));
  try {
    await writeFile(
      path.join(directory, "pnpm"),
      `#!/usr/bin/env python3
import os
for name in ("APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID", "APPLE_API_ISSUER", "APPLE_API_KEY", "APPLE_API_KEY_PATH"):
    assert name not in os.environ, name + " must be unset for ad-hoc signing"
`,
      { mode: 0o755 },
    );
    for (const mode of ["ad-hoc", "developer-id"]) {
      const result = spawnSync(
        "bash",
        ["-e", "-c", pilotStepRun(source, "build-pilot")],
        {
          cwd: directory,
          env: {
            PATH: directory + path.delimiter + process.env.PATH,
            RUNNER_TEMP: directory,
            SIGNING_MODE: mode,
            SIGNING_IDENTITY: "Developer ID Application: Fixture (TEAM)",
            ...(await previewBuildEnv(directory)),
            APPLE_ID: "",
            APPLE_PASSWORD: "",
            APPLE_TEAM_ID: "",
            APPLE_API_ISSUER: "",
            APPLE_API_KEY: "",
            APPLE_API_KEY_PATH: "",
          },
          encoding: "utf8",
        },
      );
      assert.equal(result.status, 0, result.stderr);
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("pilot notarization saves the submission before waiting and staples only accepted results", async () => {
  const source = await readFile(
    new URL("../.github/workflows/e3_capture_pilot.yaml", import.meta.url),
    "utf8",
  );
  const id = "a25e8bbd-7dba-4acf-82f4-a97333d46e3e";
  for (const outcome of ["accepted", "pending", "rerun"]) {
    const directory = await mkdtemp(path.join(os.tmpdir(), "pilot-notary-"));
    try {
      await mkdir(path.join(directory, "scripts"));
      await mkdir(path.join(directory, "preview-signed-checkpoint"));
      await writeFile(
        path.join(
          directory,
          "preview-signed-checkpoint/E3-Shared-Recordings-Preview-arm64.zip",
        ),
        "signed fixture",
      );
      await writeFile(
        path.join(directory, "scripts/notarize-dmg.mjs"),
        `
export async function notarizeDmg({log}) {
  log("Apple notarization submission: ${id}");
  if (process.env.TEST_OUTCOME !== "accepted") throw new Error("Still pending");
}
`,
      );
      await writeFile(
        path.join(directory, "xcrun"),
        "#!/bin/sh\ntouch stapled\n",
        { mode: 0o755 },
      );
      const result = spawnSync(
        "bash",
        ["-e", "-c", pilotStepRun(source, "notarize-pilot")],
        {
          cwd: directory,
          env: {
            PATH: directory + path.delimiter + process.env.PATH,
            GITHUB_RUN_ATTEMPT: outcome === "rerun" ? "2" : "1",
            GITHUB_SHA: "fixture-commit",
            TEST_OUTCOME: outcome,
            PREVIEW_PRODUCT: "E3 Shared Recordings Preview",
          },
          encoding: "utf8",
        },
      );
      assert.equal(result.status === 0, outcome === "accepted", result.stderr);
      if (outcome === "rerun") {
        await assert.rejects(
          readFile(path.join(directory, "preview-notary-submission.json")),
          { code: "ENOENT" },
        );
      } else {
        const receipt = JSON.parse(
          await readFile(
            path.join(directory, "preview-notary-submission.json"),
            "utf8",
          ),
        );
        assert.equal(receipt.submissionId, id);
        assert.equal(receipt.commit, "fixture-commit");
        assert.match(receipt.zipSha256, /^[a-f0-9]{64}$/);
      }
      if (outcome === "accepted")
        await readFile(path.join(directory, "stapled"));
      else
        await assert.rejects(readFile(path.join(directory, "stapled")), {
          code: "ENOENT",
        });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test("Apple certificate inputs stay literal and temporary private keys are always removed", async () => {
  const source = await readFile(
    new URL("../.github/actions/apple_cert/action.yaml", import.meta.url),
    "utf8",
  );
  const step = source
    .split("    - name: Setup Certificate\n")[1]
    .split("\n    - id:")[0];
  const script = step.split("      run: |\n")[1].replace(/^        /gm, "");
  const directory = await mkdtemp(
    path.join(os.tmpdir(), "apple-certificate-inputs-"),
  );
  const hostile =
    '$(touch injected-command)`touch injected-backtick`"$HOME"\\quote';
  const inputs = {
    "apple-certificate": "certificate-" + hostile,
    "apple-certificate-password": "certificate-password-" + hostile,
    "keychain-password": "keychain-password-" + hostile,
  };
  try {
    await writeFile(
      path.join(directory, "base64"),
      `#!/usr/bin/env python3
import os
import sys
from pathlib import Path
Path(os.environ["TEST_DIRECTORY"], "certificate-input").write_bytes(sys.stdin.buffer.read())
sys.stdout.buffer.write(b"private certificate fixture")
`,
      { mode: 0o755 },
    );
    await writeFile(
      path.join(directory, "security"),
      `#!/usr/bin/env python3
import json
import os
import stat
import sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ["TEST_DIRECTORY"], "security-arguments").open("a") as log:
    log.write(json.dumps(args) + "\\n")
if args[0] == "import":
    assert "-f" in args and args[args.index("-f") + 1] == "pkcs12"
    certificate = Path(args[1])
    assert certificate.read_bytes() == b"private certificate fixture"
    assert stat.S_IMODE(certificate.stat().st_mode) == 0o600
    if os.environ["TEST_FAIL_IMPORT"] == "1":
        sys.exit(7)
if args[0] == "find-identity":
    print('1) ABC "Developer ID Application: Fixture (TEAM)"')
`,
      { mode: 0o755 },
    );
    for (const failImport of ["0", "1"]) {
      const result = spawnSync(
        "bash",
        [
          "-e",
          "-c",
          script.replace(
            /\$\{\{\s*inputs\.([^\s}]+)\s*\}\}/g,
            (_, name) => inputs[name],
          ),
        ],
        {
          cwd: directory,
          env: {
            PATH: directory + path.delimiter + process.env.PATH,
            RUNNER_TEMP: directory,
            TEST_DIRECTORY: directory,
            TEST_FAIL_IMPORT: failImport,
            APPLE_CERTIFICATE: inputs["apple-certificate"],
            APPLE_CERTIFICATE_PASSWORD: inputs["apple-certificate-password"],
            KEYCHAIN_PASSWORD: inputs["keychain-password"],
          },
          encoding: "utf8",
        },
      );
      assert.equal(result.status, failImport === "1" ? 7 : 0, result.stderr);
      assert.equal(
        await readFile(path.join(directory, "certificate-input"), "utf8"),
        inputs["apple-certificate"],
      );
      const calls = (
        await readFile(path.join(directory, "security-arguments"), "utf8")
      )
        .trim()
        .split("\n")
        .map(JSON.parse);
      const imported = calls.filter((args) => args[0] === "import").at(-1);
      assert.equal(
        imported[imported.indexOf("-P") + 1],
        inputs["apple-certificate-password"],
      );
      assert.ok(
        calls.some(
          (args) =>
            args[0] === "create-keychain" &&
            args[2] === inputs["keychain-password"],
        ),
      );
      await assert.rejects(readFile(imported[1]), { code: "ENOENT" });
      for (const value of Object.values(inputs))
        assert.ok(!(result.stdout + result.stderr).includes(value));
      for (const marker of ["injected-command", "injected-backtick"]) {
        await assert.rejects(readFile(path.join(directory, marker)), {
          code: "ENOENT",
        });
      }
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
