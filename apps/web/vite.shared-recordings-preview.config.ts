import config from "./vite.config";

if (process.env.ANARLOG_SHARED_RECORDINGS_PREVIEW !== "1") {
  throw new Error(
    "The isolated web build requires ANARLOG_SHARED_RECORDINGS_PREVIEW=1",
  );
}

export default config;
