import assert from "node:assert/strict";
import test from "node:test";

import {
  oauthProviderQueryParams,
  oauthProviderScopes,
} from "./oauth-provider.ts";

test("only Google and Microsoft sign-in receive an account chooser", () => {
  for (const provider of ["google", "azure"] as const) {
    assert.deepEqual(oauthProviderQueryParams(provider), {
      prompt: "select_account",
    });
  }
  for (const provider of ["apple", "github"] as const) {
    assert.equal(oauthProviderQueryParams(provider), undefined);
  }
});

test("only Microsoft login requests OIDC identity scopes", () => {
  assert.equal(oauthProviderScopes("azure"), "openid email profile");
  for (const provider of ["google", "apple", "github"] as const) {
    assert.equal(oauthProviderScopes(provider), undefined);
  }
});

test("E3 desktop Google sign-in requests the offline meeting grant only for E3", () => {
  const context = { flow: "desktop", scheme: "e3-shared-recordings-preview" };
  assert.deepEqual(oauthProviderQueryParams("google", context), {
    access_type: "offline",
    prompt: "consent select_account",
    hd: "e3group.ai",
    include_granted_scopes: "true",
  });
  assert.match(
    oauthProviderScopes("google", context)!,
    /meetings\.space\.readonly/,
  );
  assert.match(oauthProviderScopes("google", context)!, /calendar\.readonly/);
  assert.equal(
    oauthProviderScopes("google", { ...context, flow: "web" }),
    undefined,
  );
  assert.equal(
    oauthProviderScopes("google", { ...context, scheme: "anarlog" }),
    undefined,
  );
});
