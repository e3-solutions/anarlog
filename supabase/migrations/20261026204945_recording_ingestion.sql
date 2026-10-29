CREATE TABLE public.recording_occurrences (
  workspace uuid NOT NULL REFERENCES public.workspaces(id) ON DELETE CASCADE,
  id text NOT NULL,
  provider_key text NOT NULL,
  metadata jsonb NOT NULL,
  deleted boolean NOT NULL DEFAULT false,
  sealed boolean NOT NULL DEFAULT false,
  automatic boolean NOT NULL DEFAULT true,
  provider_started_at_ms bigint,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (workspace, id),
  UNIQUE (workspace, provider_key)
);

CREATE TABLE public.recording_captures (
  workspace uuid NOT NULL,
  occurrence text NOT NULL,
  id text NOT NULL,
  identity text NOT NULL,
  metadata jsonb NOT NULL,
  object text NOT NULL UNIQUE,
  deleted boolean NOT NULL DEFAULT false,
  frames bigint NOT NULL CHECK (frames > 0 AND frames <= 230400000),
  size_bytes bigint NOT NULL CHECK (size_bytes > 0),
  transcript_json jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (workspace, occurrence, id),
  UNIQUE (workspace, identity),
  FOREIGN KEY (workspace, occurrence) REFERENCES public.recording_occurrences(workspace, id) ON DELETE CASCADE
);

CREATE TABLE public.recording_admissions (
  workspace uuid NOT NULL,
  occurrence text NOT NULL,
  actor uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  PRIMARY KEY (workspace, occurrence, actor),
  FOREIGN KEY (workspace, occurrence) REFERENCES public.recording_occurrences(workspace, id) ON DELETE CASCADE
);

ALTER TABLE public.recording_occurrences ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.recording_captures ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.recording_admissions ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON public.recording_occurrences, public.recording_captures, public.recording_admissions FROM PUBLIC, anon, authenticated;
GRANT SELECT ON public.recording_occurrences, public.recording_captures TO authenticated;
GRANT ALL ON public.recording_occurrences, public.recording_captures, public.recording_admissions TO service_role;

CREATE POLICY recording_occurrences_read_member ON public.recording_occurrences
  FOR SELECT TO authenticated USING (
    NOT deleted AND workspace IN (SELECT id FROM public.workspaces WHERE deleted_at IS NULL)
  );
CREATE POLICY recording_captures_read_member ON public.recording_captures
  FOR SELECT TO authenticated USING (
    NOT deleted AND EXISTS (
      SELECT 1 FROM public.recording_occurrences o
      WHERE o.workspace = recording_captures.workspace AND o.id = recording_captures.occurrence
    )
  );

INSERT INTO storage.buckets (id, name, public, file_size_limit, allowed_mime_types)
VALUES ('recording-audio', 'recording-audio', false, 104857600, ARRAY['audio/wav'])
ON CONFLICT (id) DO UPDATE SET public = false, file_size_limit = EXCLUDED.file_size_limit,
  allowed_mime_types = EXCLUDED.allowed_mime_types;
CREATE POLICY recording_audio_read_member ON storage.objects
  FOR SELECT TO authenticated USING (
    bucket_id = 'recording-audio' AND EXISTS (
      SELECT 1 FROM public.recording_captures c WHERE c.object = storage.objects.name
    )
  );

