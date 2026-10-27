import { useAuth } from "~/auth";
import { env } from "~/env";

export function useE3CalendarAccess() {
  const { session } = useAuth();
  const email = session?.user.email?.toLowerCase().split("@");
  return (
    env.VITE_E3_SCRIBE === "true" &&
    email?.length === 2 &&
    email[1] === "e3group.ai"
  );
}
