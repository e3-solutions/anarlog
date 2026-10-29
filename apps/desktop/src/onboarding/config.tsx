import { platform } from "@tauri-apps/plugin-os";

import type { SectionStatus } from "./shared";

import { env } from "~/env";

export type OnboardingStep =
  | "permissions"
  | "login"
  | "calendar"
  | "imports"
  | "final";

const STEPS_MACOS: OnboardingStep[] = [
  "permissions",
  "login",
  "calendar",
  "imports",
  "final",
];
const STEPS_OTHER: OnboardingStep[] = ["login", "calendar", "imports", "final"];

export function isE3Account(email: string | undefined): boolean {
  const parts = email?.toLowerCase().split("@");
  return parts?.length === 2 && parts[1] === "e3group.ai";
}

function getOnboardingSteps(e3Account = false): OnboardingStep[] {
  const steps =
    platform() === "macos"
      ? env.VITE_E3_SCRIBE === "true"
        ? ([
            "login",
            "permissions",
            "calendar",
            "imports",
            "final",
          ] as OnboardingStep[])
        : STEPS_MACOS
      : STEPS_OTHER;
  return e3Account
    ? steps.filter((step) => step !== "calendar" && step !== "imports")
    : steps;
}

export function getInitialStep(): OnboardingStep {
  return getOnboardingSteps()[0];
}

export function getNextStep(
  currentStep: OnboardingStep,
  e3Account = false,
): OnboardingStep | null {
  const steps = getOnboardingSteps(e3Account);
  const idx = steps.indexOf(currentStep);
  return idx < steps.length - 1 ? steps[idx + 1] : null;
}

export function getPrevStep(
  currentStep: OnboardingStep,
  e3Account = false,
): OnboardingStep | null {
  const steps = getOnboardingSteps(e3Account);
  const idx = steps.indexOf(currentStep);
  return idx > 0 ? steps[idx - 1] : null;
}

export function getStepStatus(
  step: OnboardingStep,
  currentStep: OnboardingStep,
  e3Account = false,
): SectionStatus | null {
  const steps = getOnboardingSteps(e3Account);
  const stepIdx = steps.indexOf(step);
  if (stepIdx === -1) return null;
  const currentIdx = steps.indexOf(currentStep);
  if (stepIdx < currentIdx) return "completed";
  if (stepIdx === currentIdx) return "active";
  return "upcoming";
}