CREATE FUNCTION public.recording_admit(
  p_workspace uuid, p_id text, p_actor uuid, p_provider_key text,
  p_metadata jsonb, p_provider_started_at_ms bigint DEFAULT NULL
) RETURNS bigint LANGUAGE plpgsql SECURITY INVOKER SET search_path = '' AS $$
DECLARE
  existing public.recording_occurrences;
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.workspace_memberships m JOIN public.workspaces w ON w.id = m.workspace_id
    WHERE m.workspace_id = p_workspace AND m.user_id = p_actor
      AND m.deleted_at IS NULL AND w.deleted_at IS NULL
  ) THEN
    RAISE EXCEPTION 'workspace membership required' USING ERRCODE = '42501';
  END IF;
  INSERT INTO public.recording_occurrences(workspace, id, provider_key, metadata, provider_started_at_ms)
  VALUES (p_workspace, p_id, p_provider_key, p_metadata,
    COALESCE(p_provider_started_at_ms, (extract(epoch FROM now()) * 1000)::bigint))
  ON CONFLICT (workspace, id) DO NOTHING;
  SELECT * INTO STRICT existing FROM public.recording_occurrences
  WHERE workspace = p_workspace AND id = p_id FOR UPDATE;
  IF existing.deleted OR existing.provider_key <> p_provider_key THEN
    RAISE EXCEPTION 'recording identity conflict' USING ERRCODE = '23505';
  END IF;
  INSERT INTO public.recording_admissions VALUES (p_workspace, p_id, p_actor) ON CONFLICT DO NOTHING;
  RETURN existing.provider_started_at_ms;
END;
$$;

CREATE FUNCTION public.recording_upload(
  p_workspace uuid, p_occurrence text, p_id text, p_identity text,
  p_metadata jsonb, p_object text, p_frames bigint, p_size_bytes bigint
) RETURNS void LANGUAGE plpgsql SECURITY INVOKER SET search_path = '' AS $$
DECLARE
  occurrence_row public.recording_occurrences;
  capture_row public.recording_captures;
  end_frame bigint;
BEGIN
  SELECT * INTO STRICT occurrence_row FROM public.recording_occurrences
  WHERE workspace = p_workspace AND id = p_occurrence AND NOT deleted FOR UPDATE;
  SELECT * INTO capture_row FROM public.recording_captures
  WHERE workspace = p_workspace AND occurrence = p_occurrence AND id = p_id;
  IF FOUND THEN
    IF capture_row.deleted OR capture_row.metadata <> p_metadata OR capture_row.identity <> p_identity
      OR capture_row.object <> p_object OR capture_row.frames <> p_frames OR capture_row.size_bytes <> p_size_bytes THEN
      RAISE EXCEPTION 'recording identity conflict' USING ERRCODE = '23505';
    END IF;
    RETURN;
  END IF;
  IF occurrence_row.sealed THEN
    RAISE EXCEPTION 'recording is sealed' USING ERRCODE = '23505';
  END IF;
  IF (SELECT count(*) >= 32 OR COALESCE(sum(frames), 0) + p_frames > 460800000
      FROM public.recording_captures WHERE workspace = p_workspace AND occurrence = p_occurrence AND NOT deleted) THEN
    RAISE EXCEPTION 'occurrence source limit exceeded' USING ERRCODE = '22023';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM public.recording_admissions
    WHERE workspace = p_workspace AND occurrence = p_occurrence AND actor::text = p_metadata->>'uploaderId') THEN
    RAISE EXCEPTION 'recording admission required' USING ERRCODE = '42501';
  END IF;
  end_frame := greatest(1, (p_metadata->>'offsetFrames')::bigint + p_frames);
  IF end_frame > 230400000 THEN
    RAISE EXCEPTION 'occurrence timeline limit exceeded' USING ERRCODE = '22023';
  END IF;
  INSERT INTO public.recording_captures(workspace, occurrence, id, identity, metadata, object, frames, size_bytes)
  VALUES (p_workspace, p_occurrence, p_id, p_identity, p_metadata, p_object, p_frames, p_size_bytes);
  IF occurrence_row.automatic THEN
    UPDATE public.recording_occurrences SET metadata = jsonb_set(metadata, '{durationFrames}',
      to_jsonb(greatest((metadata->>'durationFrames')::bigint, end_frame)))
    WHERE workspace = p_workspace AND id = p_occurrence;
  END IF;
END;
$$;

CREATE FUNCTION public.recording_save_transcript(
  p_workspace uuid, p_occurrence text, p_id text, p_expected_sha text, p_transcript jsonb
) RETURNS void LANGUAGE plpgsql SECURITY INVOKER SET search_path = '' AS $$
DECLARE
  capture_row public.recording_captures;
