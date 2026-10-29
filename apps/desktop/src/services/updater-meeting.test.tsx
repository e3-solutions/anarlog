import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { UpdaterMeetingSync } from "./updater-meeting";

import { listenerStore } from "~/store/zustand/listener/instance";

const { published } = vi.hoisted(() => ({
  published: { active: undefined as boolean | undefined },
}));

vi.mock("@anlg/plugin-updater2", () => ({
  commands: {
    setMeetingActive: async (active: boolean) => {
      published.active = active;
      return { status: "ok", data: null };
    },
  },
}));

vi.mock("~/shared/app-store", () => ({ isAppStoreBuild: () => false }));

vi.mock("~/store/zustand/listener/instance", async () => {
  const { createStore } = await import("zustand/vanilla");
  return {
    listenerStore: createStore(() => ({
      live: {
        status: "inactive",
        loading: true,
        finalizingBySession: {},
        postStopProcessingBySession: {},
      },
    })),
  };
});

afterEach(cleanup);

test("blocks automatic restarts through recording startup and post-stop processing", async () => {
  render(<UpdaterMeetingSync />);
  await waitFor(() => expect(published.active).toBe(true));

  act(() => {
    listenerStore.setState(({ live }) => ({
      live: {
        ...live,
        loading: false,
        postStopProcessingBySession: { session: true },
      },
    }));
  });
  expect(published.active).toBe(true);

  act(() => {
    listenerStore.setState(({ live }) => ({
      live: { ...live, postStopProcessingBySession: {} },
    }));
  });
  await waitFor(() => expect(published.active).toBe(false));
});
