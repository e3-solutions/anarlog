import { expect, it, vi } from "vitest";

vi.mock("~/env", () => ({
  env: { VITE_SHARED_RECORDINGS_PREVIEW_URL: "http://localhost:8789" },
}));
vi.mock("@tauri-apps/plugin-http", () => ({
  fetch: async (request: Request) => {
    if (
      request.url !== "http://localhost:8789/nango/connections" ||
      request.headers.get("Authorization") !== "Bearer preview-session"
    ) {
      throw new Error("Authenticated preview request was not preserved");
    }
    return Response.json({
      connections: [{ integration_id: "google-meet", connection_id: "meet-1" }],
    });
  },
}));

import { listConnections } from "@anlg/api-client";

import { createDesktopApiClient } from "./api-client";

it("loads authenticated preview connections when browser networking rejects loopback", async () => {
  vi.stubGlobal("fetch", async () => {
    throw new Error("Webview network request blocked");
  });
  try {
    const client = createDesktopApiClient({
      baseUrl: "http://localhost:8789",
      headers: { Authorization: "Bearer preview-session" },
    });
    const result = await listConnections({ client });
    expect(result.data?.connections[0]?.connection_id).toBe("meet-1");
  } finally {
    vi.unstubAllGlobals();
  }
});
