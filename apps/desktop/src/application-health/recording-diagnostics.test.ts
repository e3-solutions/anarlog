import { afterEach, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  events: [] as Array<{ reason: string; sessionId: string | null }>,
}));
vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  invoke: async (
    command: string,
    args: { input?: { reason: string; sessionId: string | null } },
  ) => {
    if (command.endsWith("_record") && args.input)
      state.events.push(args.input);
  },
}));
vi.mock("~/env", () => ({ env: { VITE_E3_SCRIBE: "true" } }));

afterEach(() => {
  vi.useRealTimers();
});

it("preserves recording decisions instead of filling the outbox with unchanged collector and policy polls", async () => {
  vi.useFakeTimers();
  const { configureRecordingDiagnostics, recordRecordingDiagnostic } =
    await import("./recording-diagnostics");
  await configureRecordingDiagnostics("account-a", true);
  for (let poll = 0; poll < 240; poll++) {
    recordRecordingDiagnostic(null, "policy_decision", "collector_started", {
      queuedCount: poll,
      lastAckAgeMs: poll * 15_000,
      droppedCount: 0,
    });
    recordRecordingDiagnostic(
      "recording-a",
      "policy_decision",
      "mic_still_active",
      {
        observation: "unknown",
        micActive: true,
        missingForMs: poll * 15_000,
      },
    );
    recordRecordingDiagnostic(null, "detector_observation", "probe_started");
    await vi.advanceTimersByTimeAsync(15_000);
  }
  expect(state.events).toEqual([
    expect.objectContaining({ reason: "collector_started", sessionId: null }),
    expect.objectContaining({
      reason: "mic_still_active",
      sessionId: "recording-a",
    }),
  ]);
  recordRecordingDiagnostic("recording-a", "policy_decision", "grace_pending", {
    micActive: false,
  });
  await Promise.resolve();
  expect(state.events[state.events.length - 1]).toMatchObject({
    reason: "grace_pending",
  });
  await configureRecordingDiagnostics("account-a", false);
});
