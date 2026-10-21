const required = [
  "SUPABASE_URL",
  "SUPABASE_ANON_KEY",
  "SUPABASE_SERVICE_ROLE_KEY",
];
const missing = required.filter((name) => !process.env[name]?.trim());
if (missing.length) {
  throw new Error(
    `Missing isolated web runtime configuration: ${missing.join(", ")}`,
  );
}
if (process.env.ANARLOG_SHARED_RECORDINGS_PREVIEW !== "1") {
  throw new Error(
    "The isolated web runtime requires ANARLOG_SHARED_RECORDINGS_PREVIEW=1",
  );
}

await import("../server/index.mjs");
