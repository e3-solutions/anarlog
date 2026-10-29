import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import { isE3Account } from "../config";
import { AfterLogin } from "./after-login";
import { BeforeLogin } from "./before-login";

import { useAuth } from "~/auth";

export function LoginSection({ onContinue }: { onContinue: () => void }) {
  const auth = useAuth();

  if (auth?.session) {
    return isE3Account(auth.session.user.email) ? (
      <E3SignedIn onContinue={onContinue} />
    ) : (
      <AfterLogin onContinue={onContinue} />
    );
  }

  return <BeforeLogin />;
}

function E3SignedIn({ onContinue }: { onContinue: () => void }) {
  useMountEffect(onContinue);
  return null;
}