BEGIN
  PERFORM 1 FROM public.recording_occurrences
  WHERE workspace = p_workspace AND id = p_occurrence AND NOT deleted FOR UPDATE;
  IF NOT FOUND THEN RAISE NO_DATA_FOUND; END IF;
  SELECT * INTO STRICT capture_row FROM public.recording_captures
  WHERE workspace = p_workspace AND occurrence = p_occurrence AND id = p_id AND NOT deleted FOR UPDATE;
  IF capture_row.metadata->>'sha256' IS DISTINCT FROM p_expected_sha
    OR (capture_row.transcript_json IS NOT NULL AND capture_row.transcript_json <> p_transcript) THEN
    RAISE EXCEPTION 'recording identity conflict' USING ERRCODE = '23505';
  END IF;
  UPDATE public.recording_captures SET transcript_json = p_transcript
  WHERE workspace = p_workspace AND occurrence = p_occurrence AND id = p_id;
END;
$$;

REVOKE ALL ON FUNCTION public.recording_admit(uuid, text, uuid, text, jsonb, bigint),
  public.recording_upload(uuid, text, text, text, jsonb, text, bigint, bigint),
  public.recording_save_transcript(uuid, text, text, text, jsonb) FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.recording_admit(uuid, text, uuid, text, jsonb, bigint),
  public.recording_upload(uuid, text, text, text, jsonb, text, bigint, bigint),
  public.recording_save_transcript(uuid, text, text, text, jsonb) TO service_role;

CREATE FUNCTION public.recording_seal(p_workspace uuid, p_occurrence text)
RETURNS void LANGUAGE plpgsql SECURITY INVOKER SET search_path = '' AS $$
BEGIN
  PERFORM 1 FROM public.recording_occurrences
  WHERE workspace = p_workspace AND id = p_occurrence AND NOT deleted FOR UPDATE;
  IF NOT FOUND THEN RAISE NO_DATA_FOUND; END IF;
  IF NOT EXISTS (SELECT 1 FROM public.recording_captures
    WHERE workspace = p_workspace AND occurrence = p_occurrence AND NOT deleted) THEN
    RAISE EXCEPTION 'recording has no sources' USING ERRCODE = '23505';
  END IF;
  UPDATE public.recording_occurrences SET sealed = true WHERE workspace = p_workspace AND id = p_occurrence;
END;
$$;

CREATE FUNCTION public.recording_delete(p_workspace uuid, p_occurrence text, p_capture text DEFAULT NULL)
RETURNS SETOF text LANGUAGE plpgsql SECURITY INVOKER SET search_path = '' AS $$
BEGIN
  PERFORM 1 FROM public.recording_occurrences WHERE workspace = p_workspace AND id = p_occurrence FOR UPDATE;
  IF NOT FOUND THEN RAISE NO_DATA_FOUND; END IF;
  IF p_capture IS NOT NULL AND NOT EXISTS (SELECT 1 FROM public.recording_captures
    WHERE workspace = p_workspace AND occurrence = p_occurrence AND id = p_capture) THEN
    RAISE NO_DATA_FOUND;
  END IF;
  IF p_capture IS NULL THEN
    UPDATE public.recording_occurrences SET deleted = true WHERE workspace = p_workspace AND id = p_occurrence;
  END IF;
  UPDATE public.recording_captures SET deleted = true, transcript_json = NULL
  WHERE workspace = p_workspace AND occurrence = p_occurrence AND (p_capture IS NULL OR id = p_capture);
  RETURN QUERY SELECT object FROM public.recording_captures
  WHERE workspace = p_workspace AND occurrence = p_occurrence AND (p_capture IS NULL OR id = p_capture);
END;
$$;

REVOKE ALL ON FUNCTION public.recording_seal(uuid, text), public.recording_delete(uuid, text, text)
FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.recording_seal(uuid, text), public.recording_delete(uuid, text, text) TO service_role;
