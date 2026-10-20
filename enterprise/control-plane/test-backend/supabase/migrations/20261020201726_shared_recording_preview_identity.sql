-- Isolated preview identity tables only; not a full Anarlog database.
CREATE TABLE public.workspaces (
  id uuid PRIMARY KEY,
  owner_user_id uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  kind text NOT NULL DEFAULT 'personal',
  name text NOT NULL DEFAULT 'Personal',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  deleted_at timestamptz,
  CONSTRAINT workspaces_kind_check CHECK (kind IN ('personal', 'shared')),
  CONSTRAINT workspaces_personal_identity_check CHECK (kind = 'shared' OR id = owner_user_id)
);

CREATE TABLE public.workspace_memberships (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id uuid NOT NULL REFERENCES public.workspaces(id) ON DELETE CASCADE,
  user_id uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  role text NOT NULL DEFAULT 'member',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  deleted_at timestamptz,
  CONSTRAINT workspace_memberships_role_check CHECK (
    role IN ('owner', 'admin', 'member')
  ),
  CONSTRAINT workspace_memberships_workspace_user_key UNIQUE (workspace_id, user_id)
);

CREATE UNIQUE INDEX workspaces_personal_owner_key
  ON public.workspaces(owner_user_id)
  WHERE kind = 'personal' AND deleted_at IS NULL;

CREATE INDEX workspace_memberships_user_active_idx
  ON public.workspace_memberships(user_id, workspace_id)
  WHERE deleted_at IS NULL;

ALTER TABLE public.workspaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.workspace_memberships ENABLE ROW LEVEL SECURITY;

REVOKE ALL ON TABLE public.workspaces FROM PUBLIC, anon, authenticated;
REVOKE ALL ON TABLE public.workspace_memberships FROM PUBLIC, anon, authenticated;
GRANT SELECT ON TABLE public.workspaces TO authenticated;
GRANT SELECT ON TABLE public.workspace_memberships TO authenticated;
GRANT ALL ON TABLE public.workspaces TO service_role;
GRANT ALL ON TABLE public.workspace_memberships TO service_role;

CREATE POLICY workspaces_select_member
  ON public.workspaces
  FOR SELECT
  TO authenticated
  USING (
    deleted_at IS NULL
    AND id IN (
      SELECT membership.workspace_id
      FROM public.workspace_memberships AS membership
      WHERE membership.user_id = (SELECT auth.uid())
        AND membership.deleted_at IS NULL
    )
  );

CREATE POLICY workspaces_service_all
  ON public.workspaces
  FOR ALL
  TO service_role
  USING (true)
  WITH CHECK (true);

CREATE POLICY workspace_memberships_select_self
  ON public.workspace_memberships
  FOR SELECT
  TO authenticated
  USING (
    user_id = (SELECT auth.uid())
    AND deleted_at IS NULL
  );

CREATE POLICY workspace_memberships_service_all
  ON public.workspace_memberships
  FOR ALL
  TO service_role
  USING (true)
  WITH CHECK (true);


CREATE TABLE "nango_connections" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "user_id" uuid NOT NULL,
    "integration_id" text NOT NULL,
    "connection_id" text NOT NULL,
    "provider" text NOT NULL,
    "status" text NOT NULL DEFAULT 'connected',
    "last_error_type" text,
    "last_error_description" text,
    "last_error_at" timestamptz,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE "nango_connections" ENABLE ROW LEVEL SECURITY;

ALTER TABLE "nango_connections"
    ADD CONSTRAINT "nango_connections_user_id_fk"
    FOREIGN KEY ("user_id") REFERENCES "auth"."users"("id") ON DELETE CASCADE;

CREATE UNIQUE INDEX "nango_connections_user_integration_idx"
    ON "nango_connections" ("user_id", "integration_id");

CREATE INDEX "nango_connections_connection_id_idx"
    ON "nango_connections" ("connection_id");

CREATE POLICY "nango_connections_select_owner" ON "nango_connections"
    AS PERMISSIVE FOR SELECT TO "authenticated"
    USING ((select auth.uid()) = user_id);

CREATE POLICY "nango_connections_service_all" ON "nango_connections"
    AS PERMISSIVE FOR ALL TO "service_role"
    USING (true) WITH CHECK (true);

CREATE UNIQUE INDEX nango_connections_provider_identity_key ON public.nango_connections(integration_id,connection_id);
REVOKE ALL ON TABLE public.nango_connections FROM PUBLIC, anon, authenticated;
GRANT SELECT ON TABLE public.nango_connections TO authenticated;
GRANT ALL ON TABLE public.nango_connections TO service_role;
