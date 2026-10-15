-- A calendar occurrence has one stable share ID, but every recorder keeps a private
-- candidate. The selected snapshot can change without changing links to the share.
-- The preceding sharing migration is part of the same E3 rollout. If it was
-- deployed earlier and has data, its attendee-specific keys cannot be mapped
-- to Google occurrence IDs without the clients. Stop instead of duplicating
-- groups or invalidating old links; migrate that workspace explicitly first.
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM public.api_shared_session_snapshots) THEN
    RAISE EXCEPTION 'legacy shared snapshots require occurrence migration before candidate rollout';
  END IF;
END;
$$;
ALTER TABLE public.api_shared_session_snapshots
  DROP CONSTRAINT IF EXISTS api_shared_session_snapshots_owner_user_id_session_id_fkey;
ALTER TABLE public.api_shared_session_snapshots
  ALTER COLUMN owner_user_id DROP NOT NULL,
  ALTER COLUMN session_id DROP NOT NULL;
ALTER TABLE public.api_shared_session_snapshots
  ADD CONSTRAINT api_shared_session_selected_pair_check
  CHECK ((owner_user_id IS NULL) = (session_id IS NULL));

CREATE TABLE public.api_shared_session_candidates (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  share_id uuid NOT NULL REFERENCES public.api_shared_session_snapshots(id) ON DELETE CASCADE,
  owner_user_id uuid NOT NULL,
  session_id text NOT NULL,
  verified_overlap_seconds numeric CHECK (verified_overlap_seconds >= 0),
  transcript_coverage_seconds numeric NOT NULL DEFAULT 0 CHECK (transcript_coverage_seconds >= 0),
  transcript_characters integer NOT NULL DEFAULT 0 CHECK (transcript_characters >= 0),
  snapshot_fingerprint text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (owner_user_id, session_id),
  FOREIGN KEY (owner_user_id, session_id)
    REFERENCES public.api_session_snapshots(user_id, session_id) ON DELETE CASCADE
);
CREATE INDEX api_shared_session_candidates_share_idx
  ON public.api_shared_session_candidates (share_id);
ALTER TABLE public.api_shared_session_candidates ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON public.api_shared_session_candidates FROM PUBLIC, anon, authenticated;
GRANT ALL ON public.api_shared_session_candidates TO service_role;
CREATE POLICY api_shared_session_candidates_service_all
  ON public.api_shared_session_candidates
  FOR ALL TO service_role USING (true) WITH CHECK (true);

