CREATE TABLE public.api_shared_session_snapshots (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id uuid NOT NULL,
  session_id text NOT NULL,
  workspace_id uuid NOT NULL REFERENCES public.workspaces(id) ON DELETE CASCADE,
  calendar_event_key text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (owner_user_id, session_id),
  UNIQUE (workspace_id, calendar_event_key),
  CONSTRAINT api_shared_session_snapshots_event_key_check CHECK (
    calendar_event_key = btrim(calendar_event_key)
    AND octet_length(calendar_event_key) BETWEEN 1 AND 512
  ),
  FOREIGN KEY (owner_user_id, session_id)
    REFERENCES public.api_session_snapshots(user_id, session_id) ON DELETE CASCADE
);

CREATE INDEX api_shared_session_snapshots_workspace_idx
  ON public.api_shared_session_snapshots (workspace_id, id);

ALTER TABLE public.api_shared_session_snapshots ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON public.api_shared_session_snapshots FROM PUBLIC, anon, authenticated;
GRANT ALL ON public.api_shared_session_snapshots TO service_role;
CREATE POLICY api_shared_session_snapshots_service_all
  ON public.api_shared_session_snapshots
  FOR ALL TO service_role USING (true) WITH CHECK (true);

CREATE FUNCTION public.set_cloud_api_snapshot_workspace(
  p_actor_user_id uuid,
  p_session_id text,
  p_workspace_id uuid
)
RETURNS TABLE (share_id uuid)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = ''
AS $$
DECLARE
  v_event_key text;
  v_canonical_key text;
BEGIN
  SELECT snapshot.content_json ->> 'calendar_event_key'
  INTO v_event_key
  FROM public.api_session_snapshots AS snapshot
    WHERE snapshot.user_id = p_actor_user_id
      AND snapshot.session_id = p_session_id;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'cloud snapshot not found' USING ERRCODE = 'P0002';
  END IF;
  SELECT share.calendar_event_key INTO v_canonical_key
  FROM public.api_shared_session_snapshots AS share
  WHERE share.owner_user_id = p_actor_user_id
    AND share.session_id = p_session_id;
  IF v_canonical_key IS NULL THEN
    IF v_event_key IS NULL OR v_event_key <> btrim(v_event_key)
      OR octet_length(v_event_key) NOT BETWEEN 1 AND 512 THEN
      RAISE EXCEPTION 'calendar event key required' USING ERRCODE = '22023';
    END IF;
    v_canonical_key := v_event_key;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM public.workspaces AS workspace
    JOIN public.workspace_memberships AS membership
      ON membership.workspace_id = workspace.id
    WHERE workspace.id = p_workspace_id
      AND workspace.kind = 'shared'
      AND workspace.deleted_at IS NULL
      AND membership.user_id = p_actor_user_id
      AND membership.deleted_at IS NULL
  ) THEN
    RAISE EXCEPTION 'workspace access denied' USING ERRCODE = '42501';
  END IF;
  RETURN QUERY
  INSERT INTO public.api_shared_session_snapshots
    (owner_user_id, session_id, workspace_id, calendar_event_key)
  VALUES (p_actor_user_id, p_session_id, p_workspace_id, v_canonical_key)
  ON CONFLICT (owner_user_id, session_id)
  DO UPDATE SET workspace_id = excluded.workspace_id
  RETURNING id;
EXCEPTION WHEN unique_violation THEN
  RAISE EXCEPTION 'calendar event already shared' USING ERRCODE = '23505';
END;
$$;

CREATE FUNCTION public.delete_cloud_api_snapshot_workspace(
  p_actor_user_id uuid,
  p_session_id text
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = ''
AS $$
BEGIN
  DELETE FROM public.api_shared_session_snapshots
  WHERE owner_user_id = p_actor_user_id AND session_id = p_session_id;
  RETURN FOUND;
END;
$$;

CREATE FUNCTION public.list_shared_cloud_api_snapshots(
  p_actor_user_id uuid,
  p_limit integer DEFAULT 21,
  p_offset integer DEFAULT 0
)
RETURNS TABLE (
  share_id uuid,
  owner_user_id uuid,
  workspace_id uuid,
  content_json jsonb
)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
AS $$
  SELECT share.id, share.owner_user_id, share.workspace_id,
    jsonb_set(snapshot.content_json, '{calendar_event_key}',
      to_jsonb(share.calendar_event_key))
  FROM public.api_shared_session_snapshots AS share
  JOIN public.api_session_snapshots AS snapshot
    ON snapshot.user_id = share.owner_user_id
   AND snapshot.session_id = share.session_id
  JOIN public.workspaces AS workspace
    ON workspace.id = share.workspace_id
   AND workspace.kind = 'shared'
   AND workspace.deleted_at IS NULL
  JOIN public.workspace_memberships AS membership
    ON membership.workspace_id = share.workspace_id
   AND membership.user_id = p_actor_user_id
   AND membership.deleted_at IS NULL
  ORDER BY snapshot.published_at DESC, share.id DESC
  LIMIT greatest(1, least(p_limit, 201))
  OFFSET greatest(p_offset, 0);
$$;

CREATE FUNCTION public.read_shared_cloud_api_snapshot(
  p_actor_user_id uuid,
  p_share_id uuid
)
RETURNS TABLE (content_json jsonb)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
AS $$
  SELECT jsonb_set(snapshot.content_json, '{calendar_event_key}',
    to_jsonb(share.calendar_event_key))
  FROM public.api_shared_session_snapshots AS share
  JOIN public.api_session_snapshots AS snapshot
    ON snapshot.user_id = share.owner_user_id
   AND snapshot.session_id = share.session_id
  JOIN public.workspaces AS workspace
    ON workspace.id = share.workspace_id
   AND workspace.kind = 'shared'
   AND workspace.deleted_at IS NULL
  JOIN public.workspace_memberships AS membership
    ON membership.workspace_id = share.workspace_id
   AND membership.user_id = p_actor_user_id
   AND membership.deleted_at IS NULL
  WHERE share.id = p_share_id;
$$;

REVOKE ALL ON FUNCTION
  public.set_cloud_api_snapshot_workspace(uuid, text, uuid),
  public.delete_cloud_api_snapshot_workspace(uuid, text),
  public.list_shared_cloud_api_snapshots(uuid, integer, integer),
  public.read_shared_cloud_api_snapshot(uuid, uuid)
FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION
  public.set_cloud_api_snapshot_workspace(uuid, text, uuid),
  public.delete_cloud_api_snapshot_workspace(uuid, text),
  public.list_shared_cloud_api_snapshots(uuid, integer, integer),
  public.read_shared_cloud_api_snapshot(uuid, uuid)
TO service_role;
