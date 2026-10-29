import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { Button } from "@anlg/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@anlg/ui/components/ui/dialog";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import {
  enrollSharedRecordingAccount,
  readSharedRecordingAccount,
  type SharedRecordingAccount,
} from "./client";
import {
  configureSharedRecordings,
  sharedRecordingConfiguration,
} from "./native";

import { useAuth } from "~/auth";
import { env } from "~/env";
import {
  getStoredSettingValues,
  setSettingValues,
  useStoredSettingValuesQuery,
} from "~/settings/queries";
import type { SettingValues } from "~/settings/schema";
import { useOpenIntegrationUrl } from "~/shared/integration";
import { listenerStore } from "~/store/zustand/listener/instance";
import { useListener } from "~/stt/contexts";

export function SharedRecordingEnrollment({ userId }: { userId: string }) {
  return <ActiveEnrollment key={userId} userId={userId} />;
}

function ActiveEnrollment({ userId }: { userId: string }) {
  const { getSessionForRequest } = useAuth();
  const queryClient = useQueryClient();
  const settings = useStoredSettingValuesQuery();
  const [confirmed, setConfirmed] = useState(false);
  const idle = useListener(
    (state) => state.live.status === "inactive" && !state.live.loading,
  );
  const enroll = useMutation({
    mutationFn: async () => {
      const session = await getSessionForRequest();
      if (session?.user.id !== userId) throw new Error("Account changed.");
      const account = await enrollSharedRecordingAccount(session.access_token);
      const current = await getSessionForRequest();
      if (current?.user.id !== userId || account.userId !== userId)
        throw new Error("Account changed.");
      queryClient.setQueryData(
        ["shared-recordings", userId, "account"],
        account,
      );
    },
    retry: 1,
  });
  useMountEffect(() => {
    enroll.mutate();
  });
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  const account = useQuery({
    queryKey: ["shared-recordings", userId, "account"],
    queryFn: async () => {
      const session = await getSessionForRequest();
      if (session?.user.id !== userId) throw new Error("Account changed.");
      const result = await readSharedRecordingAccount(session.access_token);
      if (result.userId !== userId) throw new Error("Account changed.");
      return result;
    },
    refetchInterval: 5_000,
    refetchIntervalInBackground: true,
    retry: false,
  });
  const policy = account.data?.policy;
  if (
    !idle ||
    settings.isLoading ||
    settings.error ||
    !settings.data ||
    !account.data ||
    !policy?.autoRecordMeetings ||
    !policy.autoShareRecordings ||
    !account.data.workspaces.some(
      (workspace) => workspace.id === policy.workspaceId,
    )
  )
    return null;
  const calendarConfigured = savedScopes(
    settings.data.values.shared_recording_defaults_applied,
  ).includes(defaultScope(userId, policy.workspaceId));
  const ready = [
    "google-meet",
    ...(calendarConfigured ? [] : ["google-calendar"]),
  ].every((integrationId) =>
    account.data!.connections.some(
      (connection) =>
        connection.integrationId === integrationId &&
        connection.status === "connected",
    ),
  );
  if (!ready || (!calendarConfigured && !confirmed))
    return (
      <ConnectionWelcome
        key={JSON.stringify([userId, policy.workspaceId])}
        account={account.data}
        ready={ready}
        onContinue={() => setConfirmed(true)}
      />
    );
  return (
    <ApplyDefaults
      key={JSON.stringify([
        userId,
        policy.workspaceId,
        account.data.connections,
      ])}
      account={account.data}
      userId={userId}
    />
  );
}

function ApplyDefaults({
  account,
  userId,
}: {
  account: SharedRecordingAccount;
  userId: string;
}) {
  const { getSessionForRequest } = useAuth();
  const queryClient = useQueryClient();
  const apply = useMutation({
    mutationFn: async (isMounted: () => boolean) => {
      const current = async () => {
        const session = await getSessionForRequest();
        const live = listenerStore.getState().live;
        return (
          isMounted() &&
          session?.user.id === userId &&
          live.status === "inactive" &&
          !live.loading
        );
      };
      if (!(await current())) return;
      const stored = await getStoredSettingValues();
      if (!(await current())) return;
      const config = await sharedRecordingConfiguration();
      if (!(await current())) return;
      const organizationId = account.policy!.workspaceId;
      if (
        config &&
        (config.userId !== userId ||
          config.organizationId !== organizationId ||
          config.apiBaseUrl !== env.VITE_API_URL)
      )
        return;
      const scope = defaultScope(userId, organizationId);
      const applied = savedScopes(
        stored.values.shared_recording_defaults_applied,
      );
      if (!applied.includes(scope)) {
        if (!config) {
          await configureSharedRecordings({
            enabled: true,
            apiBaseUrl: env.VITE_API_URL,
            organizationId,
            userId,
            connections: account.connections,
          });
          if (!(await current())) return;
        }
        const latest = await getStoredSettingValues();
        if (!(await current())) return;
        const updates: SettingValues = {
          shared_recording_defaults_applied: JSON.stringify([
            ...applied,
            scope,
          ]),
        };
        for (const key of [
          "auto_start_detected_meetings",
          "auto_stop_meetings",
          "autostart",
        ] as const) {
          if (!latest.hasValues.has(key)) updates[key] = true;
        }
        if (!(await current())) return;
        await setSettingValues(updates);
      }
      if (
        config?.enabled &&
        JSON.stringify(config.connections) !==
          JSON.stringify(account.connections)
      ) {
        if (!(await current())) return;
        await configureSharedRecordings({
          ...config,
          connections: account.connections,
        });
      }
      await queryClient.invalidateQueries({
        queryKey: ["shared-recording-configuration", userId],
      });
    },
    retry: false,
  });
  useMountEffect(() => {
    let mounted = true;
    apply.mutate(() => mounted);
    return () => {
      mounted = false;
    };
  });
  return null;
}

function ConnectionWelcome({
  account,
  ready,
  onContinue,
}: {
  account: SharedRecordingAccount;
  ready: boolean;
  onContinue: () => void;
}) {
  const { openIntegration, openingAction } = useOpenIntegrationUrl();
  return (
    <Dialog open>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Welcome to E3</DialogTitle>
          <DialogDescription>
            Your Google sign-in connects Calendar and Meet. Supported meetings
            will automatically record and save their original audio and
            transcripts to E3’s database.
          </DialogDescription>
        </DialogHeader>
        <Button
          variant="outline"
          disabled={openingAction !== null || ready}
          onClick={() => {
            const connection = account.connections.find(
              (item) => item.integrationId === "google-meet",
            );
            openIntegration({
              nangoIntegrationId: "google-meet",
              connectionId: connection?.connectionId,
              action: connection ? "reconnect" : "connect",
            });
          }}
        >
          {ready ? "Google connected" : "Connect Google"}
        </Button>
        <p className="text-muted-foreground text-sm">
          Your Google connection is reused from sign-in. No additional account
          connection is needed.
        </p>
        <Button
          disabled={!ready || openingAction !== null}
          onClick={onContinue}
        >
          Finish setup
        </Button>
      </DialogContent>
    </Dialog>
  );
}

function defaultScope(userId: string, organizationId: string) {
  return JSON.stringify([
    userId,
    organizationId,
    env.VITE_API_URL,
    "google-calendar",
  ]);
}

function savedScopes(value: string | undefined): string[] {
  try {
    const parsed: unknown = JSON.parse(value ?? "[]");
    return Array.isArray(parsed)
      ? parsed.filter((entry): entry is string => typeof entry === "string")
      : [];
  } catch {
    return [];
  }
}
