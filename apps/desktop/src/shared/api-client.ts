import { fetch } from "@tauri-apps/plugin-http";

import { createClient } from "@anlg/api-client/client";

import { env } from "~/env";

export function createDesktopApiClient(
  config: Parameters<typeof createClient>[0],
) {
  return createClient({
    ...config,
    ...(env.VITE_SHARED_RECORDINGS_PREVIEW_URL ? { fetch } : {}),
  });
}
