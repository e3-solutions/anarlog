import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { publish } from "./publish-e3.mjs";

test("an incomplete release cannot replace the working updater feed", async () => {
  const directory = await mkdtemp(join(tmpdir(), "e3-release-"));
  const objects = new Map([
    ["update/darwin-aarch64.json", Buffer.from('{"version":"0.1.0"}')],
  ]);
  let failArchive = true;
  const request = async (url, options) => {
    const key = new URL(url).pathname.split("e3-scribe-releases/")[1];
    if (options.method === "HEAD") {
      return new Response(null, {
        headers: { "content-length": objects.get(key)?.length ?? 0 },
      });
    }
    if (key.endsWith(".tar.gz") && failArchive)
      return new Response(null, { status: 503 });
    const data =
      typeof options.body === "string"
        ? Buffer.from(options.body)
        : Buffer.concat(await Array.fromAsync(options.body));
    objects.set(key, data);
    return new Response(null);
  };
  try {
    await Promise.all([
      writeFile(join(directory, "E3-Scribe-arm64.dmg"), "installer"),
      writeFile(join(directory, "E3-Scribe.app.tar.gz"), "archive"),
      writeFile(join(directory, "E3-Scribe.app.tar.gz.sig"), "a".repeat(100)),
      writeFile(
        join(directory, "BUILD.json"),
        JSON.stringify({ version: "0.2.0", sourceSHA: "a".repeat(40) }),
      ),
    ]);
    const env = {
      E3_RELEASE_VERSION: "0.2.0",
      SUPABASE_URL: "https://example.supabase.co",
      SUPABASE_SERVICE_ROLE_KEY: "private-test",
    };
    await assert.rejects(publish(directory, env, request), /Upload failed/);
    assert.equal(
      JSON.parse(objects.get("update/darwin-aarch64.json")).version,
      "0.1.0",
    );
    failArchive = false;
    await publish(directory, env, request);
    const feed = JSON.parse(objects.get("update/darwin-aarch64.json"));
    assert.equal(feed.version, "0.2.0");
    assert.equal(feed.signature, "a".repeat(100));
    assert.equal(
      objects
        .get(new URL(feed.url).pathname.split("e3-scribe-releases/")[1])
        .toString(),
      "archive",
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
