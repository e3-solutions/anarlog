import { useQuery } from "@tanstack/react-query";

import { sharedRecordingConfiguration } from "./native";

export function useSharingConfiguration(userId: string) {
  return useQuery({
    queryKey: ["shared-recording-configuration", userId],
    queryFn: async () => {
      const config = await sharedRecordingConfiguration();
      return config?.userId === userId ? config : null;
    },
    refetchIntervalInBackground: true,
    refetchInterval: 5_000,
    retry: false,
  });
}
