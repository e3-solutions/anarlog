import { isE3ScribeFlow } from "./desktop-flow.ts";

export type OAuthProvider = "apple" | "azure" | "google" | "github";

export function oauthProviderQueryParams(
  provider: OAuthProvider,
  context: { flow?: string; scheme?: string } = {},
) {
  if (provider === "google" && isE3ScribeFlow(context)) {
    return {
      access_type: "offline",
      prompt: "consent select_account",
      hd: "e3group.ai",
      include_granted_scopes: "true",
    };
  }
  if (provider === "google" || provider === "azure") {
    return { prompt: "select_account" };
  }
  return undefined;
}

export function oauthProviderScopes(
  provider: OAuthProvider,
  context: { flow?: string; scheme?: string } = {},
) {
  if (provider === "google" && isE3ScribeFlow(context)) {
    return "openid email profile https://www.googleapis.com/auth/meetings.space.readonly https://www.googleapis.com/auth/calendar.readonly";
  }
  if (provider === "azure") {
    return "openid email profile";
  }
  return undefined;
}