-- The ordering is deterministic, and an exact quality tie keeps the current owner.
-- Membership is checked at read time, so revocation and snapshot deletion fail over
-- immediately even if no recorder uploads again.
CREATE FUNCTION public.choose_shared_cloud_api_candidate(p_share_id uuid)
RETURNS TABLE (owner_user_id uuid, session_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
AS $$
  SELECT candidate.owner_user_id, candidate.session_id
  FROM public.api_shared_session_snapshots AS share
  JOIN public.api_shared_session_candidates AS candidate
    ON candidate.share_id = share.id
  JOIN public.api_session_snapshots AS snapshot
    ON snapshot.user_id = candidate.owner_user_id
   AND snapshot.session_id = candidate.session_id
  JOIN public.workspace_memberships AS owner_membership
    ON owner_membership.workspace_id = share.workspace_id
   AND owner_membership.user_id = candidate.owner_user_id
   AND owner_membership.deleted_at IS NULL
  WHERE share.id = p_share_id AND candidate.transcript_characters > 0
    AND EXISTS (
      SELECT 1 FROM jsonb_array_elements(
        CASE WHEN jsonb_typeof(snapshot.content_json -> 'transcripts') = 'array'
          THEN snapshot.content_json -> 'transcripts' ELSE '[]'::jsonb END
      ) AS segment WHERE length(segment ->> 'text') > 0
    )
  -- Speech timing is a client hint, capped by the text actually stored. A
  -- one-word recording cannot win by claiming hours of word coverage.
  ORDER BY least(candidate.transcript_coverage_seconds,
      candidate.transcript_characters::numeric / 5) DESC,
    nullif(candidate.verified_overlap_seconds, 0) DESC NULLS LAST,
    candidate.transcript_characters DESC,
    (candidate.owner_user_id = share.owner_user_id
      AND candidate.session_id = share.session_id) DESC,
    candidate.owner_user_id, candidate.session_id
  LIMIT 1;
$$;
REVOKE ALL ON FUNCTION public.choose_shared_cloud_api_candidate(uuid)
  FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.choose_shared_cloud_api_candidate(uuid)
  TO service_role;

DROP FUNCTION public.set_cloud_api_snapshot_workspace(uuid, text, uuid);
CREATE FUNCTION public.set_cloud_api_snapshot_workspace(
  p_actor_user_id uuid,
  p_session_id text,
  p_workspace_id uuid,
  p_verified_overlap_seconds numeric DEFAULT NULL,
  p_conference_record text DEFAULT NULL
)
RETURNS TABLE (share_id uuid)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = ''
AS $$
DECLARE
  v_content jsonb;
  v_choice jsonb;
  v_event_key text;
  v_existing_share_id uuid;
  v_share_id uuid;
  v_overlap_seconds numeric;
  v_coverage_seconds numeric := 0;
  v_characters integer := 0;
  v_snapshot_fingerprint text;
  v_actual_characters bigint;
  v_owner uuid;
  v_session text;
BEGIN
  SELECT snapshot.content_json INTO v_content
  FROM public.api_session_snapshots AS snapshot
  WHERE snapshot.user_id = p_actor_user_id
    AND snapshot.session_id = p_session_id;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'cloud snapshot not found' USING ERRCODE = 'P0002';
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

  SELECT candidate.share_id INTO v_existing_share_id
  FROM public.api_shared_session_candidates AS candidate
  WHERE candidate.owner_user_id = p_actor_user_id
    AND candidate.session_id = p_session_id;
  v_event_key := v_content ->> 'calendar_event_key';
  IF v_event_key IS NULL OR v_event_key NOT LIKE 'google:%'
    OR v_event_key <> btrim(v_event_key)
    OR octet_length(v_event_key) NOT BETWEEN 1 AND 512 THEN
    RAISE EXCEPTION 'calendar event key required' USING ERRCODE = '22023';
  END IF;

  v_choice := v_content -> 'shared_transcript_choice';
  SELECT coalesce(sum(length(segment ->> 'text')), 0) INTO v_actual_characters
  FROM jsonb_array_elements(
    CASE WHEN jsonb_typeof(v_content -> 'transcripts') = 'array'
      THEN v_content -> 'transcripts' ELSE '[]'::jsonb END
  ) AS segment;
  IF v_actual_characters <= 0 THEN
    RAISE EXCEPTION 'nonempty transcript required' USING ERRCODE = '22023';
  END IF;
  -- Count the text we actually store, rather than a client-claimed size.
  v_characters := least(v_actual_characters, 100000000)::integer;
  v_snapshot_fingerprint := md5(
    coalesce(v_content -> 'transcripts', '[]'::jsonb)::text ||
    coalesce(p_conference_record, '')
  );
  IF v_characters <= 0 THEN
    RAISE EXCEPTION 'nonempty transcript required' USING ERRCODE = '22023';
  END IF;
  -- Client coverage is a bounded transcript-quality hint. It does not prove
  -- Meet attendance; only the separate Cloud API argument can do that.
  IF jsonb_typeof(v_choice -> 'transcript_coverage_seconds') = 'number'
    AND (v_choice ->> 'transcript_coverage_seconds')::numeric BETWEEN 0 AND 604800 THEN
    v_coverage_seconds := (v_choice ->> 'transcript_coverage_seconds')::numeric;
  END IF;
  -- Only the Cloud API may supply the independently verified overlap.
  -- Zero is a completed measurement with no overlap; NULL is unavailable.
  IF p_verified_overlap_seconds >= 0
    AND p_verified_overlap_seconds <= 604800 THEN
    v_overlap_seconds := p_verified_overlap_seconds;
  END IF;

  -- Serialize registrations for the same workspace and canonical occurrence.
  PERFORM pg_catalog.pg_advisory_xact_lock(
    pg_catalog.hashtextextended(p_workspace_id::text || ':' || v_event_key, 0)
  );
  INSERT INTO public.api_shared_session_snapshots
    (owner_user_id, session_id, workspace_id, calendar_event_key)
  VALUES (NULL, NULL, p_workspace_id, v_event_key)
  ON CONFLICT (workspace_id, calendar_event_key)
  DO UPDATE SET calendar_event_key = excluded.calendar_event_key
  RETURNING id INTO v_share_id;

  -- The old group may still point at this candidate during a workspace move.
  IF v_existing_share_id IS NOT NULL AND v_existing_share_id <> v_share_id THEN
    UPDATE public.api_shared_session_snapshots
    SET owner_user_id = NULL, session_id = NULL
    WHERE id = v_existing_share_id
      AND owner_user_id = p_actor_user_id AND session_id = p_session_id;
  END IF;

  INSERT INTO public.api_shared_session_candidates
    (share_id, owner_user_id, session_id, verified_overlap_seconds,
     transcript_coverage_seconds, transcript_characters, snapshot_fingerprint)
  VALUES (v_share_id, p_actor_user_id, p_session_id, v_overlap_seconds,
    v_coverage_seconds, v_characters, v_snapshot_fingerprint)
  ON CONFLICT (owner_user_id, session_id) DO UPDATE SET
    share_id = excluded.share_id,
    verified_overlap_seconds = CASE
      WHEN excluded.verified_overlap_seconds IS NOT NULL
        THEN excluded.verified_overlap_seconds
      WHEN public.api_shared_session_candidates.snapshot_fingerprint = excluded.snapshot_fingerprint
        THEN public.api_shared_session_candidates.verified_overlap_seconds
      ELSE NULL END,
    transcript_coverage_seconds = excluded.transcript_coverage_seconds,
    transcript_characters = excluded.transcript_characters,
    snapshot_fingerprint = excluded.snapshot_fingerprint;

  SELECT chosen.owner_user_id, chosen.session_id INTO v_owner, v_session
  FROM public.choose_shared_cloud_api_candidate(v_share_id) AS chosen;
  UPDATE public.api_shared_session_snapshots
  SET owner_user_id = v_owner, session_id = v_session
  WHERE id = v_share_id;
  IF v_existing_share_id IS NOT NULL AND v_existing_share_id <> v_share_id THEN
    DELETE FROM public.api_shared_session_snapshots AS share
    WHERE share.id = v_existing_share_id
      AND NOT EXISTS (
        SELECT 1 FROM public.api_shared_session_candidates AS candidate
        WHERE candidate.share_id = share.id
      );
  END IF;
  RETURN QUERY SELECT v_share_id;
END;
$$;

CREATE OR REPLACE FUNCTION public.delete_cloud_api_snapshot_workspace(
  p_actor_user_id uuid,
  p_session_id text
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = ''
AS $$
DECLARE
  v_share_id uuid;
  v_workspace_id uuid;
  v_event_key text;
  v_owner uuid;
  v_session text;
BEGIN
  SELECT share.id, share.workspace_id, share.calendar_event_key
    INTO v_share_id, v_workspace_id, v_event_key
  FROM public.api_shared_session_candidates AS candidate
  JOIN public.api_shared_session_snapshots AS share ON share.id = candidate.share_id
  WHERE candidate.owner_user_id = p_actor_user_id AND candidate.session_id = p_session_id;
  IF v_share_id IS NULL THEN RETURN false; END IF;
  PERFORM pg_catalog.pg_advisory_xact_lock(
    pg_catalog.hashtextextended(v_workspace_id::text || ':' || v_event_key, 0)
  );
  PERFORM 1 FROM public.api_shared_session_snapshots
    WHERE id = v_share_id FOR UPDATE;
  DELETE FROM public.api_shared_session_candidates
  WHERE owner_user_id = p_actor_user_id AND session_id = p_session_id
  RETURNING share_id INTO v_share_id;
  IF v_share_id IS NULL THEN RETURN false; END IF;
  SELECT chosen.owner_user_id, chosen.session_id INTO v_owner, v_session
  FROM public.choose_shared_cloud_api_candidate(v_share_id) AS chosen;
  IF NOT EXISTS (
    SELECT 1 FROM public.api_shared_session_candidates
    WHERE share_id = v_share_id
  ) THEN
    DELETE FROM public.api_shared_session_snapshots WHERE id = v_share_id;
  ELSE
    UPDATE public.api_shared_session_snapshots
    SET owner_user_id = v_owner, session_id = v_session
    WHERE id = v_share_id;
  END IF;
  RETURN true;
END;
$$;

CREATE OR REPLACE FUNCTION public.list_shared_cloud_api_snapshots(
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
  SELECT share.id, chosen.owner_user_id, share.workspace_id,
    jsonb_set(snapshot.content_json - 'shared_transcript_choice',
      '{calendar_event_key}', to_jsonb(share.calendar_event_key))
  FROM public.api_shared_session_snapshots AS share
  JOIN LATERAL public.choose_shared_cloud_api_candidate(share.id) AS chosen ON true
  JOIN public.api_session_snapshots AS snapshot
    ON snapshot.user_id = chosen.owner_user_id
   AND snapshot.session_id = chosen.session_id
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

CREATE OR REPLACE FUNCTION public.read_shared_cloud_api_snapshot(
  p_actor_user_id uuid,
  p_share_id uuid
)
RETURNS TABLE (content_json jsonb)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
AS $$
  SELECT jsonb_set(snapshot.content_json - 'shared_transcript_choice',
    '{calendar_event_key}', to_jsonb(share.calendar_event_key))
  FROM public.api_shared_session_snapshots AS share
  JOIN LATERAL public.choose_shared_cloud_api_candidate(share.id) AS chosen ON true
  JOIN public.api_session_snapshots AS snapshot
    ON snapshot.user_id = chosen.owner_user_id
   AND snapshot.session_id = chosen.session_id
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

REVOKE ALL ON FUNCTION public.set_cloud_api_snapshot_workspace(uuid, text, uuid, numeric, text)
  FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.set_cloud_api_snapshot_workspace(uuid, text, uuid, numeric, text)
  TO service_role;
