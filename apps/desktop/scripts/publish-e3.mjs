#!/usr/bin/env node
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export async function publish(directory, env = process.env, request = fetch) {
  const version = env.E3_RELEASE_VERSION;
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version ?? "")) {
    throw new Error("E3_RELEASE_VERSION must be a semantic version");
  }
  const origin = new URL(env.SUPABASE_URL);
  if (origin.protocol !== "https:")
    throw new Error("SUPABASE_URL must use HTTPS");
  const secret = env.SUPABASE_SERVICE_ROLE_KEY;
  if (!secret) throw new Error("SUPABASE_SERVICE_ROLE_KEY is required");
  const bucket = env.E3_RELEASE_BUCKET ?? "e3-scribe-releases";
  if (!/^[a-z0-9-]+$/.test(bucket)) throw new Error("Invalid release bucket");
  const root = `${origin.origin}/storage/v1/object`;
  const publicUrl = (key) => `${root}/public/${bucket}/${key}`;
  const files = [
    "E3-Scribe-arm64.dmg",
    "E3-Scribe.app.tar.gz",
    "E3-Scribe.app.tar.gz.sig",
    "BUILD.json",
  ];
  const sizes = await Promise.all(
    files.map(async (name) => {
      const info = await stat(resolve(directory, name));
      if (!info.isFile() || info.size === 0)
        throw new Error(`Missing release file: ${name}`);
      return info.size;
    }),
  );
  const build = JSON.parse(
    await readFile(resolve(directory, "BUILD.json"), "utf8"),
  );
  if (
    build.version !== version ||
    !/^[a-f0-9]{40}$/.test(build.sourceSHA ?? "")
  ) {
    throw new Error(
      "BUILD.json must match the release version and source commit",
    );
  }
  if (
    !env.E3_APP_IDENTIFIER ||
    build.bundleIdentifier !== env.E3_APP_IDENTIFIER ||
    build.signed !== true ||
    build.notarized !== true ||
    build.updaterEnabled !== true
  ) {
    throw new Error(
      "Release must be signed, notarized, updater-enabled, and match E3_APP_IDENTIFIER",
    );
  }
  const signature = (
    await readFile(resolve(directory, files[2]), "utf8")
  ).trim();
  if (!/^[A-Za-z0-9+/=]+$/.test(signature) || signature.length < 64) {
    throw new Error("Missing Tauri updater signature");
  }
  const prefix = `releases/${version}`;
  const upload = async (key, body, size, contentType, replace = false) => {
    const response = await request(`${root}/${bucket}/${key}`, {
      method: "POST",
      redirect: "error",
      headers: {
        Authorization: `Bearer ${secret}`,
        apikey: secret,
        "Content-Type": contentType,
        "Content-Length": String(size),
        "Cache-Control": replace ? "max-age=0" : "max-age=31536000, immutable",
        "x-upsert": String(replace),
      },
      body,
      duplex: "half",
    });
    if (!response.ok)
      throw new Error(`Upload failed for ${key}: HTTP ${response.status}`);
    const check = await request(publicUrl(key), {
      method: "HEAD",
      redirect: "error",
      cache: "no-store",
      headers: { "Accept-Encoding": "identity" },
    });
    if (!check.ok || Number(check.headers.get("content-length")) !== size) {
      throw new Error(`Public release verification failed for ${key}`);
    }
  };
  for (const [index, name] of files.entries()) {
    const type = name.endsWith(".json")
      ? "application/json"
      : "application/octet-stream";
    await upload(
      `${prefix}/${name}`,
      createReadStream(resolve(directory, name)),
      sizes[index],
      type,
    );
  }
  const feed = {
    version,
    notes: env.E3_RELEASE_NOTES ?? "E3 Scribe update",
    pub_date: new Date().toISOString(),
    url: publicUrl(`${prefix}/${files[1]}`),
    signature,
  };
  const body = JSON.stringify(feed);
  const feedKey = "update/darwin-aarch64.json";
  await upload(
    feedKey,
    body,
    Buffer.byteLength(body),
    "application/json",
    true,
  );
  return {
    installer: publicUrl(`${prefix}/${files[0]}`),
    updater: publicUrl(feedKey),
  };
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    if (!process.argv[2])
      throw new Error("Usage: publish-e3.mjs <artifact-directory>");
    console.log(JSON.stringify(await publish(process.argv[2]), null, 2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
